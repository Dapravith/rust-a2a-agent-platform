import { expect, test } from "@playwright/test";
import { countWorkflows, resetDatabase, seedRunning, waitForIdle } from "./support/db.ts";

test.beforeEach(async () => {
  await waitForIdle();
  await resetDatabase();
});

test("shows the task form, the pipeline and live agent discovery", async ({ page }) => {
  await page.goto("/");
  await expect(page).toHaveTitle("Dashboard · A2A Review");
  await expect(page.getByRole("heading", { level: 1, name: "Dashboard" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Dashboard" })).toHaveAttribute("aria-current", "page");
  await expect(page.getByLabel("Software engineering question")).toBeVisible();
  await expect(page.getByRole("button", { name: "Run workflow" })).toBeEnabled();
  // From GET /api/agents (real Agent Cards served by the agent processes).
  const how = page.getByRole("complementary", { name: "How it works" });
  await expect(how.getByText("Draft Worker")).toBeVisible();
  await expect(how.getByText("Draft Reviewer")).toBeVisible();
  await expect(how.getByText("Reachable")).toHaveCount(2);
  await expect(page.getByText("No workflows yet")).toBeVisible();
});

test("validates the question before submitting", async ({ page }) => {
  await page.goto("/");
  const field = page.getByLabel("Software engineering question");
  await page.getByRole("button", { name: "Run workflow" }).click();
  await expect(page.getByText("Enter a question for the agents.")).toBeVisible();
  await expect(field).toHaveAttribute("aria-invalid", "true");
  await expect(field).toBeFocused();
  await field.fill("too short");
  await page.getByRole("button", { name: "Run workflow" }).click();
  await expect(page.getByText("Add at least 1 more character (minimum 10).")).toBeVisible();
  await field.fill("Long enough question now");
  await expect(field).not.toHaveAttribute("aria-invalid", "true");
  await expect(page.getByText(/more character/)).toBeHidden();
  await expect(page.locator("[data-count]")).toHaveText("24 / 4,000");
  expect(await countWorkflows()).toBe(0);
});

test("server-side validation works without JavaScript", async ({ browser }) => {
  const ctx = await browser.newContext({ javaScriptEnabled: false });
  const page = await ctx.newPage();
  await page.goto("/");
  await page.getByLabel("Software engineering question").fill("         x");
  await page.getByRole("button", { name: "Run workflow" }).click();
  // Rust rejected it (422); its message is shown on the field, the text is kept.
  await expect(page.getByText("Must contain at least 10 non-whitespace characters.")).toBeVisible();
  await expect(page.getByLabel("Software engineering question")).toHaveValue("         x");
  expect(await countWorkflows()).toBe(0);
  await ctx.close();
});

test("submits a task and lands on its live detail page", async ({ page }) => {
  const question = "How should a Rust service validate pagination parameters?";
  await page.goto("/");
  await page.getByLabel("Software engineering question").fill(question);
  await page.getByRole("button", { name: "Run workflow" }).click();
  await expect(page).toHaveURL(/\/workflows\/[0-9a-f-]{36}$/);
  // A real Worker → Reviewer run: the page follows it live to the terminal
  // state. Generous timeout: real agent and database latency, not the UI.
  await expect(page.getByText(/^Completed: the (draft passed review|Reviewer requested changes)$/)).toBeVisible({ timeout: 90_000 });
  await expect(page.getByRole("region", { name: "Worker draft" }).getByRole("article")).toBeVisible();
  await expect(page.getByRole("region", { name: "Reviewer verdict" }).getByText(/Passed|Changes requested/).first()).toBeVisible();
  expect(await countWorkflows(question)).toBe(1);
});

test("a double submit creates exactly one workflow", async ({ page }) => {
  const question = "What is a safe default timeout for outbound HTTP calls?";
  await page.goto("/");
  await page.getByLabel("Software engineering question").fill(question);
  const button = page.getByRole("button", { name: "Run workflow" });
  await button.dblclick();
  await expect(page).toHaveURL(/\/workflows\/[0-9a-f-]{36}$/);
  await waitForIdle();
  expect(await countWorkflows(question)).toBe(1);
});

test("shows progress while submitting and blocks a second submit", async ({ page }) => {
  await page.goto("/");
  await page.getByLabel("Software engineering question").fill("How do I make a Rust CLI respect NO_COLOR?");
  // Observe each submit after the enhancement script ran, and keep the page here.
  await page.evaluate(() => {
    const form = document.querySelector<HTMLFormElement>("form[data-task-form]")!;
    const btn = form.querySelector<HTMLButtonElement>("[data-submit]")!;
    const log: unknown[] = [];
    (window as unknown as { __submits: unknown[] }).__submits = log;
    form.addEventListener("submit", (e) => {
      log.push({ blocked: e.defaultPrevented, disabled: btn.disabled, busy: btn.getAttribute("aria-busy"), label: btn.textContent?.trim() });
      e.preventDefault();
    });
  });
  await page.getByRole("button", { name: "Run workflow" }).click();
  // Second attempt (Enter key / programmatic) while the first is in flight.
  await page.evaluate(() => document.querySelector<HTMLFormElement>("form[data-task-form]")!.requestSubmit());
  const submits = await page.evaluate(() => (window as unknown as { __submits: unknown[] }).__submits);
  expect(submits).toEqual([
    { blocked: false, disabled: true, busy: "true", label: "Submitting…" },
    { blocked: true, disabled: true, busy: "true", label: "Submitting…" },
  ]);
  await expect(page.locator("[data-form-status]")).toHaveText("Submitting your task to the orchestrator…");
  expect(await countWorkflows()).toBe(0);
});

test("explains that another workflow is running", async ({ page }) => {
  const running = await seedRunning("A long-running question that is still being drafted");
  await page.goto("/");
  await expect(page.getByText("A workflow is in progress")).toBeVisible();
  await page.getByLabel("Software engineering question").fill("A second question while one is running");
  await page.getByRole("button", { name: "Run workflow" }).click();
  // Rust answers 409 workflow_active; the text is kept for a later retry.
  await expect(page.getByText("Another workflow is still running")).toBeVisible();
  await expect(page.getByRole("link", { name: "View the running workflow" })).toHaveAttribute("href", `/workflows/${running.id}`);
  await expect(page.getByLabel("Software engineering question")).toHaveValue("A second question while one is running");
  await running.complete();
});
