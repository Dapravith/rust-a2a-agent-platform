// Live updates for the workflow detail page.
//
// While `#workflow-live[data-active="true"]`, fetch the server-rendered
// `/workflows/{id}/live` fragment every POLL_MS. The DOM is replaced only when
// the server-computed `data-signature` changes, so polls that bring nothing
// new never disturb selection, focus or scroll. Polling stops at a terminal
// state, pauses while the tab is hidden, and backs off on errors.
const POLL_MS = 2000;
const MAX_BACKOFF_MS = 30_000;
const MAX_FAILURES = 6;

const region = () => document.getElementById("workflow-live");

function start(): void {
  const initial = region();
  if (!initial || initial.dataset.active !== "true") return;
  const id = initial.dataset.workflowId!;
  const url = `/workflows/${encodeURIComponent(id)}/live`;

  const announcer = document.createElement("div");
  announcer.className = "sr-only";
  announcer.setAttribute("role", "status");
  announcer.setAttribute("aria-live", "polite");
  document.body.append(announcer);

  let timer: number | undefined;
  let failures = 0;
  let inFlight = false;
  let stopped = false;

  const indicator = (text: string) => {
    const el = region()?.querySelector<HTMLElement>("[data-live-indicator]");
    if (!el) return;
    el.hidden = text === "";
    el.textContent = text;
  };

  const schedule = (ms: number) => {
    window.clearTimeout(timer);
    if (!stopped && document.visibilityState === "visible") timer = window.setTimeout(tick, ms);
  };

  const swap = (html: string) => {
    const current = region();
    if (!current) return;
    const doc = new DOMParser().parseFromString(html, "text/html");
    const next = doc.getElementById("workflow-live");
    if (!next || next.dataset.workflowId !== id) throw new Error("unexpected fragment");
    if (next.dataset.signature === current.dataset.signature) return;

    // Keep keyboard focus on the same control when it still exists.
    const focusedId = current.contains(document.activeElement) ? document.activeElement?.id : undefined;
    const adopted = document.importNode(next, true);
    current.replaceWith(adopted);
    if (focusedId) document.getElementById(focusedId)?.focus({ preventScroll: true });
    adopted.dispatchEvent(new CustomEvent("portal:updated", { bubbles: true }));

    if (adopted.dataset.statusText && adopted.dataset.statusText !== current.dataset.statusText) {
      announcer.textContent = adopted.dataset.statusText;
      document.title = document.title.replace(/^\w+ workflow/, `${adopted.dataset.statusText.split(/[ .]/)[0]} workflow`);
    }
    if (adopted.dataset.active !== "true") stopped = true;
  };

  async function tick(): Promise<void> {
    if (inFlight || stopped) return;
    inFlight = true;
    try {
      const res = await fetch(url, { headers: { Accept: "text/html" }, cache: "no-store", credentials: "same-origin" });
      if (res.status === 404) {
        stopped = true;
        indicator("This workflow no longer exists. Live updates stopped.");
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      swap(await res.text());
      failures = 0;
      indicator("");
      schedule(POLL_MS);
    } catch {
      failures += 1;
      if (failures >= MAX_FAILURES) {
        stopped = true;
        indicator("Live updates paused: the server is not responding. Reload the page to try again.");
        return;
      }
      const delay = Math.min(POLL_MS * 2 ** failures, MAX_BACKOFF_MS);
      indicator(`Connection problem. Retrying in ${Math.round(delay / 1000)} s…`);
      schedule(delay);
    } finally {
      inFlight = false;
    }
  }

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") schedule(0);
    else window.clearTimeout(timer);
  });

  schedule(POLL_MS);
}

start();
