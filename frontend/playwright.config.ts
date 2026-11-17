// Browser tests against the real stack: Rust API + both A2A agents (mock
// inference) on a scratch database (scripts/e2e-stack.sh), and the built Astro
// server. A second Astro server points at a closed port to exercise the
// "API unreachable" states. Run with `npm run test:e2e`.
import { defineConfig, devices } from "@playwright/test";

const node = process.execPath;
const API = "http://127.0.0.1:18080";

export default defineConfig({
  testDir: "tests/e2e",
  // One active workflow at a time is a backend rule; tests share one database.
  workers: 1,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  timeout: 180_000,
  expect: { timeout: 20_000 },
  reporter: [["list"], ["html", { open: "never", outputFolder: "test-results/report" }]],
  outputDir: "test-results/artifacts",
  use: {
    baseURL: "http://127.0.0.1:14321",
    trace: "retain-on-failure",
    viewport: { width: 1440, height: 900 },
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 900 } } }],
  webServer: [
    {
      command: "../scripts/e2e-stack.sh",
      url: `${API}/healthz`,
      timeout: 240_000,
      reuseExistingServer: !process.env.CI,
      stdout: "ignore",
      stderr: "pipe",
    },
    {
      command: `"${node}" dist/server/entry.mjs`,
      url: "http://127.0.0.1:14321/",
      env: { HOST: "127.0.0.1", PORT: "14321", API_ORIGIN: API, API_TIMEOUT_MS: "10000" },
      reuseExistingServer: !process.env.CI,
    },
    {
      command: `"${node}" dist/server/entry.mjs`,
      url: "http://127.0.0.1:14322/",
      env: { HOST: "127.0.0.1", PORT: "14322", API_ORIGIN: "http://127.0.0.1:18999", API_TIMEOUT_MS: "2000" },
      reuseExistingServer: !process.env.CI,
    },
  ],
});
