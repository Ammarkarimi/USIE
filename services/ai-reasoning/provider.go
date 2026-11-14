// Package ai implements grounded question answering about an analyzed
// repository version: retrieval of evidence from the knowledge graph and
// source, a provider-neutral model interface, validation of the model's
// citations, and persistence of conversations.
//
// The model is given evidence and asked for a structured answer whose claims
// cite that evidence. It is given no tools and cannot act on anything.
package ai

import (
	"context"
	"encoding/json"
	"errors"
)

// Provider generates a structured answer from a prompt. Implementations
// must return JSON that conforms to Prompt.Schema, or an error.
type Provider interface {
	// Name identifies the provider ("anthropic").
	Name() string
	// Model is the model identifier sent to the provider.
	Model() string
	Generate(ctx context.Context, p Prompt) (*Generation, error)
}

// Turn is one prior exchange in a conversation.
type Turn struct {
	Role    string // "user" or "assistant"
	Content string
}

type Prompt struct {
	// System is stable across requests (it is cached by providers that can).
	System string
	// History holds earlier turns, oldest first.
	History []Turn
	// User is the new user turn: evidence followed by the question.
	User string
	// Schema is the JSON schema the output must follow.
	Schema json.RawMessage
}

type Usage struct {
	InputTokens      int64 `json:"input_tokens"`
	OutputTokens     int64 `json:"output_tokens"`
	CacheReadTokens  int64 `json:"cache_read_tokens"`
	CacheWriteTokens int64 `json:"cache_write_tokens"`
}

func (u Usage) Total() int64 {
	return u.InputTokens + u.OutputTokens + u.CacheReadTokens + u.CacheWriteTokens
}

type Generation struct {
	JSON json.RawMessage
	// Model is the model that produced the output (it can differ from the
	// requested one when a fallback served the request).
	Model string
	Usage Usage
}

var (
	// ErrRefused means the provider declined to answer.
	ErrRefused = errors.New("the AI provider declined to answer this request")
	// ErrIncomplete means the output was cut off (token limit).
	ErrIncomplete = errors.New("the AI provider's answer was cut off before it was complete")
)

// RefusalError carries the provider's refusal details.
type RefusalError struct {
	Category    string
	Explanation string
	Usage       Usage
}

func (e *RefusalError) Error() string { return ErrRefused.Error() }
func (e *RefusalError) Unwrap() error { return ErrRefused }
