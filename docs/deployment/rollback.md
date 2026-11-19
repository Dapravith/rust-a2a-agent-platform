# Rollback

A rollback is a Git change, like any deployment.

## Application images

```sh
git log -p -- infrastructure/kubernetes/overlays/dev/kustomization.yaml   # find the last good digests
git revert <gitops commit>                                                # or edit the digests back
gh pr create --fill                                                       # review, merge
```

Dev syncs automatically; staging and production need a manual sync after merge. The previous digests are still in GHCR with their provenance, so nothing is rebuilt. Don't roll back with `kubectl set image`: in dev, self-heal reverts it within minutes, and in staging and production it leaves the cluster out of sync with Git.

## Database schema: check before rolling back

Migrations are forward-only, and image rollback **does not** undo them. The API refuses to start against a schema newer than itself ("database has migration … unknown to this binary"); `db-restore-test.sh` tests this. Before rolling back across a release, compare `backend/migrations/` between the two commits:

| Migrations between the two releases | Rollback path |
|---|---|
| none | revert the digests |
| one or more | a forward fix (new image) is preferred. Otherwise restore the database to a backup taken before the migration (`backup-restore.md`), accept the data written since, then revert the digests |

All migrations so far are additive. Keep them that way, because a destructive migration makes the restore path the only one.

## In-flight work

On SIGTERM every process stops accepting connections and drains, within 30 s. A workflow still running in the API when it stops is marked `failed` with `interrupted` on the next start; workflows waiting for a human decision keep their state. The API's `Recreate` strategy means there is never an old and a new orchestrator at once. Approval records and audit tables are append-only and untouched by rollback.

## Verification status

The schema guard, the interrupted-workflow recovery (`restart_recovery_fails_interrupted_workflows`) and SIGTERM draining (`scripts/container-smoke.sh`) are tested. The GitOps revert reconciled by Argo CD is **UNVERIFIED** (no running cluster yet).
