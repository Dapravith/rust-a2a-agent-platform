// The Rust REST endpoints this portal uses. Paths and payloads mirror
// backend/apps/api/src/routes.rs; nothing else is assumed to exist.
import type { ApiClient } from "../../lib/api-client.ts";
import { agentsSchema, workflowDetailSchema, workflowPageSchema, workflowSchema } from "./schemas.ts";

export const PAGE_SIZE = 25;

/** Cursor values come straight from `next_before`; keep full precision (µs). */
const CURSOR = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,9})?(Z|[+-]\d{2}:\d{2})$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export const isCursor = (value: string): boolean => CURSOR.test(value);
export const isWorkflowId = (value: string): boolean => UUID.test(value);

export function workflowsApi(request: ApiClient) {
  return {
    /** `GET /api/workflows?limit=&before=`: newest first. */
    list: async (opts: { limit?: number; before?: string } = {}) => {
      const params = new URLSearchParams({ limit: String(opts.limit ?? PAGE_SIZE) });
      if (opts.before) params.set("before", opts.before);
      return (await request(`/api/workflows?${params}`, workflowPageSchema)).data;
    },

    /** `GET /api/workflows/{id}`: workflow, agent runs, draft and review. */
    get: async (id: string) =>
      (await request(`/api/workflows/${encodeURIComponent(id)}`, workflowDetailSchema)).data,

    /**
     * `POST /api/workflows`. 202 = created, 200 = same Idempotency-Key replayed
     * (the existing workflow is returned, nothing new runs).
     */
    create: async (question: string, idempotencyKey: string) => {
      const res = await request("/api/workflows", workflowSchema, {
        method: "POST",
        headers: { "Content-Type": "application/json", "Idempotency-Key": idempotencyKey },
        body: JSON.stringify({ question }),
      });
      return { workflow: res.data, replayed: res.status === 200 };
    },

    /** `GET /api/agents`: Agent Card discovery status of both agents. */
    agents: async () => (await request("/api/agents", agentsSchema)).data.agents,
  };
}

export type WorkflowsApi = ReturnType<typeof workflowsApi>;
