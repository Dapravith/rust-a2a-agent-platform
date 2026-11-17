import { defineConfig, envField } from "astro/config";
import node from "@astrojs/node";

// Server-rendered portal (Phase 4, see docs/PHASE4.md). The Node server is the
// only thing the browser talks to; it calls the Rust API server-side at
// API_ORIGIN, so the API and the agents can stay on a private network.
export default defineConfig({
  output: "server",
  adapter: node({ mode: "standalone" }),
  server: { host: "127.0.0.1", port: 4321 },
  build: { inlineStylesheets: "never" },
  security: {
    // Rejects cross-site form POSTs (CSRF) via Sec-Fetch-Site / Origin.
    checkOrigin: true,
    // Host names the portal is served under. Astro trusts the Host header only
    // for these (otherwise it assumes `localhost`, and the Origin check fails
    // for browsers that send no Sec-Fetch-Site). Build-time setting.
    allowedDomains: (process.env.PORTAL_HOSTS ?? "127.0.0.1,localhost")
      .split(",")
      .map((hostname) => hostname.trim())
      .filter(Boolean)
      .map((hostname) => ({ hostname })),
  },
  devToolbar: { enabled: false },
  redirects: { "/history": "/workflows" },
  env: {
    schema: {
      // Read at runtime from the process environment; never sent to the browser.
      API_ORIGIN: envField.string({
        context: "server",
        access: "secret",
        optional: true,
        default: "http://127.0.0.1:8080",
        url: true,
      }),
      API_TIMEOUT_MS: envField.number({
        context: "server",
        access: "secret",
        optional: true,
        default: 10000,
        int: true,
        gt: 0,
      }),
    },
  },
  vite: {
    // Keep every script and asset external so the strict CSP (script-src 'self') holds.
    build: { assetsInlineLimit: 0 },
  },
});
