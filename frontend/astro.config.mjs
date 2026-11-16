import { defineConfig } from "astro/config";

// Static build served by the orchestrator (same origin, no CORS).
// In dev, `/api` is proxied to the orchestrator.
export default defineConfig({
  output: "static",
  build: { format: "directory", inlineStylesheets: "never" },
  vite: {
    // Keep every script external so the strict CSP (script-src 'self') holds.
    build: { assetsInlineLimit: 0 },
    server: {
      proxy: { "/api": process.env.ORCHESTRATOR_URL ?? "http://127.0.0.1:8080" },
    },
  },
});
