-- Phase 6B C5: new failure code `tool_evidence_invalid` (an agent cited MCP
-- tool calls that the MCP audit does not back). The CHECK lists are
-- replaced as a whole; existing rows keep validating.

ALTER TABLE eng_workflows DROP CONSTRAINT eng_workflows_error_code_check;
ALTER TABLE eng_workflows ADD CONSTRAINT eng_workflows_error_code_check CHECK (error_code IN (
    'interrupted', 'agent_unavailable', 'agent_timeout', 'agent_failed', 'artifact_invalid',
    'tool_not_permitted', 'policy_violation', 'candidate_mismatch', 'budget_exceeded',
    'provider_unconfigured', 'tool_evidence_invalid', 'internal_error'));

ALTER TABLE eng_agent_runs DROP CONSTRAINT eng_agent_runs_error_code_check;
ALTER TABLE eng_agent_runs ADD CONSTRAINT eng_agent_runs_error_code_check CHECK (error_code IN (
    'interrupted', 'agent_unavailable', 'agent_timeout', 'agent_failed', 'artifact_invalid',
    'tool_not_permitted', 'policy_violation', 'candidate_mismatch', 'budget_exceeded',
    'provider_unconfigured', 'tool_evidence_invalid', 'internal_error'));
