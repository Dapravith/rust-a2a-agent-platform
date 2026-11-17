// Shared progressive enhancements, loaded on every page:
// 1. `<time data-local>`: server renders UTC; show the viewer's local time.
// 2. `[data-copy-target]`: copy the target element's text to the clipboard.
// Both re-run on `portal:updated`, fired after a live region is swapped.
import { formatLocal } from "../lib/format.ts";

function localizeTimes(root: ParentNode = document): void {
  for (const el of root.querySelectorAll<HTMLTimeElement>("time[data-local]")) {
    const iso = el.dateTime;
    if (!iso || Number.isNaN(Date.parse(iso))) continue;
    el.title = el.textContent ?? "";
    el.textContent = formatLocal(iso);
  }
}

function enableCopyButtons(root: ParentNode = document): void {
  if (!navigator.clipboard?.writeText) return; // insecure context: keep hidden
  for (const btn of root.querySelectorAll<HTMLButtonElement>("button[data-copy-target]")) btn.hidden = false;
}

const announcer = (() => {
  const el = document.createElement("div");
  el.className = "sr-only";
  el.setAttribute("role", "status");
  el.setAttribute("aria-live", "polite");
  document.body.append(el);
  return el;
})();

document.addEventListener("click", async (event) => {
  const btn = (event.target as Element | null)?.closest<HTMLButtonElement>("button[data-copy-target]");
  if (!btn) return;
  const target = document.getElementById(btn.dataset.copyTarget ?? "");
  const label = btn.querySelector<HTMLElement>("[data-copy-label]");
  if (!target || !label) return;
  const original = label.dataset.original ?? label.textContent ?? "Copy";
  label.dataset.original = original;
  try {
    await navigator.clipboard.writeText(target.textContent ?? "");
    label.textContent = "Copied";
    announcer.textContent = "Copied to clipboard.";
  } catch {
    label.textContent = "Copy failed";
    announcer.textContent = "Could not copy to the clipboard.";
  }
  window.setTimeout(() => {
    label.textContent = original;
  }, 2000);
});

document.addEventListener("portal:updated", (event) => {
  const root = (event.target as ParentNode | null) ?? document;
  localizeTimes(root);
  enableCopyButtons(root);
});

localizeTimes();
enableCopyButtons();
