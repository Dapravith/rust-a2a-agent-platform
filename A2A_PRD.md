# Rust + Astro A2A Agent Platform — Short PRD

**Version:** 1.2 · Peer Review Draft  
**Date:** 16 November 2026  
**Purpose:** Gather architecture, scope, and DevOps feedback before implementation.

## 1. Product goal

Build a small, secure, full-stack platform where **two independent AI agents collaborate using the Agent2Agent (A2A) protocol**. A developer submits a software engineering question, a **Worker Agent** creates a draft, and a **Reviewer Agent** evaluates it. The dashboard displays both outputs, task status, and history.

The goal is to **prove real A2A interoperability**, not build an autonomous coding or deployment system.

## 2. High-level flow

**User → Astro Dashboard → Rust Axum Orchestrator → Worker (A2A) → Orchestrator → Reviewer (A2A) → Orchestrator → Dashboard**

- **Orchestrator:** Validates requests, discovers trusted Agent Cards, coordinates A2A messages, handles failures, and stores results.
- **Worker:** Calls one configured LLM to generate a draft artifact.
- **Reviewer:** Independently assesses the draft and returns a `pass` or `needs_changes` verdict with findings.
- **Supabase PostgreSQL:** Stores workflows, agent runs, and artifacts; only the Rust backend accesses it.

Agents run as separate Rust A2A servers. Their communication uses the **official Rust A2A SDK with JSON-RPC/HTTP**, not custom agent endpoints. The orchestrator owns the sequence; the agents do not call each other directly in the MVP.

## 3. Core technology stack

| Layer | Choice |
|---|---|
| Frontend | Astro 7, TypeScript, native CSS |
| Backend | Rust, Axum, Tokio |
| A2A | Official Rust A2A SDK |
| AI | One hosted LLM provider via `reqwest`; mock mode for tests |
| Database | Supabase-managed PostgreSQL + SQLx |
| Local development | Cargo and npm |

## 4. Scope and delivery plan

**P0 — Working MVP:** Three web views (submit, history, detail); two independently discoverable A2A agents; Worker → Reviewer workflow; persisted results; safe error handling; interoperability and end-to-end tests. One active workflow at a time.

**P1 — Private remote pilot:** K3s on a remote server; Terraform for supported infrastructure provisioning; Ansible for host hardening; GitHub Actions for CI/scanning/image publishing; Argo CD for approved pull-based deployment; Grafana, Prometheus, Loki, and Alloy for metrics, logs, and alerts; Trivy vulnerability scanning; documented rollback and backup checks.

**TBD / out of scope:** Dozzle as an optional live-log viewer; public SaaS access; extra agents; MCP/tool execution; automated repository edits; multi-model routing; high availability; self-directed agent loops. P1 must not delay the P0 protocol proof.

## 5. Security and operations

- Store LLM/database secrets **server-side only**. Keep agents and management endpoints private; use restricted operator access and HTTPS for remote use.
- Record structured logs with service, timestamp, request/workflow ID, outcome, and trusted source IP where applicable. Do not log keys or sensitive prompt content.
- Alert on repeated authentication failures, successful admin access outside approved networks, service downtime, A2A errors, database outages, and resource pressure.
- **An unfamiliar IP is not proof of an attack.** Correlate identity, trusted proxy/SSH logs, behavior, and Kubernetes audit events before taking action.
- Scan dependencies, images, and deployment configurations; test alert delivery and incident runbooks before calling P1 ready.

## 6. Example use case

A developer asks: *“Propose validation rules for a Rust API that creates crowdfunding campaigns.”* The Worker drafts field rules and responses. The Reviewer flags missing limits and currency-handling issues. Both artifacts appear in Astro. A review verdict of `needs_changes` is a **successful review result**, not a system execution failure. No code is executed or changed.

## 7. Definition of done

**P0 passes when:** Both agents work independently through A2A; a browser-submitted task completes the Worker → Reviewer sequence; results survive an API restart; and failure/interoperability tests pass.

**P1 passes when:** A private remote deployment works through approved CI/CD; management access is restricted; key security/operational alerts deliver successfully; and rollback/restore are demonstrated.

## 8. Feedback requested from reviewers

1. Are **two agents and a fixed Worker → Reviewer flow** enough to validate A2A, or is an important use case missing?
2. Are the Rust/Astro/A2A/Supabase boundaries clear and maintainable?
3. Is **K3s + Terraform + Ansible + GitHub Actions + Argo CD** justified for a *private pilot*, or should any part be deferred?
4. What are the **top three security or reliability risks** not covered here?
5. For logs and incident response, is **Grafana/Loki** sufficient? Would Dozzle add meaningful value?

**Review instruction:** Please comment on incorrect assumptions, unnecessary complexity, missing acceptance tests, and high-impact risks. Suggestions beyond P0/P1 should be listed as future enhancements rather than expanding the MVP.
