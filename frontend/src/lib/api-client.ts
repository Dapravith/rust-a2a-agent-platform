// Server-side HTTP transport to the Rust orchestrator API. Used only from Astro
// pages and endpoints during server rendering; the browser never calls Rust.
// It adds a timeout, refuses redirects, bounds the body size, decodes the
// `{"error": {code, message}}` envelope and validates every 2xx body.
import { z } from "astro/zod";
import { ApiError } from "./errors.ts";

/** `{"error": {code, message}}`: the error body of every Rust API endpoint. */
const errorEnvelopeSchema = z.object({
  error: z.object({ code: z.string(), message: z.string() }),
});

export interface ApiClientOptions {
  /** Base origin of the Rust API, e.g. `http://127.0.0.1:8080`. */
  origin: string;
  timeoutMs: number;
  fetch?: typeof fetch;
}

export interface ApiResponse<T> {
  status: number;
  data: T;
}

export type ApiClient = <S extends z.ZodType>(
  path: string,
  schema: S,
  init?: { method?: "GET" | "POST"; headers?: Record<string, string>; body?: string },
) => Promise<ApiResponse<z.output<S>>>;

/** Largest body accepted from the API (the biggest real payload is ~100 KB). */
const MAX_BODY_BYTES = 2 * 1024 * 1024;

export function createApiClient(options: ApiClientOptions): ApiClient {
  const fetchImpl = options.fetch ?? fetch;
  const base = new URL(options.origin);

  return async (path, schema, init = {}) => {
    if (!path.startsWith("/api/")) throw new Error(`refusing non-API path: ${path}`);
    const url = new URL(path, base);
    let res: Response;
    try {
      res = await fetchImpl(url, {
        method: init.method ?? "GET",
        body: init.body,
        headers: { Accept: "application/json", ...init.headers },
        redirect: "error",
        signal: AbortSignal.timeout(options.timeoutMs),
      });
    } catch (e) {
      const name = e instanceof Error ? e.name : "";
      if (name === "TimeoutError" || name === "AbortError") {
        throw new ApiError("timeout", 0, "timeout", `No response from the API within ${options.timeoutMs} ms.`);
      }
      throw new ApiError("network", 0, "network", "The API could not be reached.");
    }

    let text: string;
    try {
      text = await res.text();
    } catch {
      throw new ApiError("network", res.status, "network", "The API connection closed while reading the response.");
    }
    if (text.length > MAX_BODY_BYTES) {
      throw new ApiError("invalid_response", res.status, "invalid_response", "The API response was too large.");
    }
    let body: unknown = undefined;
    try {
      body = text ? JSON.parse(text) : undefined;
    } catch {
      body = undefined;
    }

    if (!res.ok) {
      const envelope = errorEnvelopeSchema.safeParse(body);
      if (envelope.success) {
        throw new ApiError("http", res.status, envelope.data.error.code, envelope.data.error.message);
      }
      throw new ApiError("http", res.status, "http_error", `The API responded with HTTP ${res.status}.`);
    }

    const parsed = schema.safeParse(body);
    if (!parsed.success) {
      // Field paths only; never log the payload (it contains user and agent text).
      const where = parsed.error.issues.map((i) => i.path.join(".") || "(root)").join(", ");
      console.error(`[api] invalid response for ${init.method ?? "GET"} ${url.pathname}: ${where}`);
      throw new ApiError("invalid_response", res.status, "invalid_response", "The API response did not match the expected format.");
    }
    return { status: res.status, data: parsed.data };
  };
}
