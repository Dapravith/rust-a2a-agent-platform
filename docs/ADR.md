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