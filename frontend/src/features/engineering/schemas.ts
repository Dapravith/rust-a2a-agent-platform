// Runtime schemas for `/api/eng` (backend/apps/api/src/eng/routes.rs, store.rs).
// Read-only views: the portal renders what the Rust orchestrator recorded and
// decides nothing. Unknown extra fields are ignored; known fields are strict.
import { z } from "astro/zod";

const timestamp = z.string().refine((s) => !Number.isNaN(Date.parse(s)), "invalid timestamp");
const sha = z.string().regex(/^[0-9a-f]{40}$/);

export const engWorkflowSchema = z.object({
  id: z.uuid(),
  title: z.string(),
  requirements: z.string(),
  requested_by: z.string(),
  status: z.string(),
  version: z.number().int(),
  agent_runs: z.number().int(),
  max_agent_runs: z.number().int(),
  candidate_commit_sha: sha.nullable(),
  pending_decision_hash: z.string().nullable(),
  error_code: z.string().nullable(),
  error: z.string().nullable(),
  created_at: timestamp,
  updated_at: timestamp,
  finished_at: timestamp.nullable(),
});

export const engRunSchema = z.object({
  id: z.uuid(),
  role: z.string(),
  attempt: z.number().int(),
  agent_id: z.string(),
  provider: z.string(),
  model: z.string().nullable(),
  correlation_id: z.uuid(),
  status: z.string(),
  a2a_task_id: z.string().nullable(),
  input_tokens: z.number().int().nullable(),
  output_tokens: z.number().int().nullable(),
  error_code: z.string().nullable(),
  error: z.string().nullable(),
  started_at: timestamp,
  finished_at: timestamp.nullable(),
});

export const engArtifactSchema = z.object({
  id: z.uuid(),
  agent_run_id: z.uuid().nullable(),
  kind: z.string(),
  schema_version: z.number().int(),
  producer_kind: z.string(),
  producer_id: z.string(),
  provider: z.string().nullable(),
  model: z.string().nullable(),
  content: z.unknown(),
  content_hash: z.string(),
  commit_sha: sha.nullable(),
  created_at: timestamp,
});

export const toolCallSchema = z.object({
  id: z.uuid(),
  at: timestamp,
  run_id: z.uuid().nullable(),
  role: z.string().nullable(),
  agent_id: z.string().nullable(),
  tool: z.string(),
  target: z.string().nullable(),
  outcome: z.enum(["allowed", "refused", "failed"]),
  error_code: z.string().nullable(),
  head_sha: sha.nullable(),
  patch_sha256: z.string().nullable(),
  exit_code: z.number().int().nullable(),
  duration_ms: z.number().int(),
});

export const gateSchema = z.object({ gate: z.string(), passed: z.boolean(), detail: z.string() });

export const approvalSchema = z.object({
  id: z.uuid(),
  gate: z.string(),
  decision: z.string(),
  approver_id: z.string(),
  approver_role: z.string(),
  identity_issuer: z.string(),
  comment: z.string().nullable(),
  reviewed_commit_sha: z.string().nullable(),
  decided_at: timestamp,
});

export const auditEventSchema = z.object({
  id: z.uuid(),
  at: timestamp,
  actor_kind: z.string(),
  actor_id: z.string(),
  action: z.string(),
  from_status: z.string().nullable(),
  to_status: z.string().nullable(),
});

export const engDetailSchema = z.object({
  workflow: engWorkflowSchema,
  awaiting_decision: z.string().nullable().optional().default(null),
  gates: z.array(gateSchema).nullable(),
  runs: z.array(engRunSchema),
  artifacts: z.array(engArtifactSchema),
  approvals: z.array(approvalSchema),
  audit: z.array(auditEventSchema),
  tool_calls: z.array(toolCallSchema).optional().default([]),
  workspace_id: z.uuid().nullable().optional().default(null),
});

export const engListSchema = z.object({ items: z.array(engWorkflowSchema) });

export const engStatusSchema = z.object({
  approvals_enabled: z.boolean(),
  providers: z.array(
    z.object({ role: z.string(), agent_id: z.string(), provider: z.string(), model: z.string().nullable() }),
  ),
});

export type EngWorkflow = z.infer<typeof engWorkflowSchema>;
export type EngRun = z.infer<typeof engRunSchema>;
export type EngArtifact = z.infer<typeof engArtifactSchema>;
export type ToolCall = z.infer<typeof toolCallSchema>;
export type EngDetail = z.infer<typeof engDetailSchema>;
export type EngStatus = z.infer<typeof engStatusSchema>;
