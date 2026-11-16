// Typed client for the orchestrator REST API (same origin).

export type WorkflowStatus = "queued" | "running" | "completed" | "failed";
export type Verdict = "pass" | "needs_changes";

export interface Workflow {
  id: string;
  question: string;
  status: WorkflowStatus;
  verdict: Verdict | null;
  error: string | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}

export interface AgentRun {
  id: string;
  agent: "worker" | "reviewer";
  status: "running" | "completed" | "failed";
  a2a_task_id: string | null;
  a2a_context_id: string | null;
  a2a_state: string | null;
  error: string | null;
  started_at: string;
  finished_at: string | null;
}

export interface Draft {
  title: string;
  summary: string;
  content: string;
  assumptions: string[];
}

export interface Finding {
  severity: "low" | "medium" | "high";
  title: string;
  detail: string;
}

export interface Review {
  verdict: Verdict;
  summary: string;
  findings: Finding[];
}

export interface WorkflowDetail extends Workflow {
  runs: AgentRun[];
  draft: Draft | null;
  review: Review | null;
}

export interface AgentInfo {
  role: string;
  reachable: boolean;
  name: string;
  version?: string;
  description?: string;
  error?: string;
}

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, { ...init, headers: { Accept: "application/json", ...init?.headers } });
  } catch {
    throw new ApiError(0, "network", "Cannot reach the orchestrator API.");
  }
  const body: unknown = await res.json().catch(() => null);
  if (!res.ok) {
    const err = (body as { error?: { code?: string; message?: string } } | null)?.error;
    throw new ApiError(res.status, err?.code ?? "http_error", err?.message ?? `Request failed (HTTP ${res.status}).`);
  }
  return body as T;
}

export const api = {
  agents: () => request<{ agents: AgentInfo[] }>("/api/agents"),
  list: (before?: string) =>
    request<{ items: Workflow[]; next_before: string | null }>(
      `/api/workflows?limit=25${before ? `&before=${encodeURIComponent(before)}` : ""}`,
    ),
  get: (id: string) => request<WorkflowDetail>(`/api/workflows/${encodeURIComponent(id)}`),
  create: (question: string, idempotencyKey: string) =>
    request<Workflow>("/api/workflows", {
      method: "POST",
      headers: { "Content-Type": "application/json", "Idempotency-Key": idempotencyKey },
      body: JSON.stringify({ question }),
    }),
};
