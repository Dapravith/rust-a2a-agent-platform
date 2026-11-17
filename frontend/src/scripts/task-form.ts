// Enhances the task form (src/features/workflows/components/TaskForm.astro).
// The server remains authoritative: it re-validates via Rust and re-renders on
// error. This script only gives faster feedback and blocks double submits.
const MIN_CHARS = 10;

function newKey(): string {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  // randomUUID needs a secure context; getRandomValues does not.
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

const chars = (s: string): number => Array.from(s).length;

for (const form of document.querySelectorAll<HTMLFormElement>("form[data-task-form]")) {
  const textarea = form.querySelector<HTMLTextAreaElement>("textarea")!;
  const keyInput = form.querySelector<HTMLInputElement>("[data-idempotency-key]")!;
  const count = form.querySelector<HTMLElement>("[data-count]")!;
  const error = form.querySelector<HTMLElement>("[data-field-error]")!;
  const submit = form.querySelector<HTMLButtonElement>("[data-submit]")!;
  const label = submit.querySelector<HTMLElement>(".btn__label")!;
  const status = form.querySelector<HTMLElement>("[data-form-status]")!;
  const max = Number(form.dataset.max);
  const idleLabel = label.textContent ?? "Run workflow";
  let submitting = false;
  let lastValue = textarea.value;

  const describedBy = (withError: boolean) =>
    ["question-hint", "question-count", withError ? "question-error" : null].filter(Boolean).join(" ");

  const setError = (message: string | null) => {
    error.hidden = !message;
    error.textContent = message ?? "";
    if (message) textarea.setAttribute("aria-invalid", "true");
    else textarea.removeAttribute("aria-invalid");
    textarea.setAttribute("aria-describedby", describedBy(Boolean(message)));
  };

  const updateCount = () => {
    const n = chars(textarea.value);
    count.textContent = `${n.toLocaleString("en-US")} / ${max.toLocaleString("en-US")}`;
    count.dataset.state = n >= max ? "limit" : "";
  };

  const setSubmitting = (on: boolean) => {
    submitting = on;
    submit.disabled = on;
    submit.setAttribute("aria-busy", String(on));
    label.textContent = on ? "Submitting…" : idleLabel;
    status.textContent = on ? "Submitting your task to the orchestrator…" : "";
  };

  textarea.addEventListener("input", () => {
    updateCount();
    // A different question is a different submission: never replay an old key for it.
    if (textarea.value !== lastValue) {
      keyInput.value = newKey();
      lastValue = textarea.value;
    }
    if (!error.hidden && chars(textarea.value.trim()) >= MIN_CHARS) setError(null);
  });

  form.addEventListener("submit", (event) => {
    if (submitting) {
      event.preventDefault();
      return;
    }
    const n = chars(textarea.value.trim());
    if (n < MIN_CHARS) {
      event.preventDefault();
      setError(
        n === 0
          ? "Enter a question for the agents."
          : `Add at least ${MIN_CHARS - n} more character${MIN_CHARS - n === 1 ? "" : "s"} (minimum ${MIN_CHARS}).`,
      );
      textarea.focus();
      return;
    }
    setSubmitting(true);
  });

  // Back/forward cache restores the page as it was: re-enable the form.
  window.addEventListener("pageshow", (event) => {
    if (event.persisted) setSubmitting(false);
  });

  // Focus after a server-side error is set by `autofocus` in the markup.
  updateCount();
}
