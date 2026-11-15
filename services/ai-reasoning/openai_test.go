package ai

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

type fakeOpenAI struct {
	t        *testing.T
	status   int
	response string
	lastBody map[string]any
	lastHdr  http.Header
	calls    int
}

func (f *fakeOpenAI) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	f.calls++
	if r.URL.Path != "/chat/completions" {
		f.t.Errorf("unexpected path %s", r.URL.Path)
	}
	body, _ := io.ReadAll(r.Body)
	f.lastBody = map[string]any{}
	if err := json.Unmarshal(body, &f.lastBody); err != nil {
		f.t.Errorf("request body is not JSON: %v", err)
	}
	f.lastHdr = r.Header.Clone()
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(f.status)
	io.WriteString(w, f.response)
}

func completion(finish, content, refusal string) string {
	c, _ := json.Marshal(content)
	ref := "null"
	if refusal != "" {
		b, _ := json.Marshal(refusal)
		ref = string(b)
	}
	return `{"id":"chatcmpl-1","object":"chat.completion","created":1,"model":"gpt-6-astra-2026-10-01",
		"choices":[{"index":0,"message":{"role":"assistant","content":` + string(c) + `,"refusal":` + ref + `},"finish_reason":"` + finish + `","logprobs":null}],
		"usage":{"prompt_tokens":1000,"completion_tokens":40,"total_tokens":1040,"prompt_tokens_details":{"cached_tokens":800}}}`
}

func TestOpenAIProvider(t *testing.T) {
	fake := &fakeOpenAI{t: t, status: 200}
	srv := httptest.NewServer(fake)
	defer srv.Close()
	p, err := NewOpenAI(OpenAIConfig{APIKey: "sk-test", BaseURL: srv.URL, ReasoningEffort: "medium"})
	if err != nil {
		t.Fatal(err)
	}
	if p.Name() != "openai" || p.Model() != "gpt-6-astra" {
		t.Errorf("defaults: %s %s", p.Name(), p.Model())
	}
	prompt := Prompt{System: SystemPrompt(), History: []Turn{{"user", "earlier q"}, {"assistant", "earlier a"}}, User: "new q", Schema: AnswerSchema()}
	ctx := context.Background()

	answer := `{"answerable":"yes","summary":"s","claims":[],"limitations":[],"followups":[]}`
	fake.response = completion("stop", answer, "")
	gen, err := p.Generate(ctx, prompt)
	if err != nil {
		t.Fatal(err)
	}
	if string(gen.JSON) != answer || gen.Model != "gpt-6-astra-2026-10-01" || gen.Usage != (Usage{InputTokens: 200, OutputTokens: 40, CacheReadTokens: 800}) {
		t.Errorf("generation: %+v", gen)
	}

	b := fake.lastBody
	if b["model"] != "gpt-6-astra" || b["max_completion_tokens"] != float64(32000) || b["reasoning_effort"] != "medium" {
		t.Errorf("model/limits/effort: %v %v %v", b["model"], b["max_completion_tokens"], b["reasoning_effort"])
	}
	if b["store"] != false || b["prompt_cache_key"] != "usie-repository-answers" {
		t.Errorf("store/cache key: %v %v", b["store"], b["prompt_cache_key"])
	}
	rf, _ := b["response_format"].(map[string]any)
	js, _ := rf["json_schema"].(map[string]any)
	if rf["type"] != "json_schema" || js["strict"] != true || js["name"] != "repository_answer" || js["schema"] == nil {
		t.Errorf("response_format: %v", rf)
	}
	var roles []string
	for _, m := range b["messages"].([]any) {
		roles = append(roles, m.(map[string]any)["role"].(string))
	}
	if strings.Join(roles, ",") != "system,user,assistant,user" {
		t.Errorf("roles: %v", roles)
	}
	if b["temperature"] != nil || b["tools"] != nil {
		t.Error("no sampling parameters or tools may be sent")
	}
	if fake.lastHdr.Get("Authorization") != "Bearer sk-test" {
		t.Errorf("auth header: %q", fake.lastHdr.Get("Authorization"))
	}

	// Without a configured effort, reasoning_effort is not sent.
	plain, _ := NewOpenAI(OpenAIConfig{APIKey: "sk-test", BaseURL: srv.URL, Model: "gpt-4.1"})
	if _, err := plain.Generate(ctx, prompt); err != nil || fake.lastBody["reasoning_effort"] != nil || fake.lastBody["model"] != "gpt-4.1" {
		t.Errorf("no effort: %v %v", err, fake.lastBody["reasoning_effort"])
	}

	// A refusal in the message, and a content-filter stop, are refusals.
	fake.response = completion("stop", "", "I can't help with that.")
	_, err = p.Generate(ctx, prompt)
	var refusal *RefusalError
	if !errors.As(err, &refusal) || refusal.Explanation != "I can't help with that." || refusal.Usage.OutputTokens != 40 {
		t.Errorf("refusal: %v %+v", err, refusal)
	}
	fake.response = completion("content_filter", "", "")
	if _, err := p.Generate(ctx, prompt); !errors.As(err, &refusal) || refusal.Category != "content_filter" {
		t.Errorf("content filter: %v", err)
	}

	// Truncated output is an error, not a partial answer.
	fake.response = completion("length", `{"answerable":`, "")
	if _, err := p.Generate(ctx, prompt); !errors.Is(err, ErrIncomplete) {
		t.Errorf("length: %v", err)
	}

	// Authentication errors are reported without retries or the key.
	fake.status, fake.response, fake.calls = 401, `{"error":{"message":"Incorrect API key provided: sk-test","type":"invalid_request_error","code":"invalid_api_key"}}`, 0
	_, err = p.Generate(ctx, prompt)
	if err == nil || !strings.Contains(err.Error(), "rejected the configured credentials") || strings.Contains(err.Error(), "sk-test") || fake.calls != 1 {
		t.Errorf("auth error: %v (calls %d)", err, fake.calls)
	}

	if _, err := NewOpenAI(OpenAIConfig{}); err == nil {
		t.Error("missing key must be rejected")
	}
	if _, err := NewOpenAI(OpenAIConfig{APIKey: "k", ReasoningEffort: "extreme"}); err == nil {
		t.Error("invalid effort must be rejected")
	}
}
