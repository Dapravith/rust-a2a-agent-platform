# Backup and restore

A backup counts only once a restore from it has been tested.

| What | Where it lives | Backup | Restore tested |
|---|---|---|---|
| Application data | Supabase Postgres | Supabase backups (plan-dependent: confirm daily backups / PITR in the dashboard, owner decision D7) **and** a nightly `pg_dump -Fc` kept off-provider | **yes**, logical dump: `infrastructure/scripts/db-restore-test.sh` |
| Kubernetes state | K3s embedded etcd | `k3s etcd-snapshot`, every 6 h, 28 kept, copied off-node | **yes**: `infrastructure/scripts/etcd-restore-test.sh` |
| Sealed Secrets key | `kube-system` Secret | manual export, offline and encrypted (`secrets.md`) | **UNVERIFIED** |
| Manifests, config history | Git | GitHub (+ any clone) | n/a |
| Release artifacts | GHCR, by digest, with provenance | registry retention | n/a |

## Application database

Logical backup:

```sh
pg_dump "$DATABASE_MIGRATION_URL" -Fc --no-owner -f a2a-$(date -u +%Y%m%dT%H%MZ).dump
```

Keep ACLs (do **not** pass `--no-acl`). The runtime role's GRANTs and the row-level-security policies are part of the security model.

Restore into a new database:

1. Re-create the cluster-level group roles the migrations define (`pg_dump` doesn't include roles):
   `CREATE ROLE a2a_app NOLOGIN; CREATE ROLE a2a_mcp NOLOGIN;`
2. `pg_restore -d <new-db> --no-owner --exit-on-error <file>.dump`
3. Create the login role with a **new** password (`CREATE ROLE a2a_api LOGIN PASSWORD '…' IN ROLE a2a_app;`), seal the new URL (`secrets.md`) and point the overlay at the new database.
4. `api migrate` must report "database schema is up to date"; `api serve` verifies the schema at start.

What `db-restore-test.sh` proved on 2026-11-19 (disposable containers): a workflow created through the real API and agents survived dump and restore into a fresh Postgres. Row counts matched, the runtime-role grants and all 11 RLS policies were present, `migrate` had nothing to do, and the restored API served the same draft, review and runs. The test also caught that a dump-and-restore without step 1 fails (`role "a2a_app" does not exist`).

## K3s

On-demand snapshot before risky changes, plus the schedule from the Ansible role:

```sh
k3s etcd-snapshot save --name pre-change
k3s etcd-snapshot ls
```

Copy `/var/lib/rancher/k3s/server/db/snapshots/` and `/var/lib/rancher/k3s/server/token` off the node (the token is needed to restore, and the snapshot contains encrypted Secrets). Then write `a2a_backup_last_success_timestamp_seconds` to the monitoring backend; `A2ABackupMissing` alerts on its absence.

What `etcd-restore-test.sh` proved on 2026-11-19 (K3s v1.37.2, embedded etcd, secrets encryption on). A snapshot was saved and the test Secret's value does not appear in plaintext in it. A ConfigMap and a Secret deleted after the snapshot came back after `--cluster-reset-restore-path`, the restored Secret decrypted, and encryption was still enabled. The restore must run under the same node name: run it on the node itself.

Restore (single node): stop K3s, then `k3s server --cluster-reset --cluster-reset-restore-path=<snapshot>` with the original token in place, then start K3s. Because Git is the source of truth, rebuilding the node, re-running Ansible and re-bootstrapping Argo CD (with the Sealed Secrets key restored first) is an equally valid path. The etcd snapshot mostly saves time.

## Not claimed

Disaster recovery readiness is not claimed. Both restores above ran in disposable local environments, not on the real node. The Sealed Secrets key restore and the off-node snapshot copy have not been executed, and the Supabase-native restore depends on the plan in use.
