// Process liveness for container probes. Deliberately does not call the API:
// the portal renders an "API unavailable" page on its own, and coupling its
// probe to the API would restart healthy portal pods during an API outage.
import type { APIRoute } from "astro";

export const GET: APIRoute = () =>
  new Response("ok", {
    headers: { "Content-Type": "text/plain; charset=utf-8", "Cache-Control": "no-store" },
  });
