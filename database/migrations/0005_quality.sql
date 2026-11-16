-- Code quality and security findings, their reviews, and quality gates.

-- Findings of the engine's syntax-tree rules are cached with the parse
-- result, keyed like the IR on (content, language, parser fingerprint).
ALTER TABLE file_analyses
    ADD COLUMN findings jsonb NOT NULL DEFAULT '[]',
    ADD COLUMN metrics  jsonb NOT NULL DEFAULT '{}';

-- Every rule violation found in one analyzed version.
CREATE TABLE findings (
    organization_id  uuid NOT NULL,
    version_id       uuid NOT NULL,
    repository_id    uuid NOT NULL,
    seq              integer NOT NULL,
    rule             text NOT NULL,
    category         text NOT NULL CHECK (category IN ('bug', 'vulnerability', 'hotspot', 'smell')),
    severity         text NOT NULL CHECK (severity IN ('blocker', 'critical', 'major', 'minor', 'info')),
    path             text NOT NULL,
    start_line       integer NOT NULL,
    start_col        integer NOT NULL DEFAULT 0,
    end_line         integer NOT NULL,
    end_col          integer NOT NULL DEFAULT 0,
    message          text NOT NULL,
    function_name    text NOT NULL DEFAULT '',
    -- Identifies the same finding across versions: rule, path, normalized
    -- matched code and occurrence. Never derived from a secret's value.
    fingerprint      text NOT NULL,
    -- Not present in the baseline (the previous analyzed version).
    is_new           boolean NOT NULL DEFAULT false,
    in_test          boolean NOT NULL DEFAULT false,
    PRIMARY KEY (version_id, seq),
    FOREIGN KEY (organization_id, version_id) REFERENCES repository_versions (organization_id, id) ON DELETE CASCADE
);
CREATE INDEX findings_version_idx ON findings (organization_id, version_id, category, severity);
CREATE INDEX findings_path_idx ON findings (organization_id, version_id, path);
CREATE INDEX findings_fingerprint_idx ON findings (organization_id, repository_id, fingerprint);

-- A person's decision about a finding. It applies to every version of the
-- repository where the same fingerprint appears; deleting it reopens it.
CREATE TABLE finding_reviews (
    organization_id  uuid NOT NULL,
    repository_id    uuid NOT NULL,
    fingerprint      text NOT NULL,
    -- accepted: real, deliberately not fixed. false_positive: the rule matched
    -- code it should not have. safe: a reviewed security hotspot.
    status           text NOT NULL CHECK (status IN ('accepted', 'false_positive', 'safe')),
    comment          text NOT NULL DEFAULT '' CHECK (length(comment) <= 2000),
    reviewed_by      uuid REFERENCES users(id) ON DELETE SET NULL,
    reviewed_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (repository_id, fingerprint),
    FOREIGN KEY (organization_id, repository_id) REFERENCES repositories (organization_id, id) ON DELETE CASCADE
);

-- Quality gate conditions of a repository. NULL means the default gate.
ALTER TABLE repositories ADD COLUMN quality_gate jsonb;
