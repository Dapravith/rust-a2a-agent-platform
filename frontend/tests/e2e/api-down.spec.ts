// A portal instance whose API_ORIGIN points at a closed port (see
// playwright.config.ts): every page must degrade with recovery guidance and
// must never pretend data or a submission succeeded.
import { expect, test } from "@playwright/test";

const DOWN = "http://127.0.0.1:14322";

test("dashboard degrades but stays usable", async ({ page }) => {
  await page.goto(`${DOWN}/`);
  await expect(page.getByLabel("Software engineering question")).toBeVisible();
  await expect(page.getByText("The orchestrator API is unreachable")).toBeVisible();
  await expect(page.getByText("Status unavailable")).toHaveCount(2);
  await expect(page.getByText("Reachable", { exact: true })).toHaveCount(0);
});

test("a submission against an unreachable API reports that nothing was submitted", async ({ page }) => {
  await page.goto(`${DOWN}/`);
  await page.getByLabel("Software engineering question").fill("Will this be submitted while the API is down?");
  await page.getByRole("button", { name: "Run workflow" }).click();
  const alert = page.getByRole("alert");
  await expect(alert).toContainText("The task was not submitted");
  await expect(alert).toContainText("The orchestrator API is unreachable");
  await expect(alert).toBeFocused();
  await expect(page.getByLabel("Software engineering question")).toHaveValue("Will this be submitted while the API is down?");
  await expect(page.getByRole("button", { name: "Run workflow" })).toBeEnabled();
});

test("history and detail show an error state with a retry", async ({ page }) => {
  const res = await page.goto(`${DOWN}/workflows`);
  expect(res?.status()).toBe(503);
  await expect(page.getByText("The orchestrator API is unreachable")).toBeVisible();
  await expect(page.getByRole("link", { name: "Try again" })).toHaveAttribute("href", "/workflows");
  const detail = await page.goto(`${DOWN}/workflows/2b5d6f4e-1c2a-4b7e-9f00-000000000000`);
  expect(detail?.status()).toBe(503);
  await expect(page.getByText("The orchestrator API is unreachable")).toBeVisible();
});

test("no stack traces or internals leak into error pages", async ({ page }) => {
  await page.goto(`${DOWN}/workflows`);
  const html = await page.content();
  expect(html).not.toMatch(/ECONNREFUSED|at .*\.mjs|TypeError|18999/);
});
