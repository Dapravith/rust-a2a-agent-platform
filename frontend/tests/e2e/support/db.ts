// Direct access to the scratch e2e database (created by scripts/e2e-stack.sh)
// to arrange states the deterministic mock agents cannot produce on demand:
// long or hostile agent output, specific failures, and a workflow that stays
// "running" so live updates can be observed step by step. The portal still
// reads everything through the real Rust API.
import { randomUUID } from "node:crypto";
import pg from "pg";

const URL = process.env.E2E_DATABASE_URL ?? `postgres://postgres:postgres@127.0.0.1:54329/${process.env.E2E_DB ?? "a2a_e2e"}`;
let pool: pg.Pool | undefined;

/** Runs one or more statements; returns the first column of the first row of the last result. */
export async function sql(query: string): Promise<string> {
  pool ??= new pg.Pool({ connectionString: URL, max: 2 });
  const res = await pool.query(query);
  const last = Array.isArray(res) ? res.at(-1) : res;
  const row = last?.rows?.[0];
  return row ? String(Object.values(row)[0]) : "";
}

/** SQL string literal. */
const lit = (s: string | null): string => (s === null ? "NULL" : `'${s.replaceAll("'", "''")}'`);
const json = (v: unknown): string => `${lit(JSON.stringify(v))}::jsonb`;

export interface DraftFixture {
  title: string;
  summary: string;
  content: string;
  assumptions: string[];
}
export interface ReviewFixture {
  verdict: "pass" | "needs_changes";
  summary: string;
  findings: { severity: "low" | "medium" | "high"; title: string; detail: string }[];
}

export const sampleDraft: DraftFixture = {
  title: "Validation rules for campaign creation",
  summary: "Validate every field at the API boundary and return structured 422 errors.",
  content: "## Fields\n\n- `title`: 3–120 characters\n- `goal_cents`: positive integer\n\n```rust\nfn validate(c: &Campaign) -> Result<(), Error> { Ok(()) }\n```",
  assumptions: ["Amounts are stored in minor units."],
};
export const sampleReview: ReviewFixture = {
  verdict: "needs_changes",
  summary: "Solid start, but currency handling and upper limits are missing.",
  findings: [
    { severity: "medium", title: "No currency field", detail: "Amounts without a currency are ambiguous." },
    { severity: "high", title: "No maximum goal", detail: "Unbounded goals allow overflow and abuse." },
  ],
};

export async function resetDatabase(): Promise<void> {
  await sql("DELETE FROM artifacts; DELETE FROM agent_runs; DELETE FROM workflows;");
}

export async function countWorkflows(question?: string): Promise<number> {
  return Number(await sql(`SELECT count(*) FROM workflows${question ? ` WHERE question = ${lit(question)}` : ""}`));
}

/** Waits until no workflow is queued/running (the backend allows one at a time). */
export async function waitForIdle(timeoutMs = 120_000): Promise<void> {
  const until = Date.now() + timeoutMs;
  while (Number(await sql("SELECT count(*) FROM workflows WHERE status IN ('queued','running')")) > 0) {
    if (Date.now() > until) throw new Error("a workflow is still active");
    await new Promise((r) => setTimeout(r, 250));
  }
}

function insertRun(workflowId: string, agent: "worker" | "reviewer", status: "running" | "completed" | "failed", errorCode?: string): [string, string] {
  const id = randomUUID();
  const done = status !== "running";
  return [id, `INSERT INTO agent_runs (id, workflow_id, agent, status, a2a_task_id, a2a_context_id, a2a_state, error, error_code, started_at, finished_at)
       VALUES ('${id}', '${workflowId}', '${agent}', '${status}', '${randomUUID()}', '${randomUUID()}',
               ${lit(status === "running" ? "TASK_STATE_WORKING" : status === "completed" ? "TASK_STATE_COMPLETED" : "TASK_STATE_FAILED")},
               ${status === "failed" ? lit("agent unreachable (connection refused)") : "NULL"}, ${lit(errorCode ?? null)},
               now() - interval '20 seconds', ${done ? "now() - interval '10 seconds'" : "NULL"});`];
}

function insertArtifact(workflowId: string, runId: string, kind: "draft" | "review", content: unknown): string {
  return `INSERT INTO artifacts (id, workflow_id, agent_run_id, kind, content) VALUES ('${randomUUID()}', '${workflowId}', '${runId}', '${kind}', ${json(content)});`;
}

type CompletedOpts = { question: string; draft?: DraftFixture; review?: ReviewFixture; createdSecondsAgo?: number };

function completedSql(opts: CompletedOpts): [string, string] {
  const id = randomUUID();
  const review = opts.review ?? sampleReview;
  const ago = opts.createdSecondsAgo ?? 60;
  const w = insertRun(id, "worker", "completed");
  const r = insertRun(id, "reviewer", "completed");
  return [id, `INSERT INTO workflows (id, question, status, verdict, created_at, started_at, finished_at)
       VALUES ('${id}', ${lit(opts.question)}, 'completed', '${review.verdict}',
               now() - interval '${ago} seconds', now() - interval '${ago - 1} seconds', now() - interval '${Math.max(ago - 9, 0)} seconds');
    ${w[1]} ${insertArtifact(id, w[0], "draft", opts.draft ?? sampleDraft)}
    ${r[1]} ${insertArtifact(id, r[0], "review", review)}`];
}

export async function seedCompleted(opts: CompletedOpts): Promise<string> {
  const [id, q] = completedSql(opts);
  await sql(q);
  return id;
}

/** Many completed workflows in one round trip. */
export async function seedManyCompleted(all: CompletedOpts[]): Promise<string[]> {
  const parts = all.map(completedSql);
  await sql(parts.map(([, q]) => q).join("\n"));
  return parts.map(([id]) => id);
}

/** Failed at the Reviewer step with the Worker draft kept (as the engine does). */
export async function seedFailedAtReviewer(question: string, code = "agent_unreachable"): Promise<string> {
  const id = randomUUID();
  const w = insertRun(id, "worker", "completed");
  const r = insertRun(id, "reviewer", "failed", code);
  await sql(`INSERT INTO workflows (id, question, status, error, error_code, failed_stage, created_at, started_at, finished_at)
       VALUES ('${id}', ${lit(question)}, 'failed', 'agent unreachable: reviewer', '${code}', 'reviewer',
               now() - interval '30 seconds', now() - interval '29 seconds', now() - interval '5 seconds');
       ${w[1]} ${insertArtifact(id, w[0], "draft", sampleDraft)} ${r[1]}`);
  return id;
}

/** A workflow the engine is not executing, advanced step by step by the test. */
export async function seedRunning(question: string) {
  const id = randomUUID();
  const [worker, workerSql] = insertRun(id, "worker", "running");
  await sql(`INSERT INTO workflows (id, question, status, created_at, started_at) VALUES ('${id}', ${lit(question)}, 'running', now() - interval '3 seconds', now() - interval '2 seconds'); ${workerSql}`);
  return {
    id,
    async finishWorker() {
      await sql(`UPDATE agent_runs SET status = 'completed', a2a_state = 'TASK_STATE_COMPLETED', finished_at = now() WHERE id = '${worker}'; ${insertArtifact(id, worker, "draft", sampleDraft)}`);
    },
    async startReviewer(): Promise<string> {
      const [run, q] = insertRun(id, "reviewer", "running");
      await sql(q);
      return run;
    },
    async finishReviewer(reviewerRun: string, review: ReviewFixture = sampleReview) {
      await sql(`BEGIN;
        UPDATE agent_runs SET status = 'completed', a2a_state = 'TASK_STATE_COMPLETED', finished_at = now() WHERE id = '${reviewerRun}';
        ${insertArtifact(id, reviewerRun, "review", review)}
        UPDATE workflows SET status = 'completed', verdict = '${review.verdict}', finished_at = now() WHERE id = '${id}';
        COMMIT;`);
    },
    /** Drives the workflow to a terminal state so later tests can submit. */
    async complete() {
      await this.finishWorker();
      await this.finishReviewer(await this.startReviewer());
    },
  };
}

export async function closeDb(): Promise<void> {
  await pool?.end();
  pool = undefined;
}
