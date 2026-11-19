# GitOps

Git is the only way application changes reach the cluster. GitHub Actions never runs `kubectl` against it; Argo CD pulls from `main`.

```
merge to main ─▶ CI ─▶ release.yml: build, scan, attest, push digests ─▶ PR "gitops(dev): images from <sha>"
   ─▶ review + merge ─▶ Argo CD syncs a2a-dev automatically
   ─▶ promote.sh dev staging   ─▶ PR ─▶ review + merge ─▶ manual sync a2a-staging
   ─▶ promote.sh staging production ─▶ PR ─▶ owner approval (CODEOWNERS) ─▶ merge ─▶ manual sync a2a-production
```

A merged source PR deploys nothing beyond dev, and dev only after its own GitOps PR is merged.

## Argo CD objects (`infrastructure/argocd/`)

| Object | Policy |
|---|---|
| `install/` | Argo CD v3.6.2 at its release commit, Sealed Secrets v0.41.2 vendored; every image pinned by digest; dex and ApplicationSet scaled to 0; anonymous access, UI exec and default RBAC role off; no Ingress (`kubectl -n argocd port-forward svc/argocd-server 8443:443` over the VPN) |
| `projects/a2a.yaml` | source: this repo only; destinations: `a2a-dev`, `a2a-staging`, `a2a-production`; cluster-scoped kinds: Namespace only; namespaced kinds allow-listed (no Role, RoleBinding or Secret); orphaned resources warned |
| `a2a-dev` | automated, `prune` + `selfHeal` (manual drift is reverted), retries with backoff |
| `a2a-staging` | manual sync |
| `a2a-production` | manual sync; a permanent deny window blocks automatic sync even if `automated:` were added; `validate.sh` fails if it is |

Pruning: dev prunes automatically. That is acceptable because every dev object is in Git and the database is external. In staging and production, review the sync diff and prune explicitly.

## Promotion

```sh
git switch -c promote/staging-$(date +%Y%m%d)
infrastructure/scripts/promote.sh dev staging     # or: staging production
git commit -am "promote: dev -> staging" && git push -u origin HEAD
gh pr create --fill
```

`promote.sh` copies the four image digests and nothing else. It refuses all-zero (unreleased) digests and any order other than dev → staging → production. The PR shows exactly which digests change; provenance for each is verifiable with `gh attestation verify` (`ci-cd.md`).

After merge: Argo CD UI or `argocd app sync a2a-staging`. Health is green only when every Deployment is ready, which means the API's `/api/health` reaches the database.

## Owner actions to enable this

1. Branch protection on `main`: require PRs, the CI checks, and **review from Code Owners** (`.github/CODEOWNERS` covers production, staging, base, argocd, ansible, workflows and migrations). As the only maintainer you cannot approve your own PR, so either add a second reviewer or knowingly use admin override for production promotions.
2. A GitHub App with contents + pull-requests write on this repository only; set `GITOPS_APP_ID` (variable) and `GITOPS_APP_PRIVATE_KEY` (secret). Without them `release.yml` publishes images but opens no PR.
3. The repository is public, so Argo CD needs no repository credential. Nothing secret is ever committed (`secrets.md`).

## Drift and reconciliation

Argo CD compares every 180 s. In dev, any `kubectl edit` is reverted. In staging and production it shows `OutOfSync`, and the fix is either a Git change or a manual sync back to Git.

## Verification status

`validate.sh` renders and schema-checks every overlay and the Argo CD objects. `promote.sh` was tested on a copy of the repo: order enforcement, zero-digest refusal, idempotency, digest-only diff.

`infrastructure/scripts/argocd-local-test.sh` passed **14/14** on the local K3s test bed (2026-11-19). It installs `install/` as written, syncs through the real AppProject policy from a throwaway Git server holding a snapshot of the working tree, and checks:
- the app reaches Synced/Healthy, SealedSecrets are unsealed, and the migration runs in wave 1;
- a workflow completes through the deployed stack;
- a manual `kubectl scale` is reverted;
- a Git commit is applied and its `git revert` rolls it back;
- a Role added to Git is refused ("not permitted in project a2a");
- an app named `a2a-production` with `automated:` is held `OutOfSync` by the deny window.

Not verified: the real GitHub repo as source, which needs these files pushed to `main`, and promotion PRs through branch protection.

Two defects were found and fixed by this test: Argo CD was rendered into `default`, because upstream `install.yaml` has no namespace (`validate.sh` now fails on that); and a `PreSync` migration deadlocks a first sync, because the hook runs before its namespace and secrets exist (now a wave-1 `Sync` hook).
