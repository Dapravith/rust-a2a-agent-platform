# Deployment architecture (private pilot)

One K3s node, reachable only over a VPN. Supabase PostgreSQL stays external. Not highly available: a node failure is an outage, recovered from backups (`backup-restore.md`).

```
operator ── VPN ──▶ node :443  Traefik ──▶ portal :4321 ──▶ api :8080 ──▶ worker-agent :9101 ──▶ LLM provider :443
             │                                               │        └─▶ reviewer-agent :9102 ─▶ LLM provider :443
             └────▶ node :6443 Kubernetes API (kubectl, Argo CD UI via port-forward)
                                                             └──▶ Supabase Postgres :5432 (TLS verify-full)

GitHub Actions ─▶ GHCR (digest + provenance) ─▶ GitOps PR ─▶ human review ─▶ merge ─▶ Argo CD (pull) ─▶ K3s
```

## Workloads

| Workload | Replicas | Exposure | Talks to |
|---|---|---|---|
| portal (Astro SSR) | 1 | Ingress (Traefik, TLS), VPN only | api |
| api (Rust orchestrator) | **exactly 1** (instance lock; `Recreate`) | ClusterIP | agents, Postgres |
| worker-agent, reviewer-agent (A2A) | 1 each | ClusterIP | LLM provider (HTTPS) |
| api-migrate (Job, Argo CD PreSync) | per sync | none | Postgres (schema owner) |

Not deployed in the pilot: `role-agents` and `mcp-server`. The MCP server only binds loopback, its test runner needs a container runtime socket (host-root equivalent), LLM-written code needs a microVM boundary (ADR-004), and approvals are disabled without an identity provider. `ENG_PROVIDER` stays unset: engineering pages are read-only and writes return 503.

## Trust boundaries

| Boundary | Control |
|---|---|
| Internet → node | nftables default-deny; 443/6443 only on the VPN interface; SSH only from `admin_cidrs` |
| Pod → pod | NetworkPolicy default-deny (ingress + egress) with explicit paths; verified by `infrastructure/scripts/cluster-tests.sh` |
| Pod → host | Pod Security `restricted`: non-root, read-only rootfs, no capabilities, RuntimeDefault seccomp, no service-account token |
| App → secrets | DB credentials only in api/migrate; LLM key only in agents; runtime and schema-owner DB roles separated |
| Git → cluster | Argo CD AppProject: one repo, three namespaces, allow-listed kinds, no RBAC objects; production never auto-syncs |

**The portal is unauthenticated** (Phase 6D OIDC is not built). Anyone who reaches it can read workflows and start LLM runs, so the pilot must never get a public ingress.

## Environments

| | dev | staging | production |
|---|---|---|---|
| Status | pilot target | **UNVERIFIED** template | **UNVERIFIED** template |
| Sync | automatic (prune, self-heal) | manual | manual + deny window |
| Inference | `mock` | real (`llm-provider` secret) | real |
| Database | separate Supabase project | separate project | production project |

Namespaces on one node are not an isolation boundary between environments. Staging and production need their own node or cluster before they hold real data.

## Sizing (measured 2026-11-19, mock inference)

portal 47 MiB, api 3.6 MiB, worker 2.6 MiB, reviewer 1.7 MiB, idle CPU. Requests are set at 2–5× these values. Argo CD (dex and ApplicationSet scaled to 0), Sealed Secrets and K3s itself dominate. Plan 2 vCPU / 4 GiB with external monitoring, or 4 vCPU / 8 GiB with Prometheus, Grafana and Loki on the node (estimates, not measured on a VPS).
