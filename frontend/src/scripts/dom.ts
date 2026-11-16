// DOM helpers. Untrusted text (questions, agent output, errors) is only ever
// assigned via textContent; nothing here parses HTML.

type Child = Node | string | null | undefined | false;

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, string> = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    el.append(typeof c === "string" ? document.createTextNode(c) : c);
  }
  return el;
}

export function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`missing #${id}`);
  return el as T;
}

export function formatTime(iso: string | null): string {
  if (!iso) return "—";
  return new Date(iso).toLocaleString();
}

export function duration(start: string | null, end: string | null): string {
  if (!start) return "—";
  const ms = (end ? new Date(end).getTime() : Date.now()) - new Date(start).getTime();
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
}

const LABELS: Record<string, string> = {
  queued: "Queued",
  running: "Running",
  completed: "Completed",
  failed: "Failed",
  pass: "Pass",
  needs_changes: "Needs changes",
  low: "Low",
  medium: "Medium",
  high: "High",
  reachable: "Reachable",
  unreachable: "Unreachable",
};

export function badge(value: string): HTMLSpanElement {
  return h("span", { class: `badge badge-${value.replace(/[^a-z_]/g, "")}` }, LABELS[value] ?? value);
}

export function message(kind: "error" | "info" | "empty", text: string): HTMLParagraphElement {
  return h("p", { class: `msg msg-${kind}`, role: kind === "error" ? "alert" : "status" }, text);
}
