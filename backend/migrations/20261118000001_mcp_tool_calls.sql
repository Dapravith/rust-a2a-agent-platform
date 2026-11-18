-- Phase 6B C6: append-only audit of every MCP tool call (allowed, refused, failed).
--
-- Written only by the MCP server, through its own role `a2a_mcp`, which can
-- INSERT into this table and nothing else (it cannot read workflows,
-- artifacts or approvals). The API (`a2a_app`) can read it to cross-check
-- agent claims against what actually executed.
--
-- No foreign key to eng_workflows: refused or forged requests (bad token)
-- must be recorded too, and they have no valid workflow.

CREATE TABLE eng_tool_calls (
    id               uuid PRIMARY KEY,
    at               timestamptz NOT NULL DEFAULT clock_timestamp(),
    workflow_id      uuid        NULL,
    run_id           uuid        NULL,
    role             text        NULL CHECK (role IS NULL OR role IN
                        ('project_manager', 'project_coordinator', 'developer',
                         'code_reviewer', 'qa_tester', 'quality_lead')),
    agent_id         text        NULL CHECK (agent_id IS NULL OR char_length(agent_id) BETWEEN 1 AND 128),
    workspace_id     uuid        NULL,
    tool             text        NOT NULL CHECK (char_length(tool) BETWEEN 1 AND 128),
    target           text        NULL CHECK (target IS NULL OR char_length(target) <= 1024),
    arguments_sha256 text        NULL CHECK (arguments_sha256 IS NULL OR arguments_sha256 ~ '^[0-9a-f]{64}$'),
    outcome          text        NOT NULL CHECK (outcome IN ('allowed', 'refused', 'failed')),
    error_code       text        NULL CHECK (error_code IS NULL OR char_length(error_code) BETWEEN 1 AND 64),
    head_sha         text        NULL CHECK (head_sha IS NULL OR head_sha ~ '^[0-9a-f]{40}$'),
    exit_code        integer     NULL,
    result_sha256    text        NULL CHECK (result_sha256 IS NULL OR result_sha256 ~ '^[0-9a-f]{64}$'),
    duration_ms      bigint      NOT NULL DEFAULT 0 CHECK (duration_ms >= 0),
    -- An allowed call has no error and a result; a refused/failed one has an error and no result.
    CHECK ((outcome = 'allowed') = (error_code IS NULL)),
    CHECK ((outcome = 'allowed') = (result_sha256 IS NOT NULL)),
    -- Only transport-level refusals (no verified token) lack a run.
    CHECK (run_id IS NOT NULL OR (outcome = 'refused' AND tool = '(transport)'))
);
CREATE INDEX eng_tool_calls_run_idx ON eng_tool_calls (run_id, at, id);
CREATE INDEX eng_tool_calls_workflow_idx ON eng_tool_calls (workflow_id, at, id);

CREATE TRIGGER eng_tool_calls_append_only BEFORE UPDATE OR DELETE ON eng_tool_calls
    FOR EACH ROW EXECUTE FUNCTION eng_append_only();
CREATE TRIGGER eng_tool_calls_no_truncate BEFORE TRUNCATE ON eng_tool_calls
    FOR EACH STATEMENT EXECUTE FUNCTION eng_append_only();

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'a2a_mcp') THEN
        CREATE ROLE a2a_mcp NOLOGIN;
    END IF;
EXCEPTION
    WHEN duplicate_object OR unique_violation THEN NULL;
END
$$;

GRANT USAGE ON SCHEMA public TO a2a_mcp;
GRANT INSERT ON eng_tool_calls TO a2a_mcp;
GRANT SELECT ON eng_tool_calls TO a2a_app;

ALTER TABLE eng_tool_calls ENABLE ROW LEVEL SECURITY;
CREATE POLICY a2a_mcp_eng_tool_calls_insert ON eng_tool_calls FOR INSERT TO a2a_mcp WITH CHECK (true);
CREATE POLICY a2a_app_eng_tool_calls_read   ON eng_tool_calls FOR SELECT TO a2a_app USING (true);

DO $$
DECLARE
    r text;
BEGIN
    FOREACH r IN ARRAY ARRAY['anon', 'authenticated'] LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN
            EXECUTE format('REVOKE ALL ON eng_tool_calls FROM %I', r);
        END IF;
    END LOOP;
END
$$;
