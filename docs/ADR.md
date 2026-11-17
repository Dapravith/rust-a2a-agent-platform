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
### ADR-003: Human-supervised multi-agent engineering workflows (Phase 6, proposed extension)

**Status:** Accepted for implementation as an *additive* extension. The P0 Worker → Reviewer workflow is unchanged and stays the verified path until the extension is independently verified.

**Decision:** Add an engineering-workflow subsystem inside the existing modular monolith:
- Pure domain rules in a new crate `crates/engineering` (state machine, role policies + tool allowlist, artifact contracts with integrity hashes, quality gates). No I/O, so every rule is unit-tested and applied identically on every path.
- Persistence, orchestration, approval engine and `/api/eng` routes in `apps/api/src/eng/`. New tables (`eng_*`); artifacts, approvals and audit events are append-only (trigger + grants).
- Roles are application policy, not A2A primitives. A2A stays the transport for remote agents; A2A task state is never treated as workflow or approval state.
- Role agents are reached through a small `RoleAgent` adapter trait. Deterministic simulated agents (provider `simulation`) exist for Mode A tests only; `api serve` wires no agents and the `Disabled` authenticator, so engineering writes answer 503 until a real provider and an identity provider are configured. There is no fallback from a failing provider to the simulation.
- Human gates (plan approval, delivery decision) can only be passed by an authenticated principal with the `approver` role from a server-side `Authenticator`; request bodies carry no identity. Decisions bind to the workflow version, the artifact hash under review and (delivery) the candidate commit SHA.

**Rejected alternatives:** a workflow engine / message broker (not needed for one orchestrator per database); one microservice per role (roles are policy, agents are already independent processes when remote); storing model reasoning (only observable outputs are stored).

**Consequences:** No new infrastructure. New risk surface: code execution by agents (M5) needs a real isolation boundary before any real provider is enabled; human approvals stay BLOCKED until an IdP (OIDC) is integrated.
