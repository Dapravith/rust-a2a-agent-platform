// Read-only `/api/eng` endpoints. There is deliberately no decision (approval)
// call here: human approval needs a real identity provider (Phase 6D), and the
// portal must never mint or forward an identity it did not verify.
import type { ApiClient } from "../../lib/api-client.ts";
import { engDetailSchema, engListSchema, engStatusSchema } from "./schemas.ts";

export function engApi(request: ApiClient) {
  return {
    list: async (limit = 25) =>
      (await request(`/api/eng/workflows?limit=${encodeURIComponent(String(limit))}`, engListSchema)).data.items,
    get: async (id: string) =>
      (await request(`/api/eng/workflows/${encodeURIComponent(id)}`, engDetailSchema)).data,
    status: async () => (await request("/api/eng/status", engStatusSchema)).data,
  };
}

export type EngApi = ReturnType<typeof engApi>;
