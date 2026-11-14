# 0009 — Grounded AI answers: evidence retrieval, validated citations, no model tools

## Context
The specification asks for AI reasoning over the knowledge graph with a
provider abstraction and prompt-injection defenses, and forbids presenting
an AI assumption as verified fact. Repository content is untrusted.

## Decision
- **Retrieval, not autonomy.** For each question the service collects
  evidence itself: the focus node (when the user asks from a symbol) and its
  neighbours, symbols and files whose names match terms in the question, or
  the README and a file index for broad questions. Each item carries the
  relationships of the node with their analysis confidence and a numbered
  source excerpt. Evidence size is bounded (120,000 characters, 12,000 per
  item). The model gets **no tools**: it cannot read more, run anything or
  change anything.
- **Structured, checkable answers.** The model returns JSON (structured
  output) with a summary and claims; each claim states whether it rests on
  evidence or is an inference and cites evidence ids. The server keeps only
  citations that refer to evidence actually sent and marks evidence claims
  without a valid citation as *unsupported*. Nothing is ever upgraded.
- **Injection framing.** Evidence sits inside an element whose name is a
  random per-request token, so repository text written in advance cannot
  close it; item delimiters inside content are neutralized; the system prompt
  states that evidence is untrusted data.
- **Provider interface.** `ai.Provider` has one method that turns a prompt and
  a JSON schema into JSON. The implementation uses Claude through the official
  Go SDK: `claude-opus-5-5`, effort `medium`, structured output, prompt
  caching of the system prompt and conversation prefix, and Anthropic's
  server-side refusal fallback (a declined request is re-run on a fallback
  model chosen by refusal category). All of this is configurable; a
  different provider implements the same interface.
- **Asynchronous answers with limits.** Asking stores the question and a
  pending answer, generation runs in the API process, and the client polls.
  Limits: tokens per organization per day, questions per user per hour,
  concurrent generations per process. Answers interrupted by a restart are
  marked failed at startup.
- **Auditability.** Each answer stores the exact evidence sent, the validated
  answer, the serving model, token usage and latency. Audit events record
  that a question was asked and its length, not its text.
- **Go, not Python.** This subsystem is retrieval over PostgreSQL plus HTTPS
  calls; it needs no Python ML stack, and adding a fourth runtime to the
  single-machine deployment would cost more than it gives. The Go minimum
  moved to 1.25, which the Anthropic Go SDK requires.

## Consequences
- Answers can only be as good as the evidence retrieval finds. Retrieval is
  lexical (names in the question) plus the graph neighbourhood; there are no
  embeddings yet, so questions phrased without code names rely on the
  README and file index.
- With several API replicas each runs its own generations; budgets are
  enforced through the database and hold across replicas, while the
  concurrency bound is per process.
- Source excerpts are sent to the configured provider. The feature is off by
  default and says what it sends before each question.
- AIRecommendation (stored recommendations) remains planned.
