import { expect, test, type Page } from "@playwright/test";
import { resetDatabase, sampleReview, seedCompleted, seedFailedAtReviewer, seedRunning, waitForIdle } from "./support/db.ts";

test.beforeEach(async () => {
  await waitForIdle();
  await resetDatabase();
});

const step = (page: Page, label: string) =>
  page.getByRole("complementary", { name: "Workflow progress" }).locator(".timeline__step")
    .filter({ has: page.locator(".timeline__label", { hasText: new RegExp(`^${label}`) }) });

test("live updates follow the real backend states and stop at the terminal state", async ({ page }) => {
  const wf = await seedRunning("How should retries be bounded for a payment webhook?");
  const polls: number[] = [];
  page.on("request", (r) => {
    if (r.url().endsWith(`/workflows/${wf.id}/live`)) polls.push(Date.now());
  });

  await page.goto(`/workflows/${wf.id}`);
  await expect(page.getByText("Running. The Worker is drafting an answer.").first()).toBeVisible();
  await expect(step(page, "Worker")).toHaveClass(/is-current/);
  await expect(step(page, "Reviewer")).toHaveClass(/is-upcoming/);
  await expect(page.getByText("The Worker is drafting an answer", { exact: true })).toBeVisible();

  // Worker finishes, Reviewer starts: the page updates without a reload.
  await wf.finishWorker();
  const reviewerRun = await wf.startReviewer();
  await expect(page.getByRole("heading", { name: "Validation rules for campaign creation" })).toBeVisible();
  await expect(step(page, "Worker")).toHaveClass(/is-complete/);
  await expect(step(page, "Reviewer")).toHaveClass(/is-current/);
  await expect(page.getByText("The Reviewer is assessing the draft", { exact: true })).toBeVisible();

  await wf.finishReviewer(reviewerRun);
  await expect(page.getByText("Completed: the Reviewer requested changes")).toBeVisible();
  await expect(page.locator(".timeline__step").last()).toHaveClass(/is-complete/);
  await expect(page.locator("#workflow-live")).toHaveAttribute("data-active", "false");
  await expect(page).toHaveTitle(/^Completed workflow/);

  // Terminal: no further polling.
  const atTerminal = polls.length;
  await page.waitForTimeout(5000);
  expect(polls.length).toBe(atTerminal);
});

test("a completed workflow with needs_changes is a successful review, not a failure", async ({ page }) => {
  const id = await seedCompleted({ question: "Propose validation rules for a crowdfunding API." });
  await page.goto(`/workflows/${id}`);
  await expect(page.locator(".detail__meta").getByText("Completed")).toBeVisible();
  await expect(page.locator(".detail__meta").getByText("Changes requested")).toBeVisible();
  await expect(page.getByText("The workflow ran successfully.")).toBeVisible();
  const review = page.getByRole("region", { name: "Reviewer verdict" });
  await expect(review.getByText("2 findings · 1 high, 1 medium")).toBeVisible();
  // Findings are ordered by severity, most severe first.
  await expect(review.locator(".finding__title")).toHaveText(["No maximum goal", "No currency field"]);
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("a passed review is shown as success", async ({ page }) => {
  const id = await seedCompleted({
    question: "Is a 30 second socket timeout sensible?",
    review: { verdict: "pass", summary: "Accurate and complete.", findings: [] },
  });
  await page.goto(`/workflows/${id}`);
  await expect(page.getByText("Completed: the draft passed review")).toBeVisible();
  await expect(page.getByRole("region", { name: "Reviewer verdict" }).getByText("No findings")).toBeVisible();
});

test("a failed workflow explains the stable error code and keeps the draft", async ({ page }) => {
  const id = await seedFailedAtReviewer("What does the Reviewer do when it is offline?");
  await page.goto(`/workflows/${id}`);
  await expect(page.locator(".callout__title", { hasText: "An agent could not be reached" })).toBeVisible();
  await expect(page.getByText(/Check that the Worker and Reviewer agent processes are running/)).toBeVisible();
  await expect(page.locator(".failure").getByText("Reviewer step")).toBeVisible();
  await expect(page.locator(".failure").getByText("agent_unreachable")).toBeVisible();
  await expect(step(page, "Worker")).toHaveClass(/is-complete/);
  await expect(step(page, "Reviewer")).toHaveClass(/is-failed/);
  await expect(page.locator(".timeline__step").last()).toHaveClass(/is-failed/);
  await expect(page.getByRole("heading", { name: "Validation rules for campaign creation" })).toBeVisible();
  await expect(page.getByText("The workflow failed during review. The Worker's draft above was kept.")).toBeVisible();
  await expect(page.getByRole("link", { name: "New task" })).toHaveAttribute("href", "/");
});

test("agent output is rendered as inert text (no XSS)", async ({ page }) => {
  const id = await seedCompleted({
    question: `<img src=x onerror="window.__xss='question'"> <b>not bold</b>`,
    draft: {
      title: `<script>window.__xss='title'</script>Title`,
      summary: `<svg onload="window.__xss='summary'"></svg>`,
      content: [
        "# Heading",
        `<script>window.__xss='content'</script>`,
        `<img src=x onerror="window.__xss='img'">`,
        "[click me](javascript:window.__xss='link')",
        "![tracker](https://example.com/pixel.png)",
        "[docs](https://example.com/docs)",
      ].join("\n\n"),
      assumptions: [`<iframe src="javascript:window.__xss='assumption'"></iframe>`],
    },
    review: { ...sampleReview, findings: [{ severity: "low", title: "<u>t</u>", detail: `"><script>window.__xss='finding'</script>` }] },
  });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto(`/workflows/${id}`);
  await expect(page.getByRole("heading", { name: "Worker draft" })).toBeVisible();

  expect(await page.evaluate(() => (window as unknown as { __xss?: string }).__xss)).toBeUndefined();
  const prose = page.locator(".draft__content");
  await expect(prose.locator("script, img, iframe, svg")).toHaveCount(0);
  await expect(prose).toContainText(`<script>window.__xss='content'</script>`);
  // The javascript: link is not turned into a link; the https one is, with rel.
  await expect(prose.locator('a[href^="javascript"]')).toHaveCount(0);
  await expect(prose.getByRole("link", { name: "docs" })).toHaveAttribute("rel", "nofollow noopener noreferrer");
  // Agent headings are demoted below the page outline (h1 page, h2 card, h3 title).
  await expect(prose.locator("h4", { hasText: "Heading" })).toHaveCount(1);
  await expect(page.locator(".draft__title")).toHaveText(`<script>window.__xss='title'</script>Title`);
  await expect(page.locator(".task-text")).toContainText("<b>not bold</b>");
  await expect(page.locator("main b, main u")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("long agent output stays readable and never overflows the page", async ({ page }) => {
  const longToken = "x".repeat(400);
  const paragraphs = Array.from({ length: 60 }, (_, i) => `Paragraph ${i + 1}. ${"Lorem ipsum dolor sit amet. ".repeat(10)}`);
  const content = [
    "## A long answer",
    ...paragraphs.slice(0, 30),
    "```rust\n" + `let very_long_line = "${"y".repeat(300)}";` + "\n```",
    `An unbroken identifier: ${longToken}`,
    "| col a | col b | col c |\n|---|---|---|\n| " + "z".repeat(120) + " | b | c |",
    ...paragraphs.slice(30),
  ].join("\n\n").slice(0, 19_900);
  const id = await seedCompleted({
    question: `Explain ${"y".repeat(500)} in detail`,
    draft: { title: "T".repeat(190), summary: "S ".repeat(900), content, assumptions: Array.from({ length: 20 }, (_, i) => `Assumption ${i} ${"a".repeat(200)}`) },
  });
  for (const width of [375, 768, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`/workflows/${id}`);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow, `horizontal overflow at ${width}px`).toBeLessThanOrEqual(0);
  }
});

test("unknown and malformed workflow ids show a not-found page", async ({ page }) => {
  const res = await page.goto("/workflows/2b5d6f4e-1c2a-4b7e-9f00-000000000000");
  expect(res?.status()).toBe(404);
  await expect(page.getByText("There is no workflow with this ID")).toBeVisible();
  const bad = await page.goto("/workflows/not-a-uuid");
  expect(bad?.status()).toBe(404);
  await expect(page.getByRole("link", { name: "All workflows" })).toBeVisible();
});

test("copy buttons copy the workflow id and the draft Markdown", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const id = await seedCompleted({ question: "Copy test question for the clipboard" });
  await page.goto(`/workflows/${id}`);
  await page.getByRole("button", { name: "Copy ID" }).click();
  await expect(page.getByRole("button", { name: "Copied" })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(id);
  await page.getByRole("button", { name: "Copy Markdown" }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toContain("## Fields");
});
