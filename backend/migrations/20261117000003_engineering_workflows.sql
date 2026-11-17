-- Phase 6: human-supervised multi-agent engineering workflows.
--
-- Separate tables from the P0 Worker -> Reviewer workflow, which stays
-- untouched until its replacement is independently verified.
--
-- Integrity model:
--   * eng_workflows / eng_agent_runs: mutable, but only via guarded
--     `UPDATE ... WHERE version = <expected>` (application) and CHECKs here.
--   * eng_artifacts, eng_approvals, eng_audit_events: append-only. A trigger
--     refuses UPDATE, DELETE and TRUNCATE for every role, the owner included,
--     and the runtime role is granted SELECT/INSERT only.

CREATE TABLE eng_workflows (
    id                     uuid PRIMARY KEY,
    title                  text        NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    requirements           text        NOT NULL CHECK (char_length(requirements) BETWEEN 10 AND 8000),
    requested_by           text        NOT NULL CHECK (char_length(requested_by) BETWEEN 1 AND 200),
    status                 text        NOT NULL CHECK (status IN (
        'draft', 'awaiting_plan_approval', 'approved_for_execution', 'executing', 'reviewing',
        'testing', 'awaiting_delivery_approval', 'changes_requested', 'approved_for_delivery',
        'rejected', 'failed', 'cancelled')),
    version                bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    plan_revisions         int         NOT NULL DEFAULT 0 CHECK (plan_revisions >= 0),
    delivery_revisions     int         NOT NULL DEFAULT 0 CHECK (delivery_revisions >= 0),
    agent_runs             int         NOT NULL DEFAULT 0 CHECK (agent_runs >= 0),
    -- Budget snapshot taken at submission; never changed afterwards.
    max_plan_revisions     int         NOT NULL CHECK (max_plan_revisions BETWEEN 0 AND 10),
    max_delivery_revisions int         NOT NULL CHECK (max_delivery_revisions BETWEEN 0 AND 10),
    max_agent_runs         int         NOT NULL CHECK (max_agent_runs BETWEEN 1 AND 200),
    candidate_commit_sha   text        NULL CHECK (candidate_commit_sha ~ '^[0-9a-f]{40}$'),
    -- Hash the next human decision must reference: the plan artifact hash at
    -- the plan gate, the delivery-manifest hash at the delivery gate.
    pending_decision_hash  text        NULL CHECK (pending_decision_hash ~ '^[0-9a-f]{64}$'),
    error_code             text        NULL CHECK (error_code IN (
        'interrupted', 'agent_unavailable', 'agent_timeout', 'agent_failed', 'artifact_invalid',
        'tool_not_permitted', 'policy_violation', 'candidate_mismatch', 'budget_exceeded',
        'provider_unconfigured', 'internal_error')),
    error                  text        NULL CHECK (char_length(error) <= 1000),
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    finished_at            timestamptz NULL,
    CHECK (plan_revisions <= max_plan_revisions),
    CHECK (delivery_revisions <= max_delivery_revisions),
    CHECK (agent_runs <= max_agent_runs),
    CHECK ((status = 'failed') = (error_code IS NOT NULL)),
    CHECK ((status IN ('approved_for_delivery', 'rejected', 'failed', 'cancelled')) = (finished_at IS NOT NULL)),
    -- A delivery can only be approved for an identified candidate.
    CHECK (status <> 'approved_for_delivery' OR candidate_commit_sha IS NOT NULL)
);
CREATE INDEX eng_workflows_created_idx ON eng_workflows (created_at DESC, id DESC);

CREATE TABLE eng_agent_runs (
    id              uuid PRIMARY KEY,
    workflow_id     uuid        NOT NULL REFERENCES eng_workflows (id),
    role            text        NOT NULL CHECK (role IN (
        'project_manager', 'project_coordinator', 'developer', 'code_reviewer', 'qa_tester', 'quality_lead')),
    -- `<role>:<workflow version when the stage began>`: one live run per stage.
    stage_key       text        NOT NULL CHECK (char_length(stage_key) <= 64),
    attempt         int         NOT NULL CHECK (attempt BETWEEN 1 AND 10),
    agent_id        text        NOT NULL CHECK (char_length(agent_id) BETWEEN 1 AND 200),
    provider        text        NOT NULL CHECK (char_length(provider) BETWEEN 1 AND 64),
    model           text        NULL     CHECK (char_length(model) <= 128),
    correlation_id  uuid        NOT NULL,
    status          text        NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    a2a_task_id     text        NULL CHECK (char_length(a2a_task_id) <= 200),
    input_tokens    bigint      NULL CHECK (input_tokens >= 0),
    output_tokens   bigint      NULL CHECK (output_tokens >= 0),
    error_code      text        NULL CHECK (error_code IN (
        'interrupted', 'agent_unavailable', 'agent_timeout', 'agent_failed', 'artifact_invalid',
        'tool_not_permitted', 'policy_violation', 'candidate_mismatch', 'budget_exceeded',
        'provider_unconfigured', 'internal_error')),
    error           text        NULL CHECK (char_length(error) <= 1000),
    started_at      timestamptz NOT NULL DEFAULT now(),
    finished_at     timestamptz NULL,
    UNIQUE (workflow_id, stage_key, attempt),
    CHECK ((status = 'failed') = (error_code IS NOT NULL)),
    CHECK ((status = 'running') = (finished_at IS NULL))
);
-- Duplicate-execution guard: a retry (or a second orchestrator loop) cannot
-- start the same stage while a run is live or after it completed.
CREATE UNIQUE INDEX eng_agent_runs_one_live_per_stage ON eng_agent_runs (workflow_id, stage_key)
    WHERE status <> 'failed';
CREATE INDEX eng_agent_runs_workflow_idx ON eng_agent_runs (workflow_id, started_at);

CREATE TABLE eng_artifacts (
    id             uuid PRIMARY KEY,
    workflow_id    uuid        NOT NULL REFERENCES eng_workflows (id),
    agent_run_id   uuid        NULL REFERENCES eng_agent_runs (id),
    kind           text        NOT NULL CHECK (kind IN (
        'requirements_plan', 'work_assignment', 'code_candidate', 'review_report', 'qa_report',
        'quality_assessment', 'approval_decision', 'delivery_manifest')),
    schema_version int         NOT NULL CHECK (schema_version >= 1),
    producer_kind  text        NOT NULL CHECK (producer_kind IN ('agent', 'human', 'system')),
    producer_id    text        NOT NULL CHECK (char_length(producer_id) BETWEEN 1 AND 200),
    provider       text        NULL CHECK (char_length(provider) <= 64),
    model          text        NULL CHECK (char_length(model) <= 128),
    content        jsonb       NOT NULL CHECK (pg_column_size(content) <= 262144),
    content_hash   text        NOT NULL CHECK (content_hash ~ '^[0-9a-f]{64}$'),
    commit_sha     text        NULL CHECK (commit_sha ~ '^[0-9a-f]{40}$'),
    created_at     timestamptz NOT NULL DEFAULT now(),
    -- Agent artifacts come from a run; human/system ones never do.
    CHECK ((producer_kind = 'agent') = (agent_run_id IS NOT NULL)),
    CHECK (kind <> 'approval_decision' OR producer_kind = 'human')
);
CREATE INDEX eng_artifacts_workflow_idx ON eng_artifacts (workflow_id, kind, created_at DESC);

CREATE TABLE eng_approvals (
    id                  uuid PRIMARY KEY,
    workflow_id         uuid        NOT NULL REFERENCES eng_workflows (id),
    gate                text        NOT NULL CHECK (gate IN ('plan', 'delivery', 'workflow')),
    decision            text        NOT NULL CHECK (decision IN ('approve', 'request_changes', 'reject', 'cancel')),
    approver_id         text        NOT NULL CHECK (char_length(approver_id) BETWEEN 1 AND 200),
    approver_role       text        NOT NULL CHECK (approver_role IN ('requester', 'approver')),
    identity_issuer     text        NOT NULL CHECK (char_length(identity_issuer) BETWEEN 1 AND 200),
    comment             text        NULL CHECK (char_length(comment) <= 2000),
    reviewed_commit_sha text        NULL CHECK (reviewed_commit_sha ~ '^[0-9a-f]{40}$'),
    artifact_hash       text        NULL CHECK (artifact_hash ~ '^[0-9a-f]{64}$'),
    workflow_version    bigint      NOT NULL,
    idempotency_key     text        NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 128),
    request_hash        text        NOT NULL CHECK (request_hash ~ '^[0-9a-f]{64}$'),
    decided_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workflow_id, idempotency_key),
    -- One decision per workflow version: of two conflicting approvers, the
    -- second is stale by construction.
    UNIQUE (workflow_id, workflow_version),
    CHECK (gate = 'workflow' OR artifact_hash IS NOT NULL),
    CHECK (gate <> 'delivery' OR reviewed_commit_sha IS NOT NULL),
    CHECK ((gate = 'workflow') = (decision = 'cancel')),
    CHECK (decision NOT IN ('approve', 'request_changes', 'reject') OR approver_role = 'approver')
);

CREATE TABLE eng_audit_events (
    id               uuid PRIMARY KEY,
    workflow_id      uuid        NOT NULL REFERENCES eng_workflows (id),
    at               timestamptz NOT NULL DEFAULT clock_timestamp(),
    actor_kind       text        NOT NULL CHECK (actor_kind IN ('human', 'agent', 'system')),
    actor_id         text        NOT NULL CHECK (char_length(actor_id) BETWEEN 1 AND 200),
    action           text        NOT NULL CHECK (char_length(action) BETWEEN 1 AND 64),
    from_status      text        NULL,
    to_status        text        NULL,
    workflow_version bigint      NOT NULL,
    detail           jsonb       NOT NULL DEFAULT '{}'::jsonb CHECK (pg_column_size(detail) <= 16384)
);
CREATE INDEX eng_audit_events_workflow_idx ON eng_audit_events (workflow_id, at, id);

-- Append-only enforcement, independent of grants (also binds the owner).
CREATE FUNCTION eng_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is append-only (% refused)', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'insufficient_privilege';
END
$$;
CREATE TRIGGER eng_artifacts_append_only BEFORE UPDATE OR DELETE ON eng_artifacts
    FOR EACH ROW EXECUTE FUNCTION eng_append_only();
CREATE TRIGGER eng_approvals_append_only BEFORE UPDATE OR DELETE ON eng_approvals
    FOR EACH ROW EXECUTE FUNCTION eng_append_only();
CREATE TRIGGER eng_audit_append_only BEFORE UPDATE OR DELETE ON eng_audit_events
    FOR EACH ROW EXECUTE FUNCTION eng_append_only();
CREATE TRIGGER eng_artifacts_no_truncate BEFORE TRUNCATE ON eng_artifacts
    FOR EACH STATEMENT EXECUTE FUNCTION eng_append_only();
CREATE TRIGGER eng_approvals_no_truncate BEFORE TRUNCATE ON eng_approvals
    FOR EACH STATEMENT EXECUTE FUNCTION eng_append_only();
CREATE TRIGGER eng_audit_no_truncate BEFORE TRUNCATE ON eng_audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION eng_append_only();

-- Least privilege for the runtime role (created in 20261117000002).
GRANT SELECT, INSERT, UPDATE ON eng_workflows, eng_agent_runs TO a2a_app;
GRANT SELECT, INSERT ON eng_artifacts, eng_approvals, eng_audit_events TO a2a_app;

ALTER TABLE eng_workflows    ENABLE ROW LEVEL SECURITY;
ALTER TABLE eng_agent_runs   ENABLE ROW LEVEL SECURITY;
ALTER TABLE eng_artifacts    ENABLE ROW LEVEL SECURITY;
ALTER TABLE eng_approvals    ENABLE ROW LEVEL SECURITY;
ALTER TABLE eng_audit_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY a2a_app_eng_workflows  ON eng_workflows    FOR ALL TO a2a_app USING (true) WITH CHECK (true);
CREATE POLICY a2a_app_eng_agent_runs ON eng_agent_runs   FOR ALL TO a2a_app USING (true) WITH CHECK (true);
CREATE POLICY a2a_app_eng_artifacts  ON eng_artifacts    FOR ALL TO a2a_app USING (true) WITH CHECK (true);
CREATE POLICY a2a_app_eng_approvals  ON eng_approvals    FOR ALL TO a2a_app USING (true) WITH CHECK (true);
CREATE POLICY a2a_app_eng_audit      ON eng_audit_events FOR ALL TO a2a_app USING (true) WITH CHECK (true);

DO $$
DECLARE
    r text;
BEGIN
    FOREACH r IN ARRAY ARRAY['anon', 'authenticated'] LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN
            EXECUTE format('REVOKE ALL ON eng_workflows, eng_agent_runs, eng_artifacts, eng_approvals, eng_audit_events FROM %I', r);
        END IF;
    END LOOP;
END
$$;
