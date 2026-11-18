-- Phase 6B B2/B4.
--
-- 1. eng_tool_calls.patch_sha256: for repository.diff and workspace.apply_patch,
--    the SHA-256 of the full candidate diff *as the MCP server computed it*.
--    The orchestrator compares the digest an agent reports with this one.
-- 2. eng_workspaces: which MCP workspace belongs to which workflow. The id is
--    chosen by the MCP server (POST /control/workspaces); the orchestrator only
--    records it. One workspace per workflow; append-only.

ALTER TABLE eng_tool_calls
    ADD COLUMN patch_sha256 text NULL CHECK (patch_sha256 IS NULL OR patch_sha256 ~ '^[0-9a-f]{64}$');

CREATE TABLE eng_workspaces (
    workflow_id  uuid        PRIMARY KEY REFERENCES eng_workflows (id),
    workspace_id uuid        NOT NULL UNIQUE,
    base_sha     text        NOT NULL CHECK (base_sha ~ '^[0-9a-f]{40}$'),
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER eng_workspaces_append_only BEFORE UPDATE OR DELETE ON eng_workspaces
    FOR EACH ROW EXECUTE FUNCTION eng_append_only();
CREATE TRIGGER eng_workspaces_no_truncate BEFORE TRUNCATE ON eng_workspaces
    FOR EACH STATEMENT EXECUTE FUNCTION eng_append_only();

GRANT SELECT, INSERT ON eng_workspaces TO a2a_app;
ALTER TABLE eng_workspaces ENABLE ROW LEVEL SECURITY;
CREATE POLICY a2a_app_eng_workspaces ON eng_workspaces FOR ALL TO a2a_app USING (true) WITH CHECK (true);

DO $$
DECLARE
    r text;
BEGIN
    FOREACH r IN ARRAY ARRAY['anon', 'authenticated'] LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN
            EXECUTE format('REVOKE ALL ON eng_workspaces FROM %I', r);
        END IF;
    END LOOP;
END
$$;
