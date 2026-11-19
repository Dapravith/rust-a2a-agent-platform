# Security

## Controls in place

| Area | Control | Evidence |
|---|---|---|
| Images | distroless, uid 65532, no shell, digest-pinned bases; build provenance attested | `trivy image`: 0 HIGH/CRITICAL; `container-smoke.sh` under read-only, cap-drop ALL |
| Pods | Pod Security `restricted` enforced, no SA tokens, no service links, read-only rootfs, resource limits, namespace quota | `validate.sh` (kubeconform + trivy config: 0 HIGH/CRITICAL) |
| Network | default-deny NetworkPolicy, explicit paths, no internet egress except agents → 443 and api → 5432 | enforced on K3s: `cluster-tests.sh` 27/27 (4 allowed, 10 denied paths) |
| Host | nftables default-deny, VPN-only 443/6443, key-only SSH, audit log, secrets encryption at rest | Ansible lint; execution **UNVERIFIED** |
| Supply chain | SHA-pinned actions, checksum-verified tools, `cargo deny`, `npm run audit`, gitleaks, trivy | CI config; GitHub runs **UNVERIFIED** until pushed |
| CI credentials | PR jobs: `contents: read`, no secrets; publishing only after CI passes on `main` and with `PUBLISH_IMAGES=true` | workflow files, actionlint |
| GitOps | AppProject allow-list; production manual-only with a deny window; CODEOWNERS | `validate.sh` fails if production auto-syncs; `argocd-local-test.sh` 14/14 (Role refused, deny window holds) |
| Approvals | human-only, fail closed without an IdP (writes 503) | Phase 6 tests, unchanged |

## Open risks (ranked)

| # | Risk | Status |
|---|---|---|
| 1 | **Portal and API reads are unauthenticated** (no OIDC until Phase 6D). Anyone who reaches the portal can read every workflow and start LLM runs (cost) | Mitigated only by network placement: VPN-only, no public ingress. Blocks any public deployment |
| 2 | Argo CD's application controller and the Sealed Secrets controller hold cluster-wide privileges (upstream design; trivy reports 5 CRITICAL / 5 HIGH in the upstream bundle) | Accepted for the pilot. Argo UI is not exposed, admin password must be rotated, AppProject restricts what Git can deploy. Hardening option: Argo CD namespace-scoped install |
| 3 | Single node: environments on one node are not isolated; a node compromise reaches every namespace | Only dev is deployed. Staging and production need separate nodes |
| 4 | External egress is IP-based (no FQDN policy): agents may reach any public host on 443, the api any host on 5432 | Accepted; narrow `database-egress` to the Supabase CIDR when known. Cilium would allow FQDN rules |
| 5 | LLM-written code execution | **Disabled** in the pilot (no role-agents/mcp-server, `ENG_PROVIDER` unset). Requires a microVM boundary before enabling (ADR-004) |
| 6 | Rust dependencies inside images are invisible to trivy | covered by `cargo deny`; follow-up: `cargo auditable` builds |
| 7 | `chrono` unmaintained (RUSTSEC-2026-0543, not a vulnerability) | ignored with review date 2027-02-28 |

## Reviewed scanner findings (not suppressed, below the failure threshold)

- KSV-01010 "sensitive content in ConfigMap" on `api-config`: the matched key is `AGENT_TIMEOUT_SECS`. A name match, not a secret.
- KSV-0125 "untrusted registry": `ghcr.io` isn't in trivy's default list; images are digest-pinned and attested.
- KSV-0040 "resource quota usage": a ResourceQuota exists per namespace; the check reports per resource.

## Before any public exposure (all required)

Phase 6D identity (OIDC) on the portal and API, a separate production node, an FQDN-capable egress policy or a provider egress proxy, and the alert delivery and restore drills from `monitoring.md` and `backup-restore.md` executed.
