# P1 plan: private remote pilot (planning only, not implemented)

P1 starts only after P0 is accepted. No infrastructure has been provisioned and nothing has been deployed.
Every step below that touches a remote host, cloud account, DNS, or secrets needs explicit approval.

## Recommendation: stage P1, defer what a single private host doesn't need yet

| Item (PRD §4) | Recommendation | Reason |
|---|---|---|
| Container images + GitHub Actions publish | **P1a, do first** | Needed by everything else; small |
| Trivy (images, deps, IaC) in CI | **P1a** | Cheap, high value |
| Ansible host hardening (SSH keys only, firewall, unattended upgrades, fail2ban/auditd) | **P1a** | One host, repeatable |
| K3s (single node) | **P1a** | Gives Kubernetes audit logs + NetworkPolicy, which the alert requirements reference |
| Prometheus + Grafana + Loki + Alloy | **P1b** | Required for the PRD's alerts; run as one small stack |
| Argo CD | **P1c / defer** | For one node and one app, `kubectl apply` from a gated GitHub Actions job is simpler. Adopt Argo CD when there are ≥ 2 environments |
| Terraform | **Defer** unless the provider has a supported provider (VM, DNS, firewall) | Avoids state management for one VM. Supabase stays managed |
| Dozzle | **Skip** | Loki + Grafana already cover log search; Dozzle adds an unauthenticated UI surface |

## Deployment shape

- Namespaces: `a2a` (orchestrator, worker, reviewer) and `observability`.
- Only the orchestrator Service is exposed, via ingress with TLS and restricted operator access
  (VPN, Tailscale, or an IP allowlist plus an auth proxy). Worker and Reviewer are ClusterIP only,
  with a `NetworkPolicy` allowing ingress from the orchestrator pod alone.
- Secrets: Kubernetes Secrets from an external source (SOPS or a sealed secret). `ANTHROPIC_API_KEY`
  goes only into agent pods; `DATABASE_URL` only into the orchestrator.
- Supabase: session pooler URL, `sslmode=verify-full`, a dedicated DB role (not `postgres`) owning
  only these tables; PITR/backups enabled on the Supabase plan.

## Gaps P0 deliberately leaves for P1

1. **Agent authentication.** Agents rely on network isolation (localhost in P0, NetworkPolicy in P1).
   Add a shared bearer token or mTLS between orchestrator and agents, declared in the Agent Card's
   `securitySchemes` and enforced with the SDK's `RequestAuthorizer`.
2. **Operator authentication for the dashboard/API.** P0 binds to `127.0.0.1` with no login.
   P1 needs SSO or an auth proxy before any remote exposure.
3. **Rate limiting / LLM spend caps.** One active workflow bounds concurrency, but not daily cost.
4. **Readiness probes.** Add `/api/ready` that checks the DB and agent cards.

## Alerts (PRD §5) mapped to signals

| Alert | Signal |
|---|---|
| Service down | Prometheus `up == 0` / probe failure for each Deployment |
| A2A errors | Count of agent runs that fail (log-derived metric from structured `outcome="failed"`), plus agent task `failed`/`rejected` |
| Database outage | `/api/health` 503, SQLx pool errors in logs |
| Repeated auth failures | SSH (journald via Alloy), ingress 401/403 rate |
| Admin access from outside approved networks | Successful SSH or kube-apiserver audit events from IPs outside the allowlist, **correlated with identity**. Per PRD §5, an unfamiliar IP is a signal to investigate, not proof of an attack |
| Resource pressure | Node CPU, memory, disk, and pod restarts |

Before P1 is called ready: fire each alert in a test, run the incident runbook once, demonstrate an
image rollback (previous tag) and a Supabase restore into a scratch project.

## P1 definition of done (from PRD §7)

- A private remote deployment works through approved CI/CD.
- Management access is restricted.
- Key alerts deliver successfully.
- Rollback and restore have been demonstrated.
