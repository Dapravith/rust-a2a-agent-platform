// Security headers for every server-rendered response. The Rust API sets the
// same policy for its own responses (backend/apps/api/src/routes.rs `harden`);
// with SSR the browser receives pages from Astro, so Astro must set them.
import { defineMiddleware } from "astro:middleware";

// All scripts and styles are external files ('self'): no inline code, no
// 'unsafe-inline'. connect-src 'self' covers the live-update fragment fetch.
const CSP = [
  "default-src 'self'",
  "script-src 'self'",
  "style-src 'self'",
  "img-src 'self' data:",
  "font-src 'self'",
  "connect-src 'self'",
  "object-src 'none'",
  "frame-ancestors 'none'",
  "base-uri 'none'",
  "form-action 'self'",
].join("; ");

const HEADERS: Record<string, string> = {
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
  "Referrer-Policy": "no-referrer",
  "Cross-Origin-Opener-Policy": "same-origin",
  "Permissions-Policy": "camera=(), microphone=(), geolocation=(), payment=()",
};

export const onRequest = defineMiddleware(async (_ctx, next) => {
  const response = await next();
  // Responses from `Response.redirect()` have immutable headers; copy them.
  const res = new Response(response.body, response);
  for (const [k, v] of Object.entries(HEADERS)) res.headers.set(k, v);
  // Dev server injects inline scripts for HMR; enforce the CSP in builds only.
  if (import.meta.env.PROD) res.headers.set("Content-Security-Policy", CSP);
  // Pages show live workflow state: never serve them from a cache.
  if (res.headers.get("content-type")?.startsWith("text/html")) res.headers.set("Cache-Control", "no-store");
  return res;
});
