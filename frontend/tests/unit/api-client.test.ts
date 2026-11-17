import assert from "node:assert/strict";
import { test } from "node:test";
import { createApiClient } from "../../src/lib/api-client.ts";
import { ApiError, describeLoadError, fieldMessage } from "../../src/lib/errors.ts";
import { isCursor, isWorkflowId, workflowsApi } from "../../src/features/workflows/api.ts";

const WF = {
  id: "01a1e7b4-b138-72b0-8601-07f81a24badf",
  question: "How should a Rust service validate pagination parameters?",
  status: "completed",
  verdict: "needs_changes",
  error: null,
  error_code: null,
  failed_stage: null,
  created_at: "2026-11-17T04:12:20.123456Z",
  started_at: "2026-11-17T04:12:20.2Z",
  finished_at: "2026-11-17T04:12:24.857115Z",
};

function fakeFetch(status: number, body: unknown, seen: Request[] = []): typeof fetch {
  return (async (input: URL | RequestInfo, init?: RequestInit) => {
    seen.push(new Request(input, init));
    return new Response(typeof body === "string" ? body : JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
}

const client = (f: typeof fetch) => workflowsApi(createApiClient({ origin: "http://api.test", timeoutMs: 1000, fetch: f }));

test("list: parses the page and keeps the microsecond cursor verbatim", async () => {
  const seen: Request[] = [];
  const page = await client(fakeFetch(200, { items: [WF], next_before: WF.created_at }, seen)).list({ before: WF.created_at });
  assert.equal(page.items[0]!.status, "completed");
  assert.equal(page.next_before, "2026-11-17T04:12:20.123456Z");
  assert.equal(new URL(seen[0]!.url).searchParams.get("before"), "2026-11-17T04:12:20.123456Z");
  assert.equal(seen[0]!.redirect, "error");
});

test("create: sends the Idempotency-Key and reports a replay (200)", async () => {
  const seen: Request[] = [];
  const res = await client(fakeFetch(200, WF, seen)).create("question text", "key-1");
  assert.equal(res.replayed, true);
  assert.equal(seen[0]!.method, "POST");
  assert.equal(seen[0]!.headers.get("idempotency-key"), "key-1");
  assert.deepEqual(await seen[0]!.json(), { question: "question text" });
  assert.equal((await client(fakeFetch(202, WF)).create("q", "k")).replayed, false);
});

test("error envelope becomes a typed ApiError with the Rust code", async () => {
  await assert.rejects(
    client(fakeFetch(409, { error: { code: "workflow_active", message: "another workflow is already queued or running" } })).create("q", "k"),
    (e: unknown) => e instanceof ApiError && e.kind === "http" && e.status === 409 && e.code === "workflow_active",
  );
});

test("non-JSON error bodies still produce a safe error", async () => {
  await assert.rejects(client(fakeFetch(502, "<html>bad gateway</html>")).list(), (e: unknown) => e instanceof ApiError && e.code === "http_error" && e.uncertain);
});

test("malformed 2xx responses are rejected, not rendered", async () => {
  for (const bad of [{ ...WF, status: "exploded" }, { ...WF, id: "not-a-uuid" }, { ...WF, created_at: "yesterday" }, null, "[]"]) {
    await assert.rejects(client(fakeFetch(200, { items: [bad], next_before: null })).list(), (e: unknown) => e instanceof ApiError && e.kind === "invalid_response");
  }
  const detail = { ...WF, runs: [], draft: { title: "t", summary: "s", content: 5, assumptions: [] }, review: null };
  await assert.rejects(client(fakeFetch(200, detail)).get(WF.id), (e: unknown) => e instanceof ApiError && e.kind === "invalid_response");
});

test("unknown error codes and extra fields degrade gracefully", async () => {
  const detail = { ...WF, status: "failed", verdict: null, error: "x", error_code: "brand_new_code", runs: [], draft: null, review: null, extra: 1 };
  const w = await client(fakeFetch(200, detail)).get(WF.id);
  assert.equal(w.error_code, "brand_new_code");
});

test("network failure and timeout are distinguished", async () => {
  const refused = (async () => {
    throw new TypeError("fetch failed");
  }) as unknown as typeof fetch;
  await assert.rejects(client(refused).list(), (e: unknown) => e instanceof ApiError && e.kind === "network" && !e.uncertain);
  const hang = ((_u: unknown, init?: RequestInit) =>
    new Promise((_r, reject) => init?.signal?.addEventListener("abort", () => reject(init.signal!.reason)))) as typeof fetch;
  const slow = workflowsApi(createApiClient({ origin: "http://api.test", timeoutMs: 20, fetch: hang }));
  await assert.rejects(slow.list(), (e: unknown) => e instanceof ApiError && e.kind === "timeout" && e.uncertain);
});

test("only /api paths can be requested", async () => {
  const raw = createApiClient({ origin: "http://api.test", timeoutMs: 1000, fetch: fakeFetch(200, {}) });
  const { z } = await import("astro/zod");
  await assert.rejects(raw("//evil.example/x", z.any()), /refusing non-API path/);
});

test("input guards", () => {
  assert.ok(isCursor("2026-11-17T04:12:20.123456Z"));
  assert.ok(isCursor("2026-11-17T04:12:20+00:00"));
  assert.ok(!isCursor("';DROP"));
  assert.ok(isWorkflowId(WF.id));
  assert.ok(!isWorkflowId("../../etc"));
});

test("user-facing copy", () => {
  assert.equal(fieldMessage("invalid field `question`: must contain at least 10 non-whitespace characters"), "Must contain at least 10 non-whitespace characters.");
  assert.match(describeLoadError(new ApiError("network", 0, "network", ""), "history").title, /unreachable/);
  assert.match(describeLoadError(new Error("boom"), "history").detail, /unexpected/);
});
