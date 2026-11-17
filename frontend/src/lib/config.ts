// Server-only configuration. `astro:env/server` values are read at runtime from
// the process environment and can never be imported into browser code.
import { API_ORIGIN, API_TIMEOUT_MS } from "astro:env/server";
import { createApiClient } from "./api-client.ts";
import { workflowsApi } from "../features/workflows/api.ts";

export const api = workflowsApi(createApiClient({ origin: API_ORIGIN, timeoutMs: API_TIMEOUT_MS }));
