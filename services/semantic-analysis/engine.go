// Package analysis drives the usie-engine process: parsing source files into
// IR and resolving IR into graph records.
//
// The engine is a separate, short-lived child process. It receives no
// environment, no credentials and no network configuration; it is given
// only a directory to read. Repository content is never executed.
package analysis

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os/exec"
	"reflect"
	"strings"
	"sync"
	"time"
)

type Engine struct {
	Bin string
	// ParseTimeout is the per-file parse budget passed to the engine.
	ParseTimeout time.Duration
	MaxFileBytes int64
	Threads      int
}

type LanguageInfo struct {
	Language    string `json:"language"`
	Fingerprint string `json:"fingerprint"`
}

type Info struct {
	Engine          string         `json:"engine"`
	Version         string         `json:"version"`
	IRSchemaVersion int            `json:"ir_schema_version"`
	Languages       []LanguageInfo `json:"languages"`
}

// Fingerprint returns the parser fingerprint for a language ("" if unsupported).
func (i Info) Fingerprint(language string) string {
	for _, l := range i.Languages {
		if l.Language == language {
			return l.Fingerprint
		}
	}
	return ""
}

// maxLine bounds a single protocol line from the engine. IR for a file at
// the size limit is a few times the source size; anything near this bound
// indicates a defect, and the stream is abandoned rather than buffered.
const maxLine = 256 << 20

func (e Engine) command(ctx context.Context, args ...string) (*exec.Cmd, *limitedBuffer) {
	cmd := exec.CommandContext(ctx, e.Bin, args...)
	cmd.Env = []string{} // no inherited environment
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stderr = stderr
	cmd.WaitDelay = 10 * time.Second
	return cmd, stderr
}

func engineError(err error, stderr *limitedBuffer, ctx context.Context) error {
	if ctx.Err() != nil {
		return ctx.Err()
	}
	// The engine reports failures as one JSON line on stderr.
	var report struct {
		Code    string `json:"code"`
		Message string `json:"message"`
	}
	text := strings.TrimSpace(stderr.String())
	if last := text[strings.LastIndex(text, "\n")+1:]; json.Unmarshal([]byte(last), &report) == nil && report.Message != "" {
		return fmt.Errorf("engine: %s: %s", report.Code, report.Message)
	}
	if len(text) > 500 {
		text = text[:500]
	}
	return fmt.Errorf("engine: %w: %s", err, text)
}

func (e Engine) Info(ctx context.Context) (Info, error) {
	var info Info
	cmd, stderr := e.command(ctx, "info")
	out, err := cmd.Output()
	if err != nil {
		return info, engineError(err, stderr, ctx)
	}
	if err := json.Unmarshal(out, &info); err != nil {
		return info, fmt.Errorf("engine: invalid info output: %w", err)
	}
	if info.IRSchemaVersion == 0 || len(info.Languages) == 0 {
		return info, errors.New("engine: incomplete info output")
	}
	return info, nil
}

type ParseRequest struct {
	ID       string `json:"id"`
	Path     string `json:"path"`
	Language string `json:"language"`
	// Source is the file to read, relative to the parse root.
	Source string `json:"source,omitempty"`
}

type ParseResult struct {
	ID      string          `json:"id"`
	Path    string          `json:"path"`
	Status  string          `json:"status"` // ok | error
	Code    string          `json:"code"`
	Message string          `json:"message"`
	IR      json.RawMessage `json:"ir"`
}

// IRSummary is the part of the IR the services inspect. The IR is otherwise
// treated as opaque and passed back to the engine unchanged.
type IRSummary struct {
	SchemaVersion int               `json:"schema_version"`
	Symbols       []json.RawMessage `json:"symbols"`
	Stats         struct {
		DiagnosticsTotal int `json:"diagnostics_total"`
	} `json:"stats"`
}

// Parse runs one engine process over the requests and calls onResult once
// per request, in request order. Per-file failures are results, not errors.
func (e Engine) Parse(ctx context.Context, root string, requests []ParseRequest, onResult func(ParseResult) error) error {
	if len(requests) == 0 {
		return nil
	}
	args := []string{"parse", "--root", root}
	if e.ParseTimeout > 0 {
		args = append(args, "--timeout-ms", fmt.Sprint(e.ParseTimeout.Milliseconds()))
	}
	if e.MaxFileBytes > 0 {
		args = append(args, "--max-file-bytes", fmt.Sprint(e.MaxFileBytes))
	}
	if e.Threads > 0 {
		args = append(args, "--threads", fmt.Sprint(e.Threads))
	}
	received := 0
	err := e.stream(ctx, args, func(w *bufio.Writer) error {
		enc := json.NewEncoder(w)
		for _, r := range requests {
			if err := enc.Encode(r); err != nil {
				return err
			}
		}
		return nil
	}, func(line []byte) error {
		var res ParseResult
		if err := json.Unmarshal(line, &res); err != nil {
			return fmt.Errorf("engine: invalid parse result: %w", err)
		}
		if received >= len(requests) || res.ID != requests[received].ID {
			return fmt.Errorf("engine: parse results out of order")
		}
		received++
		return onResult(res)
	})
	if err != nil {
		return err
	}
	if received != len(requests) {
		return fmt.Errorf("engine: %d of %d parse results received", received, len(requests))
	}
	return nil
}

// ResolveConfig mirrors the engine's ProjectConfig.
type ResolveConfig struct {
	Paths        []string             `json:"paths"`
	Dependencies []DeclaredDependency `json:"dependencies"`
	TsConfigs    any                  `json:"tsconfigs"`
	Packages     any                  `json:"packages"`
	PythonRoots  []string             `json:"python_roots"`
}

type DeclaredDependency struct {
	Ecosystem string `json:"ecosystem"`
	Name      string `json:"name"`
}

type Span struct {
	StartLine int32 `json:"start_line"`
	EndLine   int32 `json:"end_line"`
}

type NodeRecord struct {
	Key           string         `json:"key"`
	Kind          string         `json:"kind"`
	Name          string         `json:"name"`
	QualifiedName string         `json:"qualified_name"`
	Path          string         `json:"path"`
	Span          *Span          `json:"span"`
	Signature     string         `json:"signature"`
	Doc           string         `json:"doc"`
	Exported      bool           `json:"exported"`
	Attrs         map[string]any `json:"attrs"`
}

type EdgeRecord struct {
	Src        string `json:"src"`
	Dst        string `json:"dst"`
	Kind       string `json:"kind"`
	Confidence string `json:"confidence"`
	Rule       string `json:"rule"`
	Evidence   *struct {
		Path string `json:"path"`
		Line int32  `json:"line"`
	} `json:"evidence"`
	Count int32 `json:"count"`
}

type ReferenceRecord struct {
	Path       string `json:"path"`
	Line       int32  `json:"line"`
	Col        int32  `json:"col"`
	Kind       string `json:"kind"`
	Text       string `json:"text"`
	Source     string `json:"source"`
	Outcome    string `json:"outcome"`
	Confidence string `json:"confidence"`
	Target     string `json:"target"`
	Rule       string `json:"rule"`
	Reason     string `json:"reason"`
}

type ResolveStats struct {
	Files               int64            `json:"files"`
	Symbols             int64            `json:"symbols"`
	ExternalPackages    int64            `json:"external_packages"`
	Edges               int64            `json:"edges"`
	References          int64            `json:"references"`
	ReferencesByOutcome map[string]int64 `json:"references_by_outcome"`
}

type Resolution struct {
	Nodes      []NodeRecord
	Edges      []EdgeRecord
	References []ReferenceRecord
	Stats      ResolveStats
}

// ResolveFile is one input to Resolve. Open returns the stored IR JSON.
type ResolveFile struct {
	Path string
	Open func() ([]byte, error)
}

// Resolve streams a project to the engine and collects the graph records.
// Output is accepted only if the terminating stats record arrives.
func (e Engine) Resolve(ctx context.Context, cfg ResolveConfig, files []ResolveFile) (*Resolution, error) {
	// The engine expects arrays, never null.
	if cfg.Paths == nil {
		cfg.Paths = []string{}
	}
	if cfg.Dependencies == nil {
		cfg.Dependencies = []DeclaredDependency{}
	}
	if cfg.PythonRoots == nil {
		cfg.PythonRoots = []string{}
	}
	if isNil(cfg.TsConfigs) {
		cfg.TsConfigs = []any{}
	}
	if isNil(cfg.Packages) {
		cfg.Packages = []any{}
	}
	res := &Resolution{}
	complete := false
	err := e.stream(ctx, []string{"resolve"}, func(w *bufio.Writer) error {
		head, err := json.Marshal(map[string]any{"type": "config", "config": cfg})
		if err != nil {
			return err
		}
		w.Write(head)
		w.WriteByte('\n')
		for _, f := range files {
			ir, err := f.Open()
			if err != nil {
				return fmt.Errorf("read IR for %s: %w", f.Path, err)
			}
			if !json.Valid(ir) || bytes.IndexByte(ir, '\n') >= 0 {
				return fmt.Errorf("stored IR for %s is not single-line JSON", f.Path)
			}
			path, _ := json.Marshal(f.Path)
			w.WriteString(`{"type":"file","path":`)
			w.Write(path)
			w.WriteString(`,"ir":`)
			w.Write(ir)
			if _, err := w.WriteString("}\n"); err != nil {
				return err
			}
			if err := ctx.Err(); err != nil {
				return err
			}
		}
		return nil
	}, func(line []byte) error {
		var head struct {
			Type string `json:"type"`
		}
		if err := json.Unmarshal(line, &head); err != nil {
			return fmt.Errorf("engine: invalid resolve record: %w", err)
		}
		if complete {
			return errors.New("engine: record after stats")
		}
		switch head.Type {
		case "node":
			var n NodeRecord
			if err := json.Unmarshal(line, &n); err != nil {
				return err
			}
			res.Nodes = append(res.Nodes, n)
		case "edge":
			var ed EdgeRecord
			if err := json.Unmarshal(line, &ed); err != nil {
				return err
			}
			res.Edges = append(res.Edges, ed)
		case "reference":
			var r ReferenceRecord
			if err := json.Unmarshal(line, &r); err != nil {
				return err
			}
			res.References = append(res.References, r)
		case "stats":
			complete = true
			return json.Unmarshal(line, &res.Stats)
		default:
			return fmt.Errorf("engine: unknown record type %q", head.Type)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	if !complete {
		return nil, errors.New("engine: resolve output is incomplete")
	}
	return res, nil
}

// stream runs the engine, feeding stdin from write and delivering stdout
// lines to read. Any error on either side stops the process.
func (e Engine) stream(ctx context.Context, args []string, write func(*bufio.Writer) error, read func([]byte) error) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	cmd, stderr := e.command(ctx, args...)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return err
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("engine: start %s: %w", e.Bin, err)
	}

	var wg sync.WaitGroup
	var writeErr error
	wg.Add(1)
	go func() {
		defer wg.Done()
		w := bufio.NewWriterSize(stdin, 1<<20)
		writeErr = write(w)
		if writeErr == nil {
			writeErr = w.Flush()
		}
		stdin.Close()
		if writeErr != nil {
			cancel()
		}
	}()

	var readErr error
	reader := bufio.NewReaderSize(stdout, 1<<20)
	for readErr == nil {
		line, err := readLine(reader)
		if err == io.EOF {
			break
		}
		if err != nil {
			readErr = err
			break
		}
		if len(line) > 0 {
			readErr = read(line)
		}
	}
	if readErr != nil {
		cancel()
		io.Copy(io.Discard, stdout)
	}
	wg.Wait()
	waitErr := cmd.Wait()
	switch {
	case readErr != nil:
		return readErr
	case waitErr != nil:
		return engineError(waitErr, stderr, ctx)
	case writeErr != nil:
		return writeErr
	}
	return nil
}

func isNil(v any) bool {
	if v == nil {
		return true
	}
	rv := reflect.ValueOf(v)
	return (rv.Kind() == reflect.Slice || rv.Kind() == reflect.Map || rv.Kind() == reflect.Pointer) && rv.IsNil()
}

func readLine(r *bufio.Reader) ([]byte, error) {
	var line []byte
	for {
		chunk, err := r.ReadSlice('\n')
		line = append(line, chunk...)
		if len(line) > maxLine {
			return nil, errors.New("engine: protocol line exceeds the size limit")
		}
		switch err {
		case nil:
			return bytes.TrimRight(line, "\r\n"), nil
		case bufio.ErrBufferFull:
			continue
		case io.EOF:
			if len(line) > 0 {
				return nil, errors.New("engine: output ends mid-record")
			}
			return nil, io.EOF
		default:
			return nil, err
		}
	}
}

type limitedBuffer struct {
	mu    sync.Mutex
	buf   bytes.Buffer
	limit int
}

func (b *limitedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if room := b.limit - b.buf.Len(); room > 0 {
		if len(p) > room {
			b.buf.Write(p[:room])
		} else {
			b.buf.Write(p)
		}
	}
	return len(p), nil
}

func (b *limitedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}
