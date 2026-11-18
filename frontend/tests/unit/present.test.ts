import assert from "node:assert/strict";
import { test } from "node:test";
import { deriveTimeline, describeFailure, detailSignature, statusSentence } from "../../src/features/workflows/present.ts";
import type { AgentRun, WorkflowDetail } from "../../src/features/workflows/schemas.ts";

const T0 = "2026-11-17T04:00:00Z";
const T1 = "2026-11-17T04:00:01Z";
const T5 = "2026-11-17T04:00:05Z";

function run(agent: "worker" | "reviewer", status: AgentRun["status"], error_code: string | null = null): AgentRun {
  return {
    id: crypto.randomUUID(),
    agent,
    status,
    a2a_task_id: "t",
    a2a_context_id: "c",
    a2a_state: null,
    error: null,
    error_code,
    started_at: T1,
    finished_at: status === "running" ? null : T5,
  };
}

function wf(p: Partial<WorkflowDetail>): WorkflowDetail {
  return {
    id: crypto.randomUUID(),
    question: "q",
    status: "running",
    verdict: null,
    error: null,
    error_code: null,
    failed_stage: null,
    created_at: T0,
    started_at: T1,
    finished_at: null,
    runs: [],
    draft: null,
    review: null,
    ...p,
  };
}

const states = (w: WorkflowDetail) => deriveTimeline(w).map((s) => s.state);

test("queued", () => {
  assert.deepEqual(states(wf({ status: "queued", started_at: null })), ["current", "upcoming", "upcoming", "upcoming"]);
});

test("running: worker in progress, then reviewer in progress", () => {
  assert.deepEqual(states(wf({ runs: [run("worker", "running")] })), ["complete", "current", "upcoming", "upcoming"]);
  assert.deepEqual(states(wf({ runs: [run("worker", "completed"), run("reviewer", "running")] })), ["complete", "complete", "current", "upcoming"]);
});

test("running before any run is recorded does not invent progress", () => {
  assert.deepEqual(states(wf({})), ["complete", "upcoming", "upcoming", "upcoming"]);
  assert.equal(statusSentence(wf({})), "Running. Contacting the agents.");
});

test("completed with needs_changes is complete, not failed", () => {
  const w = wf({ status: "completed", verdict: "needs_changes", finished_at: T5, runs: [run("worker", "completed"), run("reviewer", "completed")] });
  assert.deepEqual(states(w), ["complete", "complete", "complete", "complete"]);
  assert.equal(deriveTimeline(w)[3]!.label, "Completed");
  assert.match(deriveTimeline(w)[3]!.detail, /Changes requested · 4\.0 s total/);
});

test("failed at the reviewer keeps the worker complete", () => {
  const w = wf({ status: "failed", error: "x", error_code: "agent_unreachable", failed_stage: "reviewer", finished_at: T5, runs: [run("worker", "completed"), run("reviewer", "failed", "agent_unreachable")] });
  assert.deepEqual(states(w), ["complete", "complete", "failed", "failed"]);
  assert.equal(deriveTimeline(w)[3]!.label, "Failed");
  assert.equal(deriveTimeline(w)[2]!.detail, "An agent could not be reached");
});

test("failed before the worker run was recorded", () => {
  const w = wf({ status: "failed", error: "x", error_code: "agent_unreachable", failed_stage: "worker", finished_at: T5 });
  assert.deepEqual(states(w), ["complete", "failed", "skipped", "failed"]);
});

test("interrupted while queued", () => {
  const w = wf({ status: "failed", error: "x", error_code: "interrupted", started_at: null, finished_at: T5 });
  assert.deepEqual(states(w), ["complete", "skipped", "skipped", "failed"]);
  assert.match(describeFailure("interrupted").guidance, /not resumed automatically/);
});

test("unknown failure codes fall back to generic copy", () => {
  assert.equal(describeFailure("unclassified").title, "The workflow failed");
  assert.equal(describeFailure(null).title, "The workflow failed");
});

test("signature changes exactly when visible state changes", () => {
  const a = wf({ runs: [run("worker", "running")] });
  assert.equal(detailSignature(a), detailSignature({ ...a }));
  assert.notEqual(detailSignature(a), detailSignature({ ...a, runs: [run("worker", "completed")] }));
  assert.notEqual(detailSignature(a), detailSignature({ ...a, draft: { title: "t", summary: "s", content: "c", assumptions: [], mock: false } }));
});
