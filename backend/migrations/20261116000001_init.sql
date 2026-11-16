-- Workflow persistence for the Worker -> Reviewer A2A orchestrator.
-- Only the Rust backend connects to this database (PRD §2).

CREATE TABLE workflows (
    id              uuid PRIMARY KEY,
    question        text        NOT NULL CHECK (char_length(question) BETWEEN 1 AND 4000),
    status          text        NOT NULL CHECK (status IN ('queued', 'running', 'completed', 'failed')),
    verdict         text        NULL     CHECK (verdict IN ('pass', 'needs_changes')),
    error           text        NULL     CHECK (char_length(error) <= 1000),
    idempotency_key text        NULL     UNIQUE CHECK (char_length(idempotency_key) <= 128),
    created_at      timestamptz NOT NULL DEFAULT now(),
    started_at      timestamptz NULL,
    finished_at     timestamptz NULL,
    -- A completed workflow always has a verdict; a failed one always has an error.
    CHECK (status <> 'completed' OR verdict IS NOT NULL),
    CHECK (status <> 'failed' OR error IS NOT NULL)
);

-- Admission control: at most one queued/running workflow, enforced by the
-- database so it holds under concurrent requests and across restarts.
CREATE UNIQUE INDEX workflows_single_active ON workflows ((true))
    WHERE status IN ('queued', 'running');

CREATE INDEX workflows_created_at_idx ON workflows (created_at DESC, id DESC);

CREATE TABLE agent_runs (
    id             uuid PRIMARY KEY,
    workflow_id    uuid        NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    agent          text        NOT NULL CHECK (agent IN ('worker', 'reviewer')),
    status         text        NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    a2a_task_id    text        NULL,
    a2a_context_id text        NULL,
    a2a_state      text        NULL,
    error          text        NULL CHECK (char_length(error) <= 1000),
    started_at     timestamptz NOT NULL DEFAULT now(),
    finished_at    timestamptz NULL,
    UNIQUE (workflow_id, agent)
);

CREATE TABLE artifacts (
    id           uuid PRIMARY KEY,
    workflow_id  uuid        NOT NULL REFERENCES workflows (id) ON DELETE CASCADE,
    agent_run_id uuid        NOT NULL REFERENCES agent_runs (id) ON DELETE CASCADE,
    kind         text        NOT NULL CHECK (kind IN ('draft', 'review')),
    content      jsonb       NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workflow_id, kind)
);

CREATE INDEX agent_runs_workflow_idx ON agent_runs (workflow_id);
CREATE INDEX artifacts_workflow_idx ON artifacts (workflow_id);

-- Supabase exposes the public schema through its Data API. Enabling RLS with
-- no policies denies the anon/authenticated roles; the backend connects as
-- the table owner, which bypasses RLS.
ALTER TABLE workflows  ENABLE ROW LEVEL SECURITY;
ALTER TABLE agent_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE artifacts  ENABLE ROW LEVEL SECURITY;
