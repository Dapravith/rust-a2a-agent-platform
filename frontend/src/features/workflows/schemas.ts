// Runtime schemas for the Rust REST API responses (backend/apps/api/src/routes.rs,
// db.rs and crates/contracts). Every response is validated before rendering, so a
// malformed or unexpected payload becomes an `invalid_response` error instead of a
// broken page. Unknown extra fields are ignored for forward compatibility; known
// fields are strict. Rust remains the authority on every rule: nothing here
// decides workflow behaviour.
import { z } from "astro/zod";

export const workflowStatus = z.enum(["queued", "running", "completed", "failed"]);
export const verdict = z.enum(["pass", "needs_changes"]);
export const severity = z.enum(["low", "medium", "high"]);
export const agentRole = z.enum(["worker", "reviewer"]);
export const runStatus = z.enum(["running", "completed", "failed"]);

const timestamp = z.string().refine((s) => !Number.isNaN(Date.parse(s)), "invalid timestamp");

export const workflowSchema = z.object({
  id: z.uuid(),
  question: z.string(),
  status: workflowStatus,
  verdict: verdict.nullable(),
  error: z.string().nullable(),
  // Stable codes from a closed Rust enum. Kept as strings so a code added later
  // degrades to a generic description instead of failing the whole page.
  error_code: z.string().nullable().optional().default(null),
  failed_stage: agentRole.nullable().optional().default(null),
  created_at: timestamp,
  started_at: timestamp.nullable(),
  finished_at: timestamp.nullable(),
});

export const agentRunSchema = z.object({
  id: z.uuid(),
  agent: agentRole,
  status: runStatus,
  a2a_task_id: z.string().nullable(),
  a2a_context_id: z.string().nullable(),
  a2a_state: z.string().nullable(),
  error: z.string().nullable(),
  error_code: z.string().nullable().optional().default(null),
  started_at: timestamp,
  finished_at: timestamp.nullable(),
});

export const draftSchema = z.object({
  title: z.string(),
  summary: z.string(),
  content: z.string(),
  assumptions: z.array(z.string()),
  // Set by the Worker in mock mode (no model called). Absent on real and older drafts.
  mock: z.boolean().optional().default(false),
});

export const findingSchema = z.object({
  severity,
  title: z.string(),
  detail: z.string(),
});

export const reviewSchema = z.object({
  verdict,
  summary: z.string(),
  findings: z.array(findingSchema),
});

export const workflowDetailSchema = workflowSchema.extend({
  runs: z.array(agentRunSchema),
  draft: draftSchema.nullable(),
  review: reviewSchema.nullable(),
});

export const workflowPageSchema = z.object({
  items: z.array(workflowSchema),
  next_before: timestamp.nullable(),
});

export const agentInfoSchema = z.object({
  role: agentRole,
  reachable: z.boolean(),
  name: z.string(),
  version: z.string().optional(),
  description: z.string().optional(),
  error: z.string().optional(),
});

export const agentsSchema = z.object({ agents: z.array(agentInfoSchema) });

export const errorEnvelopeSchema = z.object({
  error: z.object({ code: z.string(), message: z.string() }),
});

export type WorkflowStatus = z.infer<typeof workflowStatus>;
export type Verdict = z.infer<typeof verdict>;
export type Severity = z.infer<typeof severity>;
export type AgentRole = z.infer<typeof agentRole>;
export type Workflow = z.infer<typeof workflowSchema>;
export type AgentRun = z.infer<typeof agentRunSchema>;
export type Draft = z.infer<typeof draftSchema>;
export type Finding = z.infer<typeof findingSchema>;
export type Review = z.infer<typeof reviewSchema>;
export type WorkflowDetail = z.infer<typeof workflowDetailSchema>;
export type WorkflowPage = z.infer<typeof workflowPageSchema>;
export type AgentInfo = z.infer<typeof agentInfoSchema>;

export const isActive = (status: WorkflowStatus): boolean => status === "queued" || status === "running";
