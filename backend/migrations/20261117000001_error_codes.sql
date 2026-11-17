-- Phase 3: stable, machine-readable failure codes and the stage that failed.
-- `error` stays the human-readable (client-safe) message; `error_code` is the
-- contract clients and alerts should key on. Adding a code needs a migration.

ALTER TABLE workflows
    ADD COLUMN error_code   text NULL,
    ADD COLUMN failed_stage text NULL CHECK (failed_stage IN ('worker', 'reviewer'));

ALTER TABLE agent_runs
    ADD COLUMN error_code text NULL;

-- Backfill rows written before this migration.
UPDATE workflows SET error_code = CASE
        WHEN error LIKE 'Interrupted by an orchestrator restart%' THEN 'interrupted'
        ELSE 'unclassified'
    END
    WHERE status = 'failed';
UPDATE workflows w SET failed_stage = r.agent
    FROM agent_runs r
    WHERE w.status = 'failed' AND r.workflow_id = w.id AND r.status = 'failed';
UPDATE agent_runs SET error_code = CASE
        WHEN error LIKE 'Interrupted by an orchestrator restart%' THEN 'interrupted'
        ELSE 'unclassified'
    END
    WHERE status = 'failed';

-- Keep in sync with `api::error_code::ErrorCode` (a test checks every variant).
ALTER TABLE workflows ADD CONSTRAINT workflows_error_code_known CHECK (error_code IN (
    'agent_unreachable', 'agent_card_invalid', 'agent_untrusted', 'agent_protocol_error',
    'agent_timeout', 'agent_rejected', 'agent_canceled', 'agent_failed',
    'agent_unsupported_interaction', 'artifact_invalid',
    'llm_timeout', 'llm_rate_limited', 'llm_provider_error', 'llm_unavailable',
    'llm_refused', 'llm_output_truncated', 'llm_output_invalid',
    'persistence_failed', 'internal_error', 'interrupted', 'unclassified'));
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_error_code_known CHECK (error_code IN (
    'agent_unreachable', 'agent_card_invalid', 'agent_untrusted', 'agent_protocol_error',
    'agent_timeout', 'agent_rejected', 'agent_canceled', 'agent_failed',
    'agent_unsupported_interaction', 'artifact_invalid',
    'llm_timeout', 'llm_rate_limited', 'llm_provider_error', 'llm_unavailable',
    'llm_refused', 'llm_output_truncated', 'llm_output_invalid',
    'persistence_failed', 'internal_error', 'interrupted', 'unclassified'));

-- A failed row always carries a code; a non-failed row never does.
ALTER TABLE workflows ADD CONSTRAINT workflows_error_code_iff_failed
    CHECK ((status = 'failed') = (error_code IS NOT NULL));
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_error_code_iff_failed
    CHECK ((status = 'failed') = (error_code IS NOT NULL));

-- Terminal timestamps are consistent with status.
ALTER TABLE workflows ADD CONSTRAINT workflows_finished_iff_terminal
    CHECK ((status IN ('completed', 'failed')) = (finished_at IS NOT NULL));
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_finished_iff_terminal
    CHECK ((status IN ('completed', 'failed')) = (finished_at IS NOT NULL));

-- Failure analysis: "which workflows failed, and why" without a scan.
CREATE INDEX workflows_failed_idx ON workflows (error_code, created_at DESC)
    WHERE status = 'failed';
