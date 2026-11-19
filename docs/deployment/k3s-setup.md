# Host and K3s setup

Target: one Debian 13 or Ubuntu 24.04 **amd64** VPS. K3s **v1.37.2+k3s1** (stable channel, 2026-11-19), installed from the release binary with its SHA-256 checked. Everything runs through `infrastructure/ansible/site.yml`; applying it to a real host requires owner authorization.

## Terraform

Not provided. The VPS provider is undecided (owner decision D1), and no provider-specific code is written before that. Once chosen, add `infrastructure/terraform/` only if the provider has a maintained Terraform provider: server, firewall, and DNS records with remote locked state. Until then the host is created by hand and Ansible takes over.

## Before running Ansible

1. Create the VPS with your SSH key and a sudo user (the provider's default user is fine).
2. Set up the VPN (WireGuard or Tailscale, owner decision D2) and note the node's VPN address.
3. `cp infrastructure/ansible/inventory.example.yml infrastructure/ansible/inventory.yml` (git-ignored) and fill in `ansible_host`, `ansible_user`, `node_ip`.
4. In `group_vars/all.yml`, set `admin_ssh_public_keys`, `admin_cidrs` (must include the address you connect from), `vpn_interface`, `vpn_cidr`.

## Run

```sh
cd infrastructure/ansible
ansible-playbook site.yml --check --diff   # review every change first
ansible-playbook site.yml                  # authorized change
```

| Role | Does |
|---|---|
| `base` | safe upgrade, unattended security updates, chrony, persistent size-capped journal |
| `ssh` | admin user with exclusive authorized keys, passwordless sudo; **lock-down only when `ssh_lockdown_confirmed: true`** |
| `firewall` | nftables input default-deny: SSH from `admin_cidrs`, 443/6443 on the VPN interface, pod/service CIDRs |
| `k3s` | kernel parameters, checksum-verified binary, config, Pod Security admission, API audit log, systemd unit; waits for the node |

## Lock-out protection

- **SSH:** the first run leaves password and root login alone. Confirm `ssh <admin_user>@<host>` works with your key in a second terminal, then rerun with `ssh_lockdown_confirmed: true`. The hardened config is checked with `sshd -t`; if it fails it is removed, and sshd is reloaded (not restarted), so existing sessions stay open.
- **Firewall:** the ruleset is syntax-checked before it is written. Applying it arms a systemd timer that restores the previous ruleset after **120 s** unless Ansible reconnects over SSH and cancels it. A rule that cuts SSH therefore reverts itself.
- **Last resort:** the provider's web console (serial/VNC) works regardless of SSH and firewall. Run `nft flush ruleset` there.

## K3s configuration

From `roles/k3s/templates/config.yaml.j2`:

| Setting | Why |
|---|---|
| `secrets-encryption: true` | Secrets encrypted at rest in the datastore |
| `cluster-init: true` + snapshot schedule every 6 h, 28 kept | embedded etcd, so `k3s etcd-snapshot` backups work |
| `protect-kernel-defaults: true` | kubelet refuses to run with unsafe kernel settings (set by the role) |
| `write-kubeconfig-mode: 0600` | admin kubeconfig readable by root only |
| `psa.yaml` | cluster default `baseline` enforced, `restricted` audit and warn; app namespaces enforce `restricted` |
| `audit-policy.yaml` | API audit log: metadata only for Secrets and ConfigMaps, full bodies for writes |

The API server listens on all addresses (K3s itself uses `127.0.0.1:6443`); only the firewall restricts 6443 to the VPN.

Remote kubectl: copy `/etc/rancher/k3s/k3s.yaml` over SSH, replace `127.0.0.1` with `node_ip`, keep it mode 600, and use it over the VPN only.

## Bootstrap (once, after the node is Ready)

```sh
kubectl apply --server-side -k infrastructure/argocd/install   # Argo CD v3.6.2 + Sealed Secrets v0.41.2
# back up the Sealed Secrets key now: docs/deployment/secrets.md
kubectl apply -k infrastructure/argocd                         # AppProject + Applications
```

From then on, changes reach the cluster only through Git (`gitops.md`).

## Verification status

- `ansible-playbook --syntax-check` and `ansible-lint` (production profile) pass.
- Role execution: **UNVERIFIED**. Needs a disposable amd64 VM; not run against any host.
- K3s settings in a cluster: **verified** on 2026-11-19 in the local test bed (`infrastructure/scripts/local-cluster.sh`: real K3s v1.37.2 in Docker, the same `psa.yaml` and `audit-policy.yaml`, minus host-only settings). `cluster-tests.sh` passed 27/27: Pod Security `restricted` rejects privileged and non-hardened pods, the quota rejects pods without resources, secrets encryption is enabled, 4 allowed and 10 denied network paths behave as written, TLS ingress works with no route to the API, and a workflow completes in the cluster.
- etcd snapshot and restore: **verified** (`etcd-restore-test.sh`, see `backup-restore.md`).
- On a loaded laptop, etcd loses leader election. `LOCAL_DATASTORE=sqlite local-cluster.sh up` uses K3s's SQLite datastore instead; everything except etcd snapshots then works. The test-bed-only settings (tmpfs etcd, relaxed timeouts, leader election off) are never used on the real node.
