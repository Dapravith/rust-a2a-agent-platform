# Secrets

**Sealed Secrets** (controller v0.41.2, installed by `infrastructure/argocd/install`). Values are encrypted with the cluster's public key on an operator machine, and only the encrypted `SealedSecret` is committed. The controller in the cluster decrypts it into a normal Secret, which K3s stores encrypted at rest (`secrets-encryption: true`). base64 in a plain Secret is encoding, not encryption, and plain Secrets are never committed (gitleaks and `check-secrets.sh` run in CI).

## Inventory

| Secret | Key | Used by | Contents |
|---|---|---|---|
| `api-database` | `url` | api | runtime role (`a2a_app` member): DML only, no DDL |
| `api-database-migration` | `url` | api-migrate Job | schema owner |
| `llm-provider` | `anthropic-api-key` | worker, reviewer (staging, production) | provider key with a spend limit |
| `portal-tls` | `tls.crt`, `tls.key` | Traefik | portal certificate (source: owner decision D4) |

The DB URLs must be `postgres://...?sslmode=verify-full&sslrootcert=/etc/a2a/db-ca/ca.crt`, on Supabase's direct connection or session pooler (the API holds a session-level advisory lock). The CA certificate is public and lives in each overlay's `db-ca.yaml`. GitHub delivery and approval-system credentials don't exist yet (Phase 6D). When they arrive, each gets its own Secret, mounted only where it is used.

## Create or rotate

```sh
kubeseal --fetch-cert --controller-namespace kube-system > pub.pem   # public key, safe to keep
kubectl create secret generic api-database -n a2a-dev \
  --from-literal=url='postgres://...' --dry-run=client -o yaml \
| kubeseal --cert pub.pem --format yaml > infrastructure/kubernetes/overlays/dev/sealed-api-database.yaml
```

Add the file to the overlay's `resources:` and merge through a PR. Sealed values are bound to their name and namespace, so a dev secret cannot be replayed into production.

Rotation: issue the new credential at the source (Supabase role password, provider console), seal it, merge, sync, confirm the pods restarted and are healthy, then revoke the old one at the source. Pods read Secrets at start; `kubectl rollout restart` makes them pick up a change.

## Key ownership and recovery

The controller's private key is the root of all committed secrets. Back it up right after bootstrap and after every key renewal (every 30 days by default; old keys are kept, so older SealedSecrets still decrypt):

```sh
kubectl -n kube-system get secret -l sealedsecrets.bitnami.com/sealed-secrets-key -o yaml > sealed-secrets-keys.yaml
```

Keep that file **offline and encrypted** (for example `age`-encrypted on two separate media held by the owner), never in Git or CI.

| Event | Consequence and action |
|---|---|
| Node rebuilt, key backup available | Apply the key backup **before** installing the controller; every SealedSecret decrypts unchanged |
| Key lost | Committed SealedSecrets can never be decrypted again. Re-create every credential at its source, reseal, and replace all sealed files |
| Key leaked | Treat every sealed value as exposed: rotate all credentials at their source, then rotate the sealing key (`--key-renew-period` / delete the key and restart the controller) and reseal |
| A credential leaked | Rotate it at the source first, then reseal and merge |

Access: only the owner holds cluster-admin and the key backup. Pods have no service-account tokens, and the AppProject cannot create Secrets directly.

## Verification status

Verified in the local test bed: five test credentials were sealed with `kubeseal` against the in-cluster controller, committed to the test Git server (no plaintext in the sealed files), and unsealed into Secrets that the pods used; the AppProject never needed `Secret` permission. Not verified: the key backup and restore onto a rebuilt cluster.
