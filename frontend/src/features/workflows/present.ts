// Presentation model for a workflow: labels, the execution timeline and failure
// guidance. Everything is derived from fields Rust returns (status, runs,
// timestamps, error_code, failed_stage); no progress is estimated or invented.
import type { Tone } from "../../components/ui/tone.ts";
import { formatDuration } from "../../lib/format.ts";
import type { AgentRole, AgentRun, Severity, WorkflowDetail, WorkflowStatus, Workflow } from "./schemas.ts";

export const STATUS_LABEL: Record<WorkflowStatus, string> = {
  queued: "Queued",
  running: "Running",
  completed: "Completed",
  failed: "Failed",
};

export const STATUS_TONE: Record<WorkflowStatus, Tone> = {
  queued: "neutral",
  running: "info",
  completed: "success",
  failed: "error",
};

export const VERDICT_LABEL = { pass: "Passed", needs_changes: "Changes requested" } as const;
export const VERDICT_TONE = { pass: "success", needs_changes: "warning" } as const satisfies Record<string, Tone>;

export const SEVERITY_LABEL: Record<Severity, string> = { high: "High", medium: "Medium", low: "Low" };
export const SEVERITY_TONE: Record<Severity, Tone> = { high: "error", medium: "warning", low: "neutral" };
export const SEVERITY_ORDER: Record<Severity, number> = { high: 0, medium: 1, low: 2 };

export const AGENT_LABEL: Record<AgentRole, string> = { worker: "Worker", reviewer: "Reviewer" };

// Failure guidance, keyed on the stable `error_code` values from
// backend/apps/api/src/error_code.rs. Unknown codes fall back to a generic text.

export interface FailureCopy {
  title: string;
  guidance: string;
}

const RETRY = "Submit the task again from the dashboard.";

const FAILURES: Record<string, FailureCopy> = {
  agent_unreachable: {
    title: "An agent could not be reached",
    guidance: `Check that the Worker and Reviewer agent processes are running, then ${RETRY.toLowerCase()}`,
  },
  agent_card_invalid: {
    title: "The agent returned an invalid Agent Card",
    guidance: "The agent answered, but its Agent Card could not be read. An operator needs to check the agent deployment.",
  },
  agent_untrusted: {
    title: "The agent's identity did not match the trusted configuration",
    guidance: "The orchestrator refused to talk to it. An operator needs to check the configured agent URLs and names.",
  },
  agent_protocol_error: {
    title: "The agent sent an unexpected A2A response",
    guidance: `This is usually a version mismatch between the orchestrator and the agent. ${RETRY}`,
  },
  agent_unsupported_interaction: {
    title: "The agent asked for an interaction this workflow does not support",
    guidance: "The agent requested further input, which the fixed Worker → Reviewer flow cannot provide. Rephrase the task so it can be answered in one step.",
  },
  agent_timeout: {
    title: "The agent did not finish in time",
    guidance: `The task was canceled at the deadline. ${RETRY} If it keeps timing out, narrow the question.`,
  },
  agent_rejected: {
    title: "The agent rejected the request",
    guidance: "The agent considered its input invalid. Rephrase the task and submit it again.",
  },
  agent_canceled: { title: "The agent task was canceled", guidance: RETRY },
  agent_failed: { title: "The agent reported a failure", guidance: RETRY },
  artifact_invalid: {
    title: "The agent's result did not pass validation",
    guidance: `The orchestrator rejected a malformed result rather than store it. ${RETRY}`,
  },
  llm_timeout: { title: "The language model timed out", guidance: RETRY },
  llm_rate_limited: {
    title: "The language model provider is rate limiting requests",
    guidance: "Wait a minute before submitting the task again.",
  },
  llm_provider_error: { title: "The language model provider returned an error", guidance: `Wait a moment, then ${RETRY.toLowerCase()}` },
  llm_unavailable: { title: "The language model provider is unavailable", guidance: `Wait a moment, then ${RETRY.toLowerCase()}` },
  llm_refused: {
    title: "The language model declined to answer",
    guidance: "Rephrase the task as a software engineering question and submit it again.",
  },
  llm_output_truncated: {
    title: "The model's answer was cut off",
    guidance: "The answer exceeded the output limit. Narrow the question and submit it again.",
  },
  llm_output_invalid: {
    title: "The model produced an answer in an invalid format",
    guidance: RETRY,
  },
  persistence_failed: {
    title: "The result could not be saved",
    guidance: `The database was unavailable while saving. ${RETRY}`,
  },
  internal_error: { title: "The orchestrator hit an internal error", guidance: RETRY },
  interrupted: {
    title: "Interrupted by an orchestrator restart",
    guidance: "Interrupted workflows are not resumed automatically, to avoid running and billing the same work twice. Submit the task again.",
  },
};

export function describeFailure(code: string | null): FailureCopy {
  return (code && FAILURES[code]) || { title: "The workflow failed", guidance: RETRY };
}

// Timeline: Queued → Worker → Reviewer → Completed/Failed.

export type StepState = "complete" | "current" | "failed" | "upcoming" | "skipped";

export interface TimelineStep {
  key: "queued" | "worker" | "reviewer" | "outcome";
  label: string;
  state: StepState;
  detail: string;
  /** ISO timestamp shown next to the step, when the backend recorded one. */
  at: string | null;
}

const STATE_TEXT: Record<StepState, string> = {
  complete: "Done",
  current: "In progress",
  failed: "Failed",
  upcoming: "Waiting",
  skipped: "Not run",
};
export const stepStateText = (s: StepState): string => STATE_TEXT[s];

function latestRun(runs: AgentRun[], agent: AgentRole): AgentRun | undefined {
  return runs.filter((r) => r.agent === agent).at(-1);
}

function agentStep(w: WorkflowDetail, agent: AgentRole): TimelineStep {
  const run = latestRun(w.runs, agent);
  const label = AGENT_LABEL[agent];
  const doing = agent === "worker" ? "Drafting an answer" : "Assessing the draft";
  if (run) {
    const took = formatDuration(run.started_at, run.finished_at);
    switch (run.status) {
      case "running":
        return { key: agent, label, state: "current", detail: `${doing}…`, at: run.started_at };
      case "completed":
        return {
          key: agent,
          label,
          state: "complete",
          detail: agent === "worker" ? `Draft produced${took ? ` in ${took}` : ""}` : `Review returned${took ? ` in ${took}` : ""}`,
          at: run.finished_at ?? run.started_at,
        };
      case "failed":
        return {
          key: agent,
          label,
          state: "failed",
          detail: describeFailure(run.error_code ?? w.error_code).title,
          at: run.finished_at ?? run.started_at,
        };
    }
  }
  // No run recorded for this agent.
  if (w.status === "failed") {
    if (w.failed_stage === agent) {
      return { key: agent, label, state: "failed", detail: describeFailure(w.error_code).title, at: w.finished_at };
    }
    return { key: agent, label, state: "skipped", detail: "Not started", at: null };
  }
  return {
    key: agent,
    label,
    state: "upcoming",
    detail: agent === "worker" ? "Will draft an answer" : "Will assess the draft independently",
    at: null,
  };
}

export function deriveTimeline(w: WorkflowDetail): TimelineStep[] {
  const queued: TimelineStep =
    w.status === "queued"
      ? { key: "queued", label: "Queued", state: "current", detail: "Waiting for the orchestrator to start", at: w.created_at }
      : { key: "queued", label: "Queued", state: "complete", detail: "Accepted by the orchestrator", at: w.created_at };

  let outcome: TimelineStep;
  if (w.status === "completed") {
    const total = formatDuration(w.started_at, w.finished_at);
    outcome = {
      key: "outcome",
      label: "Completed",
      state: "complete",
      detail: `${w.verdict ? VERDICT_LABEL[w.verdict] : "Finished"}${total ? ` · ${total} total` : ""}`,
      at: w.finished_at,
    };
  } else if (w.status === "failed") {
    outcome = {
      key: "outcome",
      label: "Failed",
      state: "failed",
      detail: w.failed_stage ? `Stopped at the ${AGENT_LABEL[w.failed_stage]} step` : "Stopped before completion",
      at: w.finished_at,
    };
  } else {
    outcome = { key: "outcome", label: "Completed", state: "upcoming", detail: "Results appear here when the review is in", at: null };
  }

  return [queued, agentStep(w, "worker"), agentStep(w, "reviewer"), outcome];
}

/** One sentence describing where the workflow is; announced to screen readers on change. */
export function statusSentence(w: Pick<Workflow, "status" | "verdict" | "failed_stage"> & { runs?: AgentRun[] }): string {
  switch (w.status) {
    case "queued":
      return "Queued. Waiting for the orchestrator to start.";
    case "running": {
      const current = w.runs?.findLast((r) => r.status === "running");
      if (current?.agent === "reviewer") return "Running. The Reviewer is assessing the draft.";
      if (current?.agent === "worker") return "Running. The Worker is drafting an answer.";
      return "Running. Contacting the agents.";
    }
    case "completed":
      return w.verdict === "needs_changes"
        ? "Completed. The Reviewer requested changes."
        : "Completed. The Reviewer passed the draft.";
    case "failed":
      return w.failed_stage ? `Failed during the ${AGENT_LABEL[w.failed_stage]} step.` : "Failed.";
  }
}

/**
 * Compact fingerprint of everything that changes the rendered detail view. The
 * live-update script swaps the DOM only when it changes, so text selection and
 * scroll position survive polls that bring nothing new.
 */
export function detailSignature(w: WorkflowDetail): string {
  return [
    w.status,
    w.verdict ?? "-",
    w.finished_at ?? "-",
    w.runs.map((r) => `${r.agent}:${r.status}:${r.a2a_state ?? "-"}`).join(","),
    w.draft ? "d" : "-",
    w.review ? "r" : "-",
  ].join("|");
}
