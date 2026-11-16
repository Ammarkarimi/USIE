package quality

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"hash/fnv"
	"strings"
)

// Duplication finds blocks of DuplicationWindow or more identical code
// lines across the files added to it.
//
// Lines are compared after trimming and collapsing whitespace. Blank lines,
// comment lines, lines of only brackets or punctuation, and import-like
// lines are not code lines and are skipped, so the same code formatted with
// different blank lines or comments still matches.
type Duplication struct {
	maxLines int
	lines    int
	tooLarge bool
	files    []dupFile
}

type dupFile struct {
	path   string
	hashes []uint64 // one per code line
	lineNo []int32  // original 1-based line of each code line
}

// FileDuplication is the result for one file.
type FileDuplication struct {
	CodeLines       int
	DuplicatedLines int
	Blocks          []Raw
}

// DuplicationResult is the result for all files.
type DuplicationResult struct {
	Files           map[string]FileDuplication
	CodeLines       int
	DuplicatedLines int
	Blocks          int
	// Skipped is set when the repository had more code lines than the
	// detector's memory budget allows; no duplication is reported then.
	Skipped bool
}

// NewDuplication bounds the work to maxLines code lines in total.
func NewDuplication(maxLines int) *Duplication { return &Duplication{maxLines: maxLines} }

// Add records the code lines of one file.
func (d *Duplication) Add(path string, content []byte) {
	if d.tooLarge {
		return
	}
	f := dupFile{path: path}
	line := int32(0)
	for len(content) > 0 {
		line++
		var text []byte
		if i := bytes.IndexByte(content, '\n'); i >= 0 {
			text, content = content[:i], content[i+1:]
		} else {
			text, content = content, nil
		}
		norm, ok := codeLine(text)
		if !ok {
			continue
		}
		h := fnv.New64a()
		_, _ = h.Write(norm)
		f.hashes = append(f.hashes, h.Sum64())
		f.lineNo = append(f.lineNo, line)
	}
	d.lines += len(f.hashes)
	if d.lines > d.maxLines {
		d.tooLarge, d.files = true, nil
		return
	}
	d.files = append(d.files, f)
}

var importPrefixes = []string{"import ", "from ", "package ", "using ", "#include", "use ", "require(", "extern crate", "@import"}

// codeLine returns the normalized line, or false for a line that is not code.
func codeLine(text []byte) ([]byte, bool) {
	t := bytes.TrimSpace(text)
	if len(t) == 0 {
		return nil, false
	}
	s := string(t)
	for _, c := range []string{"//", "#", "/*", "*", "--", "<!--"} {
		if strings.HasPrefix(s, c) && !strings.HasPrefix(s, "#include") {
			return nil, false
		}
	}
	for _, p := range importPrefixes {
		if strings.HasPrefix(s, p) {
			return nil, false
		}
	}
	letters := false
	for _, r := range s {
		if r == '_' || ('a' <= r && r <= 'z') || ('A' <= r && r <= 'Z') || ('0' <= r && r <= '9') {
			letters = true
			break
		}
	}
	if !letters {
		return nil, false // `}`, `});`, `]`, ...
	}
	return []byte(strings.Join(strings.Fields(s), " ")), true
}

type dupLoc struct{ file, pos int32 }

// Result computes duplicated lines and blocks.
func (d *Duplication) Result() DuplicationResult {
	res := DuplicationResult{Files: map[string]FileDuplication{}, Skipped: d.tooLarge}
	if d.tooLarge {
		return res
	}
	w := DuplicationWindow
	index := map[uint64][]dupLoc{}
	buf := make([]byte, 8*w)
	window := func(f *dupFile, pos int) uint64 {
		for i := 0; i < w; i++ {
			binary.LittleEndian.PutUint64(buf[i*8:], f.hashes[pos+i])
		}
		h := fnv.New64a()
		_, _ = h.Write(buf)
		return h.Sum64()
	}
	for fi := range d.files {
		f := &d.files[fi]
		for pos := 0; pos+w <= len(f.hashes); pos++ {
			k := window(f, pos)
			index[k] = append(index[k], dupLoc{int32(fi), int32(pos)})
		}
	}
	// Mark every code line covered by a window that occurs elsewhere, and
	// remember for each window start one other occurrence.
	marked := make([][]bool, len(d.files))
	partner := make([]map[int]dupLoc, len(d.files))
	for _, locs := range index {
		if len(locs) < 2 {
			continue
		}
		for i, l := range locs {
			if marked[l.file] == nil {
				marked[l.file] = make([]bool, len(d.files[l.file].hashes))
				partner[l.file] = map[int]dupLoc{}
			}
			for j := 0; j < w; j++ {
				marked[l.file][int(l.pos)+j] = true
			}
			other := locs[(i+1)%len(locs)]
			partner[l.file][int(l.pos)] = other
		}
	}
	for fi := range d.files {
		f := &d.files[fi]
		fd := FileDuplication{CodeLines: len(f.hashes)}
		res.CodeLines += len(f.hashes)
		m := marked[fi]
		for pos := 0; pos < len(m); {
			if !m[pos] {
				pos++
				continue
			}
			start := pos
			for pos < len(m) && m[pos] {
				pos++
			}
			end := pos - 1
			fd.DuplicatedLines += end - start + 1
			if len(fd.Blocks) >= 50 {
				continue
			}
			where := ""
			for s := start; s <= end; s++ {
				if o, ok := partner[fi][s]; ok {
					of := &d.files[o.file]
					where = fmt.Sprintf(" Also at %s:%d.", of.path, of.lineNo[o.pos])
					break
				}
			}
			h := fnv.New64a()
			for s := start; s <= end; s++ {
				binary.LittleEndian.PutUint64(buf[:8], f.hashes[s])
				_, _ = h.Write(buf[:8])
			}
			first, last := int(f.lineNo[start]), int(f.lineNo[end])
			fd.Blocks = append(fd.Blocks, Raw{
				Rule: RuleDuplicated, StartLine: first, EndLine: last,
				Message: fmt.Sprintf("%d code lines (lines %d–%d) are duplicated.%s", end-start+1, first, last, where),
				Context: fmt.Sprintf("%016x", h.Sum64()),
			})
		}
		res.DuplicatedLines += fd.DuplicatedLines
		res.Blocks += len(fd.Blocks)
		res.Files[f.path] = fd
	}
	return res
}
