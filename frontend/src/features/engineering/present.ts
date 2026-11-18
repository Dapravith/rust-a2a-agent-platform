// Pure presentation helpers for engineering workflows (unit-tested).
import type { Tone } from "../workflows/present.ts";
import type { EngRun, ToolCall } from "./schemas.ts";

const STATUS: Record<string, { label: string; tone: Tone }> = {
  draft: { label: "Planning", tone: "info" },
  awaiting_plan_approval: { label: "Awaiting plan approval", tone: "warning" },
  approved_for_execution: { label: "Plan approved", tone: "info" },
  executing: { label: "Developing", tone: "info" },
  reviewing: { label: "In review", tone: "info" },
  testing: { label: "Testing", tone: "info" },
  quality_assessment: { label: "Quality assessment", tone: "info" },
  awaiting_delivery_approval: { label: "Awaiting delivery decision", tone: "warning" },
  changes_requested: { label: "Changes requested", tone: "warning" },
  approved_for_delivery: { label: "Approved for delivery", tone: "success" },
  rejected: { label: "Rejected", tone: "error" },
  failed: { label: "Failed", tone: "error" },
  cancelled: { label: "Cancelled", tone: "neutral" },
};

/** Unknown statuses degrade to a neutral label instead of breaking the page. */
export function engStatus(status: string): { label: string; tone: Tone } {
  return STATUS[status] ?? { label: status.replaceAll("_", " "), tone: "neutral" };
}

const ROLE: Record<string, string> = {
  project_manager: "Project Manager",
  project_coordinator: "Project Coordinator",
  developer: "Developer",
  code_reviewer: "Code Reviewer",
  qa_tester: "QA Tester",
  quality_lead: "Quality Lead",
};

export function roleLabel(role: string | null): string {
  return role ? (ROLE[role] ?? role) : "—";
}

/**
 * `deterministic` and `simulation` runs are not AI models; say so plainly, so
 * a rule-based run can never be mistaken for real provider output.
 */
export function providerLabel(run: Pick<EngRun, "provider" | "model">): { label: string; isModel: boolean } {
  if (run.provider === "deterministic") return { label: "Deterministic (rule-based, no model)", isModel: false };
  if (run.provider === "simulation") return { label: "Simulation (test only)", isModel: false };
  if (run.provider === "test-double") return { label: "Test double", isModel: false };
  return { label: run.model ? `${run.provider} · ${run.model}` : run.provider, isModel: true };
}

export function toolTone(call: Pick<ToolCall, "outcome" | "exit_code">): Tone {
  if (call.outcome === "refused") return "warning";
  if (call.outcome === "failed") return "error";
  if (call.exit_code !== null && call.exit_code !== 0) return "error";
  return "success";
}

/** Tool calls grouped by the agent run that made them (correlation). */
export function callsByRun(calls: ToolCall[]): Map<string, ToolCall[]> {
  const m = new Map<string, ToolCall[]>();
  for (const c of calls) {
    if (!c.run_id) continue;
    m.set(c.run_id, [...(m.get(c.run_id) ?? []), c]);
  }
  return m;
}

export function tokens(run: Pick<EngRun, "input_tokens" | "output_tokens">): string | null {
  if (run.input_tokens === null && run.output_tokens === null) return null;
  return `${run.input_tokens ?? "?"} in / ${run.output_tokens ?? "?"} out`;
}
