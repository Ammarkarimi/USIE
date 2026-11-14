-- AI reasoning: conversations about one repository version, and their
-- messages. Answers keep the exact context that was sent to the model and
-- the provider's usage figures, so every answer can be audited later.

CREATE TABLE ai_conversations (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id  uuid NOT NULL,
    repository_id    uuid NOT NULL,
    version_id       uuid NOT NULL,
    title            text NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    created_by       uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, id),
    FOREIGN KEY (organization_id, version_id) REFERENCES repository_versions (organization_id, id) ON DELETE CASCADE
);
CREATE INDEX ai_conversations_org_idx ON ai_conversations (organization_id, updated_at DESC);
CREATE INDEX ai_conversations_repo_idx ON ai_conversations (repository_id, updated_at DESC);

CREATE TABLE ai_messages (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id  uuid NOT NULL,
    conversation_id  uuid NOT NULL,
    seq              integer NOT NULL,
    role             text NOT NULL CHECK (role IN ('user', 'assistant')),
    -- The question (user) or the rendered answer text (assistant).
    content          text NOT NULL DEFAULT '',
    -- Optional graph node the question is about.
    focus_key        text NOT NULL DEFAULT '',
    -- pending → ok | error | refused (assistant messages only).
    status           text NOT NULL DEFAULT 'ok' CHECK (status IN ('pending', 'ok', 'error', 'refused')),
    -- Validated structured answer: claims, citations, limitations.
    answer           jsonb NOT NULL DEFAULT '{}',
    -- The context items sent to the model (ids, node keys, paths, lines).
    context          jsonb NOT NULL DEFAULT '[]',
    provider         text NOT NULL DEFAULT '',
    model            text NOT NULL DEFAULT '',
    input_tokens     integer NOT NULL DEFAULT 0,
    output_tokens    integer NOT NULL DEFAULT 0,
    cache_read_tokens integer NOT NULL DEFAULT 0,
    latency_ms       integer NOT NULL DEFAULT 0,
    error            text NOT NULL DEFAULT '',
    created_by       uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    completed_at     timestamptz,
    UNIQUE (conversation_id, seq),
    FOREIGN KEY (organization_id, conversation_id) REFERENCES ai_conversations (organization_id, id) ON DELETE CASCADE
);
-- Daily budget and per-user rate checks.
CREATE INDEX ai_messages_usage_idx ON ai_messages (organization_id, created_at);
CREATE INDEX ai_messages_user_idx ON ai_messages (created_by, created_at) WHERE role = 'user';
CREATE INDEX ai_messages_pending_idx ON ai_messages (created_at) WHERE status = 'pending';
