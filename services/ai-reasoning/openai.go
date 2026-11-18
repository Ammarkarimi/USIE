package ai

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"time"

	"github.com/openai/openai-go/v3"
	"github.com/openai/openai-go/v3/option"
	"github.com/openai/openai-go/v3/shared"
)

// DefaultOpenAIModel is OpenAI's recommended general model for complex
// reasoning and coding; it supports structured outputs on Chat Completions.
const DefaultOpenAIModel = "gpt-6-luna"

type OpenAIConfig struct {
	APIKey string
	Model  string
	// ReasoningEffort is sent as reasoning_effort when set (none, minimal,
	// low, medium, high, xhigh, max). Models that do not reason reject it:
	// leave it empty for those.
	ReasoningEffort string
	// MaxCompletionTokens includes reasoning tokens on reasoning models.
	MaxCompletionTokens int64
	Timeout             time.Duration
	// BaseURL overrides the API endpoint (tests, gateways).
	BaseURL    string
	HTTPClient *http.Client
}

// OpenAI calls OpenAI's Chat Completions API through the official SDK.
type OpenAI struct {
	client openai.Client
	cfg    OpenAIConfig
}

func NewOpenAI(cfg OpenAIConfig) (*OpenAI, error) {
	if cfg.APIKey == "" {
		return nil, errors.New("ai: an OpenAI API key is required")
	}
	if cfg.Model == "" {
		cfg.Model = DefaultOpenAIModel
	}
	switch cfg.ReasoningEffort {
	case "", "none", "minimal", "low", "medium", "high", "xhigh", "max":
	default:
		return nil, fmt.Errorf("ai: unknown reasoning effort %q", cfg.ReasoningEffort)
	}
	if cfg.MaxCompletionTokens <= 0 {
		cfg.MaxCompletionTokens = 32000
	}
	if cfg.Timeout <= 0 {
		cfg.Timeout = 5 * time.Minute
	}
	opts := []option.RequestOption{
		option.WithAPIKey(cfg.APIKey),
		option.WithRequestTimeout(cfg.Timeout),
		option.WithMaxRetries(2),
	}
	if cfg.BaseURL != "" {
		opts = append(opts, option.WithBaseURL(cfg.BaseURL))
	}
	if cfg.HTTPClient != nil {
		opts = append(opts, option.WithHTTPClient(cfg.HTTPClient))
	}
	return &OpenAI{client: openai.NewClient(opts...), cfg: cfg}, nil
}

func (o *OpenAI) Name() string  { return "openai" }
func (o *OpenAI) Model() string { return o.cfg.Model }

func (o *OpenAI) Generate(ctx context.Context, p Prompt) (*Generation, error) {
	messages := []openai.ChatCompletionMessageParamUnion{openai.SystemMessage(p.System)}
	for _, t := range p.History {
		if t.Role == "assistant" {
			messages = append(messages, openai.AssistantMessage(t.Content))
		} else {
			messages = append(messages, openai.UserMessage(t.Content))
		}
	}
	messages = append(messages, openai.UserMessage(p.User))

	params := openai.ChatCompletionNewParams{
		Model:               o.cfg.Model,
		Messages:            messages,
		MaxCompletionTokens: openai.Int(o.cfg.MaxCompletionTokens),
		// Structured outputs: the answer must match the schema exactly.
		ResponseFormat: openai.ChatCompletionNewParamsResponseFormatUnion{
			OfJSONSchema: &shared.ResponseFormatJSONSchemaParam{
				JSONSchema: shared.ResponseFormatJSONSchemaJSONSchemaParam{
					Name:        "repository_answer",
					Description: openai.String("An answer about a code repository whose claims cite evidence items."),
					Schema:      p.Schema,
					Strict:      openai.Bool(true),
				},
			},
		},
		// The system prompt is the same for every question; a shared key
		// helps OpenAI route requests to its prompt cache.
		PromptCacheKey: openai.String("usie-repository-answers"),
		// Do not keep completions on OpenAI's side for later retrieval.
		Store: openai.Bool(false),
	}
	if o.cfg.ReasoningEffort != "" {
		params.ReasoningEffort = shared.ReasoningEffort(o.cfg.ReasoningEffort)
	}

	resp, err := o.client.Chat.Completions.New(ctx, params)
	if err != nil {
		return nil, classifyOpenAIError(err)
	}
	cached := resp.Usage.PromptTokensDetails.CachedTokens
	usage := Usage{
		// prompt_tokens includes cached tokens; report them separately.
		InputTokens:     resp.Usage.PromptTokens - cached,
		OutputTokens:    resp.Usage.CompletionTokens,
		CacheReadTokens: cached,
	}
	if len(resp.Choices) == 0 {
		return nil, errors.New("ai: the provider returned no answer")
	}
	choice := resp.Choices[0]
	switch {
	case choice.Message.Refusal != "":
		return nil, &RefusalError{Explanation: choice.Message.Refusal, Usage: usage}
	case choice.FinishReason == "content_filter":
		return nil, &RefusalError{Category: "content_filter", Usage: usage}
	case choice.FinishReason == "length":
		return nil, ErrIncomplete
	case choice.Message.Content == "":
		return nil, errors.New("ai: the provider returned no answer text")
	}
	return &Generation{JSON: []byte(choice.Message.Content), Model: resp.Model, Usage: usage}, nil
}

// classifyOpenAIError turns API errors into messages that are safe to show
// users (no request details or keys).
func classifyOpenAIError(err error) error {
	var apiErr *openai.Error
	if errors.As(err, &apiErr) {
		switch apiErr.StatusCode {
		case http.StatusUnauthorized, http.StatusForbidden:
			return fmt.Errorf("ai: the AI provider rejected the configured credentials (HTTP %d)", apiErr.StatusCode)
		case http.StatusTooManyRequests:
			return fmt.Errorf("ai: the AI provider is rate limiting requests or the account's quota is used up; try again later (HTTP 429)")
		case http.StatusBadRequest, http.StatusNotFound, http.StatusRequestEntityTooLarge:
			return fmt.Errorf("ai: the AI provider rejected the request (HTTP %d)", apiErr.StatusCode)
		default:
			if apiErr.StatusCode >= 500 {
				return fmt.Errorf("ai: the AI provider is unavailable (HTTP %d)", apiErr.StatusCode)
			}
			return fmt.Errorf("ai: AI provider error (HTTP %d)", apiErr.StatusCode)
		}
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return errors.New("ai: the AI provider did not answer in time")
	}
	if errors.Is(err, context.Canceled) {
		return err
	}
	return fmt.Errorf("ai: could not reach the AI provider: %w", err)
}
