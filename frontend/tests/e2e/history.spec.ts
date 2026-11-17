import { expect, test } from "@playwright/test";
import { resetDatabase, seedCompleted, seedFailedAtReviewer, seedManyCompleted, waitForIdle } from "./support/db.ts";

test.beforeEach(async () => {
  await waitForIdle();
  await resetDatabase();
});

test("empty history explains what to do next", async ({ page }) => {
  await page.goto("/workflows");
  await expect(page.getByRole("heading", { level: 1, name: "Workflows" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Workflows" }).first()).toHaveAttribute("aria-current", "page");
  await expect(page.getByText("No workflows yet")).toBeVisible();
  await page.getByRole("link", { name: "Submit a task" }).click();
  await expect(page).toHaveURL("/");
});

test("lists workflows newest first with status, verdict and links", async ({ page }) => {
  const older = await seedFailedAtReviewer("Older failed question about reviewer outages", "agent_unreachable");
  const newer = await seedCompleted({ question: "Newer completed question about validation rules", createdSecondsAgo: 10 });
  await page.goto("/workflows");
  const rows = page.locator(".wf-row");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText("Newer completed question");
  await expect(rows.nth(0)).toContainText("Completed");
  await expect(rows.nth(0)).toContainText("Changes requested");
  await expect(rows.nth(1)).toContainText("Failed");
  await expect(rows.nth(1)).toContainText("Not reviewed");
  await expect(rows.nth(0).getByRole("link")).toHaveAttribute("href", `/workflows/${newer}`);
  await rows.nth(1).click(); // whole row is clickable
  await expect(page).toHaveURL(`/workflows/${older}`);
});

test("pages through history with the API cursor", async ({ page }) => {
  await seedManyCompleted(Array.from({ length: 30 }, (_, i) => ({ question: `Paged question number ${String(i).padStart(2, "0")}`, createdSecondsAgo: 1000 - i })));
  await page.goto("/workflows");
  await expect(page.locator(".wf-row")).toHaveCount(25);
  await expect(page.locator(".wf-row").first()).toContainText("Paged question number 29");
  await page.getByRole("link", { name: "Older" }).click();
  await expect(page).toHaveURL(/before=/);
  await expect(page.locator(".wf-row")).toHaveCount(5);
  await expect(page.locator(".wf-row").last()).toContainText("Paged question number 00");
  await expect(page.getByRole("link", { name: "Older" })).toHaveCount(0);
  await page.getByRole("link", { name: "Newest" }).click();
  await expect(page.locator(".wf-row")).toHaveCount(25);
});

test("an invalid cursor is not forwarded to the API", async ({ page }) => {
  await page.goto("/workflows?before=';DROP");
  await expect(page).toHaveURL("/workflows");
});

test("legacy static URLs redirect", async ({ page }) => {
  const id = await seedCompleted({ question: "Legacy link question for redirects" });
  await page.goto("/history/");
  await expect(page).toHaveURL("/workflows");
  await page.goto(`/workflow/?id=${id}`);
  await expect(page).toHaveURL(`/workflows/${id}`);
});
