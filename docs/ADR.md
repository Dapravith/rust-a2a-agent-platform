### ADR-001: Rust Backend Architecture

**Decision:** Adopt a modular monolith for the core Rust Axum application with two independently running A2A agent servers.

**Implementation rules:**
- One Rust Cargo workspace.
- One central Axum application for REST API, orchestration, and persistence.
- Organize application code into cohesive Rust modules.
- Worker and Reviewer run as independent A2A servers.
- Use the official Rust A2A SDK for communication.
- Supabase PostgreSQL is accessed only by the core application.
- Avoid introducing additional microservices, event brokers, service discovery systems, or distributed workflow engines.
- Keep Kubernetes deployment and observability in P1.

**Rationale:** This approach balances maintainability, simple application logic, and real A2A interoperability while preserving the approved product scope.

**Trade-off:** The system requires three Rust runtime processes, but avoids a full microservices architecture with distributed business logic and independently owned databases.
### ADR-002: Server-rendered Astro portal (Phase 4)

**Decision:** Replace the static Astro build served by the Rust API (`FRONTEND_DIR`) with an Astro SSR portal (`output: "server"`, official `@astrojs/node` adapter, standalone mode). The browser talks only to the portal; the portal calls the Rust REST API server-side at `API_ORIGIN`.

**Implementation rules:**
- Rust stays the only business backend: validation, admission, idempotency, orchestration, persistence. The portal renders, forwards and explains; it never decides.
- Pages work without JavaScript (form POST + 303 redirect, cursor links, `<noscript>` refresh). Scripts only enhance: instant validation, double-submit guard, live updates, copy, local time.
- Live updates poll a server-rendered fragment (`/workflows/{id}/live`), so agent output is rendered by one code path, on the server.
- No client framework, no state library; `markdown-it` (server-only) is the one runtime dependency besides Astro and the adapter.
- The portal sets the CSP and security headers itself (`src/middleware.ts`); Astro `checkOrigin` rejects cross-site form posts.

**Rationale:** Server rendering removes the browser → API JSON surface (the API and agents can stay on a private network), gives correct HTTP status codes for 404/503 states, and renders untrusted Markdown once, safely, on the server.

**Trade-off / deployment implications:** One more runtime process (Node ≥ 22.23.4) next to the three Rust processes. The portal must reach the API (`API_ORIGIN`); its public host names must be listed at build time (`PORTAL_HOSTS`, for the Origin check). TLS, authentication and rate limiting still belong to the reverse proxy / P1, as before. `FRONTEND_DIR` must be unset (the Rust static-serving code is kept, unused, and can be removed later).
