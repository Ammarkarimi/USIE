package ai

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"

	graph "github.com/ammarkarimi/usie/services/knowledge-graph"
)

type Config struct {
	// DailyTokenBudget caps tokens (input, output and cache) per organization
	// per UTC day. 0 means no cap.
	DailyTokenBudget int64
	// UserRequestsPerHour caps questions per user. 0 means no cap.
	UserRequestsPerHour int
	// MaxConcurrent bounds simultaneous generations in this process.
	MaxConcurrent int
	// MaxHistoryTurns is how many earlier exchanges are sent as context.
	MaxHistoryTurns int
	// Timeout bounds one generation, retrieval included.
	Timeout time.Duration
}

// Sources returns a SourceReader for one version of one organization.
type Sources func(orgID, versionID string) SourceReader

type Service struct {
	Pool     *pgxpool.Pool
	Provider Provider
	Graph    graph.Store
	Sources  Sources
	Log      *slog.Logger
	Cfg      Config
	Limits   RetrievalLimits

	once    sync.Once
	slots   chan struct{}
	running sync.WaitGroup
}

var (
	ErrNotFound      = errors.New("ai: not found")
	ErrNotReady      = errors.New("ai: the version has not finished analysis")
	ErrBudget        = errors.New("ai: the organization's daily AI token budget is used up")
	ErrUserRate      = errors.New("ai: hourly question limit reached")
	ErrBusy          = errors.New("ai: an answer is still being generated in this conversation")
	ErrInvalidInput  = errors.New("ai: invalid input")
	errNoProvider    = errors.New("ai: no AI provider is configured")
	maxQuestionChars = 4000
)

func (s *Service) init() {
	s.once.Do(func() {
		n := s.Cfg.MaxConcurrent
		if n <= 0 {
			n = 4
		}
		s.slots = make(chan struct{}, n)
		if s.Cfg.MaxHistoryTurns <= 0 {
			s.Cfg.MaxHistoryTurns = 4
		}
		if s.Cfg.Timeout <= 0 {
			s.Cfg.Timeout = 6 * time.Minute
		}
		if s.Limits.MaxChars == 0 {
			s.Limits = DefaultRetrievalLimits()
		}
	})
}

// Enabled reports whether questions can be answered.
func (s *Service) Enabled() bool { return s != nil && s.Provider != nil }

type Status struct {
	Enabled          bool   `json:"enabled"`
	Provider         string `json:"provider"`
	Model            string `json:"model"`
	DailyTokenBudget int64  `json:"daily_token_budget"`
	TokensUsedToday  int64  `json:"tokens_used_today"`
	// DataNotice tells users what leaves the installation.
	DataNotice string `json:"data_notice"`
}

func (s *Service) Status(ctx context.Context, orgID string) (Status, error) {
	st := Status{Enabled: s.Enabled(), DailyTokenBudget: s.Cfg.DailyTokenBudget}
	if !st.Enabled {
		st.DataNotice = "AI answers are disabled on this installation: no AI provider is configured."
		return st, nil
	}
	st.Provider, st.Model = s.Provider.Name(), s.Provider.Model()
	st.DataNotice = fmt.Sprintf("Questions, earlier answers in the conversation, and excerpts of the repository's source code and analysis are sent to %s (model %s) to produce answers.", st.Provider, st.Model)
	used, err := s.usedToday(ctx, orgID)
	st.TokensUsedToday = used
	return st, err
}

func (s *Service) usedToday(ctx context.Context, orgID string) (int64, error) {
	return UsedToday(ctx, s.Pool, orgID)
}

// UsedToday is the organization's AI token use since 00:00 UTC, across
// answers and code change proposals, which share one daily budget.
func UsedToday(ctx context.Context, pool *pgxpool.Pool, orgID string) (int64, error) {
	var used int64
	err := pool.QueryRow(ctx, `SELECT coalesce(sum(t), 0) FROM (
		SELECT input_tokens + output_tokens + cache_read_tokens AS t FROM ai_messages
		 WHERE organization_id = $1 AND created_at >= date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
		UNION ALL
		SELECT input_tokens + output_tokens + cache_read_tokens FROM code_changes
		 WHERE organization_id = $1 AND created_at >= date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC') u`, orgID).Scan(&used)
	return used, err
}

type Conversation struct {
	ID           string    `json:"id"`
	RepositoryID string    `json:"repository_id"`
	VersionID    string    `json:"version_id"`
	Title        string    `json:"title"`
	CreatedBy    *string   `json:"created_by"`
	CreatedAt    time.Time `json:"created_at"`
	UpdatedAt    time.Time `json:"updated_at"`
	Messages     []Message `json:"messages,omitempty"`
}

type Message struct {
	ID           string          `json:"id"`
	Seq          int             `json:"seq"`
	Role         string          `json:"role"`
	Content      string          `json:"content"`
	FocusKey     string          `json:"focus_key"`
	Status       string          `json:"status"`
	Answer       json.RawMessage `json:"answer"`
	Context      json.RawMessage `json:"context"`
	Provider     string          `json:"provider"`
	Model        string          `json:"model"`
	InputTokens  int             `json:"input_tokens"`
	OutputTokens int             `json:"output_tokens"`
	CacheTokens  int             `json:"cache_read_tokens"`
	LatencyMS    int             `json:"latency_ms"`
	Error        string          `json:"error"`
	CreatedAt    time.Time       `json:"created_at"`
	CompletedAt  *time.Time      `json:"completed_at"`
}

// AskInput is a new question, in a new or an existing conversation.
type AskInput struct {
	OrgID          string
	UserID         string
	VersionID      string // new conversations only
	ConversationID string // follow-ups only
	Question       string
	FocusKey       string
}

// Ask records the question and a pending answer, then generates the answer
// in the background. Callers poll the conversation for the result.
func (s *Service) Ask(ctx context.Context, in AskInput) (Conversation, error) {
	s.init()
	if !s.Enabled() {
		return Conversation{}, errNoProvider
	}
	in.Question = strings.TrimSpace(in.Question)
	if in.Question == "" || utf8.RuneCountInString(in.Question) > maxQuestionChars || !utf8.ValidString(in.Question) {
		return Conversation{}, fmt.Errorf("%w: the question must be between 1 and %d characters", ErrInvalidInput, maxQuestionChars)
	}
	if len(in.FocusKey) > 2048 {
		return Conversation{}, fmt.Errorf("%w: focus key is too long", ErrInvalidInput)
	}
	if err := s.checkLimits(ctx, in.OrgID, in.UserID); err != nil {
		return Conversation{}, err
	}

	var conv Conversation
	var pendingID string
	err := pgx.BeginFunc(ctx, s.Pool, func(tx pgx.Tx) error {
		if in.ConversationID == "" {
			var status string
			err := tx.QueryRow(ctx, `SELECT repository_id, status FROM repository_versions WHERE organization_id = $1 AND id = $2`, in.OrgID, in.VersionID).Scan(&conv.RepositoryID, &status)
			if errors.Is(err, pgx.ErrNoRows) {
				return ErrNotFound
			}
			if err != nil {
				return err
			}
			if status != "ready" {
				return ErrNotReady
			}
			err = tx.QueryRow(ctx, `INSERT INTO ai_conversations (organization_id, repository_id, version_id, title, created_by)
				VALUES ($1, $2, $3, $4, $5) RETURNING id, version_id, title, created_by::text, created_at, updated_at`,
				in.OrgID, conv.RepositoryID, in.VersionID, title(in.Question), in.UserID).
				Scan(&conv.ID, &conv.VersionID, &conv.Title, &conv.CreatedBy, &conv.CreatedAt, &conv.UpdatedAt)
			if err != nil {
				return err
			}
		} else {
			// Lock the conversation so concurrent follow-ups get distinct sequence numbers.
			err := tx.QueryRow(ctx, `SELECT id, repository_id, version_id, title, created_by::text, created_at, updated_at FROM ai_conversations
				WHERE organization_id = $1 AND id = $2 FOR UPDATE`, in.OrgID, in.ConversationID).
				Scan(&conv.ID, &conv.RepositoryID, &conv.VersionID, &conv.Title, &conv.CreatedBy, &conv.CreatedAt, &conv.UpdatedAt)
			if errors.Is(err, pgx.ErrNoRows) {
				return ErrNotFound
			}
			if err != nil {
				return err
			}
			var pending bool
			if err := tx.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM ai_messages WHERE conversation_id = $1 AND status = 'pending')`, conv.ID).Scan(&pending); err != nil {
				return err
			}
			if pending {
				return ErrBusy
			}
		}
		var next int
		if err := tx.QueryRow(ctx, `SELECT coalesce(max(seq), 0) + 1 FROM ai_messages WHERE conversation_id = $1`, conv.ID).Scan(&next); err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `INSERT INTO ai_messages (organization_id, conversation_id, seq, role, content, focus_key, created_by)
			VALUES ($1, $2, $3, 'user', $4, $5, $6)`, in.OrgID, conv.ID, next, in.Question, in.FocusKey, in.UserID); err != nil {
			return err
		}
		if err := tx.QueryRow(ctx, `INSERT INTO ai_messages (organization_id, conversation_id, seq, role, status, focus_key, provider, model, created_by)
			VALUES ($1, $2, $3, 'assistant', 'pending', $4, $5, $6, $7) RETURNING id`,
			in.OrgID, conv.ID, next+1, in.FocusKey, s.Provider.Name(), s.Provider.Model(), in.UserID).Scan(&pendingID); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `UPDATE ai_conversations SET updated_at = now() WHERE id = $1`, conv.ID)
		return err
	})
	if err != nil {
		return Conversation{}, err
	}

	s.running.Add(1)
	go func() {
		defer s.running.Done()
		s.answer(in.OrgID, conv, pendingID, in.Question, in.FocusKey)
	}()
	return s.Get(ctx, in.OrgID, conv.ID)
}

func (s *Service) checkLimits(ctx context.Context, orgID, userID string) error {
	if s.Cfg.DailyTokenBudget > 0 {
		used, err := s.usedToday(ctx, orgID)
		if err != nil {
			return err
		}
		if used >= s.Cfg.DailyTokenBudget {
			return ErrBudget
		}
	}
	if s.Cfg.UserRequestsPerHour > 0 {
		var n int
		if err := s.Pool.QueryRow(ctx, `SELECT count(*) FROM ai_messages WHERE created_by = $1 AND role = 'user' AND created_at > now() - interval '1 hour'`, userID).Scan(&n); err != nil {
			return err
		}
		if n >= s.Cfg.UserRequestsPerHour {
			return ErrUserRate
		}
	}
	return nil
}

func title(q string) string {
	q = strings.Join(strings.Fields(q), " ")
	if utf8.RuneCountInString(q) <= 80 {
		return q
	}
	r := []rune(q)
	return string(r[:79]) + "…"
}

// answer runs retrieval and generation for one pending message and records
// the outcome. It never panics into the caller and always finalizes the row.
func (s *Service) answer(orgID string, conv Conversation, messageID, question, focusKey string) {
	ctx, cancel := context.WithTimeout(context.Background(), s.Cfg.Timeout)
	defer cancel()
	log := s.Log.With("conversation_id", conv.ID, "message_id", messageID)
	started := time.Now()

	finish := func(status string, answer *Answer, pack *ContextPack, gen *Generation, usage Usage, errMsg string) {
		answerJSON, contextJSON := []byte("{}"), []byte("[]")
		if answer != nil {
			answerJSON, _ = json.Marshal(answer)
		}
		if pack != nil {
			contextJSON, _ = json.Marshal(pack.Items)
		}
		model := s.Provider.Model()
		if gen != nil && gen.Model != "" {
			model = gen.Model
		}
		content := ""
		if answer != nil {
			content = answer.Summary
		}
		// A detached context: the outcome must be recorded even on timeout.
		wctx, wcancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer wcancel()
		_, err := s.Pool.Exec(wctx, `UPDATE ai_messages SET status = $3, content = $4, answer = $5, context = $6, model = $7,
			input_tokens = $8, output_tokens = $9, cache_read_tokens = $10, latency_ms = $11, error = $12, completed_at = now()
			WHERE organization_id = $1 AND id = $2 AND status = 'pending'`,
			orgID, messageID, status, content, answerJSON, contextJSON, model,
			usage.InputTokens+usage.CacheWriteTokens, usage.OutputTokens, usage.CacheReadTokens, time.Since(started).Milliseconds(), clip(errMsg, 1000))
		if err != nil {
			log.Error("recording AI answer failed", "error", err.Error())
		}
		s.Pool.Exec(wctx, `UPDATE ai_conversations SET updated_at = now() WHERE id = $1`, conv.ID) //nolint:errcheck
		log.Info("AI answer finished", "status", status, "model", model, "input_tokens", usage.InputTokens, "output_tokens", usage.OutputTokens,
			"cache_read_tokens", usage.CacheReadTokens, "latency_ms", time.Since(started).Milliseconds(), "error", errMsg)
	}
	defer func() {
		if r := recover(); r != nil {
			log.Error("AI answer panicked", "panic", fmt.Sprint(r))
			finish("error", nil, nil, nil, Usage{}, "internal error while answering")
		}
	}()

	select {
	case s.slots <- struct{}{}:
		defer func() { <-s.slots }()
	case <-ctx.Done():
		finish("error", nil, nil, nil, Usage{}, "timed out waiting for capacity")
		return
	}

	history, err := s.history(ctx, orgID, conv.ID, messageID)
	if err != nil {
		finish("error", nil, nil, nil, Usage{}, "could not load the conversation")
		log.Error("loading history failed", "error", err.Error())
		return
	}
	retriever := &Retriever{Graph: s.Graph, Read: s.Sources(orgID, conv.VersionID), Limits: s.Limits}
	scope := graph.Scope{OrganizationID: orgID, VersionID: conv.VersionID}
	pack, err := retriever.Retrieve(ctx, scope, question, focusKey)
	if err != nil {
		msg := "could not collect evidence from the knowledge graph"
		if errors.Is(err, graph.ErrNotFound) {
			msg = "the selected code element no longer exists in this version"
		}
		finish("error", nil, nil, nil, Usage{}, msg)
		log.Error("retrieval failed", "error", err.Error())
		return
	}

	gen, err := s.Provider.Generate(ctx, Prompt{System: SystemPrompt(), History: history, User: RenderUserTurn(pack, question), Schema: AnswerSchema()})
	var refusal *RefusalError
	switch {
	case errors.As(err, &refusal):
		finish("refused", nil, pack, nil, refusal.Usage, "The AI provider declined to answer this question.")
		return
	case err != nil:
		finish("error", nil, pack, nil, Usage{}, err.Error())
		return
	}
	answer, err := ValidateAnswer(gen.JSON, pack.IDs())
	if err != nil {
		finish("error", nil, pack, gen, gen.Usage, err.Error())
		return
	}
	finish("ok", answer, pack, gen, gen.Usage, "")
}

// history returns earlier completed exchanges, most recent last.
func (s *Service) history(ctx context.Context, orgID, conversationID, exclude string) ([]Turn, error) {
	rows, err := s.Pool.Query(ctx, `SELECT role, content, status, answer FROM ai_messages
		WHERE organization_id = $1 AND conversation_id = $2 AND id <> $3 ORDER BY seq`, orgID, conversationID, exclude)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	type row struct {
		role, content, status string
		answer                []byte
	}
	var all []row
	for rows.Next() {
		var r row
		if err := rows.Scan(&r.role, &r.content, &r.status, &r.answer); err != nil {
			return nil, err
		}
		all = append(all, r)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	// Pair each question with its answer; skip failed exchanges and the
	// question being answered now (the last user row).
	var turns []Turn
	for i := 0; i+1 < len(all); i++ {
		if all[i].role != "user" || all[i+1].role != "assistant" || all[i+1].status != "ok" {
			continue
		}
		var a Answer
		text := all[i+1].content
		if json.Unmarshal(all[i+1].answer, &a) == nil && a.Summary != "" {
			text = a.PlainText()
		}
		turns = append(turns, Turn{Role: "user", Content: all[i].content}, Turn{Role: "assistant", Content: text})
		i++
	}
	if max := s.Cfg.MaxHistoryTurns * 2; len(turns) > max {
		turns = turns[len(turns)-max:]
	}
	return turns, nil
}

// FailStale marks answers left pending by a stopped process as failed.
func (s *Service) FailStale(ctx context.Context) error {
	s.init()
	_, err := s.Pool.Exec(ctx, `UPDATE ai_messages SET status = 'error', error = 'the answer was interrupted (the service restarted); ask again', completed_at = now()
		WHERE status = 'pending' AND created_at < now() - $1::interval`, (s.Cfg.Timeout + time.Minute).String())
	return err
}

// Wait blocks until background generations finish (shutdown and tests).
func (s *Service) Wait() { s.running.Wait() }

const conversationColumns = `id, repository_id, version_id, title, created_by::text, created_at, updated_at`

func (s *Service) List(ctx context.Context, orgID, repositoryID string, limit int) ([]Conversation, error) {
	rows, err := s.Pool.Query(ctx, `SELECT `+conversationColumns+` FROM ai_conversations
		WHERE organization_id = $1 AND ($2 = '' OR repository_id::text = $2) ORDER BY updated_at DESC LIMIT $3`, orgID, repositoryID, limit)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, func(row pgx.CollectableRow) (Conversation, error) {
		var c Conversation
		err := row.Scan(&c.ID, &c.RepositoryID, &c.VersionID, &c.Title, &c.CreatedBy, &c.CreatedAt, &c.UpdatedAt)
		return c, err
	})
}

func (s *Service) Get(ctx context.Context, orgID, id string) (Conversation, error) {
	var c Conversation
	err := s.Pool.QueryRow(ctx, `SELECT `+conversationColumns+` FROM ai_conversations WHERE organization_id = $1 AND id = $2`, orgID, id).
		Scan(&c.ID, &c.RepositoryID, &c.VersionID, &c.Title, &c.CreatedBy, &c.CreatedAt, &c.UpdatedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return c, ErrNotFound
	}
	if err != nil {
		return c, err
	}
	rows, err := s.Pool.Query(ctx, `SELECT id, seq, role, content, focus_key, status, answer, context, provider, model, input_tokens, output_tokens,
		cache_read_tokens, latency_ms, error, created_at, completed_at FROM ai_messages WHERE organization_id = $1 AND conversation_id = $2 ORDER BY seq`, orgID, id)
	if err != nil {
		return c, err
	}
	c.Messages, err = pgx.CollectRows(rows, pgx.RowToStructByPos[Message])
	return c, err
}

// Delete removes a conversation. Only its creator or an administrator may.
func (s *Service) Delete(ctx context.Context, orgID, id, userID string, isAdmin bool) error {
	tag, err := s.Pool.Exec(ctx, `DELETE FROM ai_conversations WHERE organization_id = $1 AND id = $2 AND ($3 OR created_by = $4)`, orgID, id, isAdmin, userID)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return nil
}
