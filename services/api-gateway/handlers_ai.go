package api

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"unicode/utf8"

	"github.com/ammarkarimi/usie/internal/pipeline"
	"github.com/ammarkarimi/usie/internal/platform/blob"
	"github.com/ammarkarimi/usie/internal/store"
	ai "github.com/ammarkarimi/usie/services/ai-reasoning"
)

// AISources reads stored file content for AI evidence, with the same rules
// as the file endpoint: only retained UTF-8 text is returned.
func AISources(st *store.Store, blobs blob.Store, maxBytes int64) ai.Sources {
	return func(orgID, versionID string) ai.SourceReader {
		return func(ctx context.Context, path string) (string, error) {
			f, err := st.GetFile(ctx, orgID, versionID, path)
			if errors.Is(err, store.ErrNotFound) {
				return "", ai.ErrNoSource
			}
			if err != nil {
				return "", err
			}
			if !f.ContentStored || (f.Encoding != "utf-8" && f.Encoding != "utf-8-bom") {
				return "", ai.ErrNoSource
			}
			data, err := blob.GetBytes(ctx, blobs, pipeline.SourceKey(orgID, f.ContentHash), maxBytes)
			if errors.Is(err, blob.ErrNotFound) {
				return "", ai.ErrNoSource
			}
			if err != nil {
				return "", err
			}
			return strings.TrimPrefix(string(data), "\xef\xbb\xbf"), nil
		}
	}
}

func (s *Server) aiError(err error) error {
	switch {
	case errors.Is(err, ai.ErrNotFound):
		return errNotFound
	case errors.Is(err, ai.ErrNotReady):
		return &apiError{http.StatusConflict, "version_not_ready", "analysis of this version has not completed"}
	case errors.Is(err, ai.ErrBudget):
		return &apiError{http.StatusTooManyRequests, "ai_budget_exhausted", "the organization's daily AI budget is used up; it resets at 00:00 UTC"}
	case errors.Is(err, ai.ErrUserRate):
		return &apiError{http.StatusTooManyRequests, "ai_rate_limited", "you have reached the hourly limit of AI questions; try again later"}
	case errors.Is(err, ai.ErrBusy):
		return &apiError{http.StatusConflict, "ai_busy", "an answer is still being generated in this conversation"}
	case errors.Is(err, ai.ErrInvalidInput):
		return errBadRequest("%s", strings.TrimPrefix(err.Error(), "ai: invalid input: "))
	}
	return err
}

func (s *Server) requireAI() error {
	if s.AI == nil || !s.AI.Enabled() {
		return &apiError{http.StatusServiceUnavailable, "ai_disabled", "AI answers are not configured on this installation"}
	}
	return nil
}

func (s *Server) getAIStatus(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if s.AI == nil {
		writeJSON(w, http.StatusOK, ai.Status{DataNotice: "AI answers are disabled on this installation: no AI provider is configured."})
		return nil
	}
	st, err := s.AI.Status(r.Context(), p.OrgID)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusOK, st)
	return nil
}

func (s *Server) listConversations(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if s.AI == nil {
		writeJSON(w, http.StatusOK, map[string]any{"conversations": []ai.Conversation{}})
		return nil
	}
	repo := r.URL.Query().Get("repository_id")
	if repo != "" && !store.ValidID(repo) {
		return errBadRequest("repository_id must be a repository id")
	}
	limit, err := queryInt(r, "limit", 50, 1, 200)
	if err != nil {
		return err
	}
	list, err := s.AI.List(r.Context(), p.OrgID, strings.ToLower(repo), limit)
	if err != nil {
		return err
	}
	if list == nil {
		list = []ai.Conversation{}
	}
	writeJSON(w, http.StatusOK, map[string]any{"conversations": list})
	return nil
}

type askBody struct {
	VersionID string `json:"version_id"`
	Question  string `json:"question"`
	FocusKey  string `json:"focus_key"`
}

func (s *Server) ask(w http.ResponseWriter, r *http.Request, p *store.Principal, conversationID string, in askBody) error {
	if in.FocusKey != "" && (!utf8.ValidString(in.FocusKey) || len(in.FocusKey) > 2048) {
		return errBadRequest("focus_key is not valid")
	}
	conv, err := s.AI.Ask(r.Context(), ai.AskInput{
		OrgID: p.OrgID, UserID: p.UserID, VersionID: in.VersionID, ConversationID: conversationID,
		Question: in.Question, FocusKey: in.FocusKey,
	})
	if err != nil {
		return s.aiError(err)
	}
	// The question text is not copied into the audit trail; its length is.
	s.audit(r, p, "ai.question_asked", "ai_conversation", conv.ID, "success", map[string]any{
		"version_id": conv.VersionID, "question_chars": utf8.RuneCountInString(in.Question), "focus": in.FocusKey != "",
	})
	writeJSON(w, http.StatusAccepted, conv)
	return nil
}

func (s *Server) createConversation(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if err := s.requireAI(); err != nil {
		return err
	}
	var in askBody
	if err := decode(r, &in); err != nil {
		return err
	}
	if !store.ValidID(in.VersionID) {
		return errBadRequest("version_id must be a version id")
	}
	in.VersionID = strings.ToLower(in.VersionID)
	return s.ask(w, r, p, "", in)
}

func (s *Server) askFollowup(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if err := s.requireAI(); err != nil {
		return err
	}
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	var in askBody
	if err := decode(r, &in); err != nil {
		return err
	}
	if in.VersionID != "" {
		return errBadRequest("version_id cannot be changed within a conversation")
	}
	return s.ask(w, r, p, id, in)
}

func (s *Server) getConversation(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if s.AI == nil {
		return errNotFound
	}
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	conv, err := s.AI.Get(r.Context(), p.OrgID, id)
	if err != nil {
		return s.aiError(err)
	}
	writeJSON(w, http.StatusOK, conv)
	return nil
}

func (s *Server) deleteConversation(w http.ResponseWriter, r *http.Request, p *store.Principal) error {
	if s.AI == nil {
		return errNotFound
	}
	id, err := pathID(r, "id")
	if err != nil {
		return err
	}
	if err := s.AI.Delete(r.Context(), p.OrgID, id, p.UserID, store.RoleAtLeast(p.Role, "admin")); err != nil {
		return s.aiError(err)
	}
	s.audit(r, p, "ai.conversation_deleted", "ai_conversation", id, "success", nil)
	w.WriteHeader(http.StatusNoContent)
	return nil
}
