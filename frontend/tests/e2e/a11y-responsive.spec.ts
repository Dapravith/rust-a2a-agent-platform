// Automated accessibility (axe, WCAG 2.2 A/AA rules), keyboard access,
// horizontal overflow at the target viewports, and screenshots for review.
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { resetDatabase, seedCompleted, seedFailedAtReviewer, seedManyCompleted, seedRunning, waitForIdle } from "./support/db.ts";

const VIEWPORTS = [
  { name: "mobile", width: 375, height: 812 },
  { name: "tablet", width: 768, height: 1024 },
  { name: "desktop", width: 1440, height: 900 },
];

let ids: { completed: string; failed: string; running: Awaited<ReturnType<typeof seedRunning>> };

test.beforeAll(async () => {
  await waitForIdle();
  await resetDatabase();
  await seedManyCompleted(Array.from({ length: 6 }, (_, i) => ({ question: `Earlier question ${i}: how should a Rust API version its endpoints?`, createdSecondsAgo: 5000 - i * 60 })));
  ids = {
    failed: await seedFailedAtReviewer("Why did the review step fail for this question about retries?"),
    completed: await seedCompleted({ question: "Propose validation rules for a Rust API that creates crowdfunding campaigns.", createdSecondsAgo: 30 }),
    running: await seedRunning("How should a webhook consumer deduplicate deliveries?"),
  };
});

test.afterAll(async () => {
  await ids.running.complete();
});

const pages = () => [
  { name: "dashboard", path: "/" },
  { name: "history", path: "/workflows" },
  { name: "detail-completed", path: `/workflows/${ids.completed}` },
  { name: "detail-failed", path: `/workflows/${ids.failed}` },
  { name: "detail-running", path: `/workflows/${ids.running.id}` },
  { name: "not-found", path: "/nope" },
];

async function axe(page: Page) {
  const results = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa", "best-practice"]).analyze();
  return results.violations.map((v) => `${v.id} (${v.impact}): ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
}

for (const vp of VIEWPORTS) {
  test(`axe, overflow and screenshots at ${vp.name} (${vp.width}px)`, async ({ page }) => {
    await page.setViewportSize({ width: vp.width, height: vp.height });
    for (const p of pages()) {
      await page.goto(p.path);
      await page.waitForLoadState("networkidle").catch(() => {});
      expect(await axe(page), `${p.name} @ ${vp.width}px`).toEqual([]);
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
      expect(overflow, `${p.name} horizontal overflow @ ${vp.width}px`).toBeLessThanOrEqual(0);
      await page.screenshot({ path: `test-results/screenshots/${vp.name}-${p.name}.png`, fullPage: true });
    }
  });
}

test("dark theme passes axe colour contrast", async ({ browser }) => {
  const ctx = await browser.newContext({ colorScheme: "dark", viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  for (const p of pages()) {
    await page.goto(`http://127.0.0.1:14321${p.path}`);
    expect(await axe(page), `${p.name} (dark)`).toEqual([]);
    if (p.name === "detail-completed" || p.name === "dashboard") {
      await page.screenshot({ path: `test-results/screenshots/dark-${p.name}.png`, fullPage: true });
    }
  }
  await ctx.close();
});

test("keyboard: skip link, navigation and form are reachable in order", async ({ page }) => {
  await page.goto("/");
  await page.keyboard.press("Tab");
  const skip = page.getByRole("link", { name: "Skip to content" });
  await expect(skip).toBeFocused();
  await expect(skip).toBeInViewport();
  await page.keyboard.press("Enter");
  await expect(page.locator("#main")).toBeFocused();
  await page.goto("/");
  const order: string[] = [];
  for (let i = 0; i < 6; i++) {
    await page.keyboard.press("Tab");
    order.push(await page.evaluate(() => {
      const el = document.activeElement as HTMLElement;
      return el.tagName === "TEXTAREA" ? el.id : (el.textContent ?? "").trim();
    }));
  }
  // A workflow is running in this suite, so its "View progress" link precedes the form.
  expect(order).toEqual(["Skip to content", "A2A Review", "Dashboard", "Workflows", "View progress", "question"]);
  await page.keyboard.type("Keyboard-only submission of a question");
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "Run workflow" })).toBeFocused();
  // Visible focus indicator on the focused control.
  const outline = await page.getByRole("button", { name: "Run workflow" }).evaluate((el) => getComputedStyle(el).outlineStyle);
  expect(outline).toBe("solid");
});

test("reduced motion stops spinner animation", async ({ browser }) => {
  const ctx = await browser.newContext({ reducedMotion: "reduce" });
  const page = await ctx.newPage();
  await page.goto(`http://127.0.0.1:14321/workflows/${ids.running.id}`);
  const duration = await page.locator(".timeline__spin").first().evaluate((el) => parseFloat(getComputedStyle(el).animationDuration));
  expect(duration).toBeLessThan(0.001);
  await ctx.close();
});

test("touch targets meet the 24px minimum (WCAG 2.5.8)", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  for (const path of ["/", "/workflows", `/workflows/${ids.completed}`]) {
    await page.goto(path);
    const small = await page.evaluate(() =>
      [...document.querySelectorAll<HTMLElement>("button, a.btn, .nav__link, .copy")]
        .filter((el) => el.offsetParent !== null)
        .map((el) => ({ el: el.textContent?.trim(), ...el.getBoundingClientRect().toJSON() }))
        .filter((r) => r.height < 24 || r.width < 24),
    );
    expect(small, path).toEqual([]);
  }
});
