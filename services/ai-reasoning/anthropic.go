package ai

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/anthropics/anthropic-sdk-go/v2"
	"github.com/anthropics/anthropic-sdk-go/v2/option"
)

// DefaultAnthropicModel is used unless USIE_AI_MODEL overrides it.
const DefaultAnthropicModel = "claude-opus-5-5"

type AnthropicConfig struct {
	APIKey string
	Model  string
	// Effort: low, medium, high, xhigh or max. Empty uses "medium".
	Effort    string
	MaxTokens int64
	Timeout   time.Duration
	// BaseURL overrides the API endpoint (tests).
	BaseURL string
	// HTTPClient overrides the transport (tests).
	HTTPClient *http.Client
	// DisableFallbacks turns off server-side refusal fallbacks.
	DisableFallbacks bool
}

// Anthropic calls Claude through the official SDK.
type Anthropic struct {
	client anthropic.Client
	cfg    AnthropicConfig
}

func NewAnthropic(cfg AnthropicConfig) (*Anthropic, error) {
	if cfg.APIKey == "" {
		return nil, errors.New("ai: an Anthropic API key is required")
	}
	if cfg.Model == "" {
		cfg.Model = DefaultAnthropicModel
	}
	if cfg.Effort == "" {
		cfg.Effort = "medium"
	}
	switch cfg.Effort {
	case "low", "medium", "high", "xhigh", "max":
	default:
		return nil, fmt.Errorf("ai: unknown effort %q", cfg.Effort)
	}
	if cfg.MaxTokens <= 0 {
		cfg.MaxTokens = 16000
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
	return &Anthropic{client: anthropic.NewClient(opts...), cfg: cfg}, nil
}

func (a *Anthropic) Name() string  { return "anthropic" }
func (a *Anthropic) Model() string { return a.cfg.Model }

func (a *Anthropic) Generate(ctx context.Context, p Prompt) (*Generation, error) {
	var messages []anthropic.BetaMessageParam
	for _, t := range p.History {
		block := anthropic.NewBetaTextBlock(t.Content)
		if t.Role == "assistant" {
			messages = append(messages, anthropic.BetaMessageParam{OfAssistant: &anthropic.BetaAssistantMessageParam{Content: []anthropic.BetaContentBlockParamUnion{block}}})
		} else {
			messages = append(messages, anthropic.NewBetaUserMessage(block))
		}
	}
	messages = append(messages, anthropic.NewBetaUserMessage(anthropic.NewBetaTextBlock(p.User)))

	params := anthropic.BetaMessageNewParams{
		Model:     a.cfg.Model,
		MaxTokens: a.cfg.MaxTokens,
		// The system prompt is identical for every request: cache it.
		System: []anthropic.BetaTextBlockParam{{Text: p.System, CacheControl: anthropic.NewBetaCacheControlEphemeralParam()}},
		// Also cache the growing conversation prefix for follow-up questions.
		CacheControl: anthropic.NewBetaCacheControlEphemeralParam(),
		Messages:     messages,
		OutputConfig: anthropic.BetaOutputConfigParam{
			Effort: anthropic.BetaOutputConfigEffort(a.cfg.Effort),
			Format: anthropic.BetaJSONOutputFormatParam{Schema: p.Schema},
		},
	}
	if !a.cfg.DisableFallbacks {
		// If the model declines, the API re-runs the request on a fallback
		// model chosen by refusal category, inside the same call.
		params.Betas = []anthropic.AnthropicBeta{anthropic.AnthropicBetaServerSideFallback2026_07_01}
		params.Fallbacks = anthropic.BetaFallbacksParamOfDefault()
	}

	resp, err := a.client.Beta.Messages.New(ctx, params)
	if err != nil {
		return nil, classifyAnthropicError(err)
	}
	usage := Usage{
		InputTokens:      resp.Usage.InputTokens,
		OutputTokens:     resp.Usage.OutputTokens,
		CacheReadTokens:  resp.Usage.CacheReadInputTokens,
		CacheWriteTokens: resp.Usage.CacheCreationInputTokens,
	}
	switch resp.StopReason {
	case anthropic.BetaStopReasonRefusal:
		return nil, &RefusalError{Category: string(resp.StopDetails.Category), Explanation: resp.StopDetails.Explanation, Usage: usage}
	case anthropic.BetaStopReasonMaxTokens:
		return nil, ErrIncomplete
	}
	var text strings.Builder
	for _, block := range resp.Content {
		if b, ok := block.AsAny().(anthropic.BetaTextBlock); ok {
			text.WriteString(b.Text)
		}
	}
	if text.Len() == 0 {
		return nil, errors.New("ai: the provider returned no answer text")
	}
	return &Generation{JSON: []byte(text.String()), Model: string(resp.Model), Usage: usage}, nil
}

// classifyAnthropicError turns API errors into messages that are safe to
// show users (no request details) while keeping the status for logs.
func classifyAnthropicError(err error) error {
	var apiErr *anthropic.Error
	if errors.As(err, &apiErr) {
		switch apiErr.StatusCode {
		case http.StatusUnauthorized, http.StatusForbidden:
			return fmt.Errorf("ai: the AI provider rejected the configured credentials (HTTP %d)", apiErr.StatusCode)
		case http.StatusTooManyRequests:
			return fmt.Errorf("ai: the AI provider is rate limiting requests; try again shortly (HTTP 429)")
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
