import assert from "node:assert/strict";
import { test } from "node:test";

import { callsByRun, engStatus, providerLabel, roleLabel, tokens, toolTone } from "../../src/features/engineering/present.ts";
import { engDetailSchema } from "../../src/features/engineering/schemas.ts";

test("rule-based, simulated and test-double runs are never labelled as models", () => {
  for (const provider of ["deterministic", "simulation", "test-double"]) {
    assert.equal(providerLabel({ provider, model: null }).isModel, false, provider);
  }
  assert.deepEqual(providerLabel({ provider: "anthropic-messages", model: "claude-opus-5-5" }), {
    label: "anthropic-messages · claude-opus-5-5",
    isModel: true,
  });
});

test("statuses map to labels; unknown statuses degrade instead of failing", () => {
  assert.equal(engStatus("awaiting_delivery_approval").tone, "warning");
  assert.equal(engStatus("failed").tone, "error");
  assert.deepEqual(engStatus("quality_assessment"), { label: "Quality assessment", tone: "info" });
  assert.deepEqual(engStatus("some_future_state"), { label: "some future state", tone: "neutral" });
  assert.equal(roleLabel("qa_tester"), "QA Tester");
  assert.equal(roleLabel(null), "—");
});

test("tool calls: refused/failed/non-zero exits are not shown as success; grouped by run", () => {
  assert.equal(toolTone({ outcome: "allowed", exit_code: 0 }), "success");
  assert.equal(toolTone({ outcome: "allowed", exit_code: 101 }), "error");
  assert.equal(toolTone({ outcome: "refused", exit_code: null }), "warning");
  assert.equal(toolTone({ outcome: "failed", exit_code: null }), "error");
  const call = (run_id: string | null) =>
    ({ run_id, outcome: "allowed", exit_code: null }) as unknown as Parameters<typeof callsByRun>[0][number];
  const m = callsByRun([call("r1"), call("r1"), call("r2"), call(null)]);
  assert.equal(m.get("r1")?.length, 2);
  assert.equal(m.size, 2);
  assert.equal(tokens({ input_tokens: null, output_tokens: null }), null);
  assert.equal(tokens({ input_tokens: 10, output_tokens: null }), "10 in / ? out");
});

const wf = {
  id: "0190a000-0000-7000-8000-000000000001",
  title: "t",
  requirements: "r",
  requested_by: "bob",
  status: "awaiting_delivery_approval",
  version: 7,
  agent_runs: 6,
  max_agent_runs: 24,
  candidate_commit_sha: "a".repeat(40),
  pending_decision_hash: "b".repeat(64),
  error_code: null,
  error: null,
  created_at: "2026-11-18T10:00:00Z",
  updated_at: "2026-11-18T10:01:00Z",
  finished_at: null,
};

test("detail parses with and without the Phase 6B fields; bad tool outcomes are rejected", () => {
  const base = { workflow: wf, gates: null, runs: [], artifacts: [], approvals: [], audit: [] };
  const old = engDetailSchema.parse(base);
  assert.deepEqual(old.tool_calls, []);
  assert.equal(old.workspace_id, null);
  const call = {
    id: "0190a000-0000-7000-8000-000000000002",
    at: "2026-11-18T10:00:30Z",
    run_id: null,
    role: "developer",
    agent_id: "a",
    tool: "workspace.apply_patch",
    target: "src/lib.rs",
    outcome: "allowed",
    error_code: null,
    head_sha: "c".repeat(40),
    patch_sha256: "d".repeat(64),
    exit_code: null,
    duration_ms: 5,
  };
  assert.equal(engDetailSchema.parse({ ...base, tool_calls: [call] }).tool_calls.length, 1);
  assert.equal(engDetailSchema.safeParse({ ...base, tool_calls: [{ ...call, outcome: "approved" }] }).success, false);
});
