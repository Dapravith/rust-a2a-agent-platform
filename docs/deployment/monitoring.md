# Monitoring

## Metrics

The API serves Prometheus text at `GET /metrics` on port 8080. Port 8080 admits only the portal pod and the `monitoring` namespace (NetworkPolicy), and there is no ingress route to the API. The portal never forwards arbitrary paths, so `/metrics` isn't reachable through it.

| Metric | Labels (all bounded) |
|---|---|
| `a2a_database_up` | none (pinged at scrape, 2 s timeout) |
| `a2a_http_requests_total` | `route` (template, e.g. `/api/workflows/{id}`), `method`, `status_class` |
| `a2a_http_request_duration_seconds` | `route` |
| `a2a_workflows_total` | `outcome`, `error_code` (stable codes) |
| `a2a_agent_calls_total` | `agent` (`worker`/`reviewer`), `outcome`, `error_code` |
| `a2a_agent_call_duration_seconds` | `agent` |

Workflow, run and user IDs are never labels; a test asserts that no workflow ID appears in the output. Pod and node metrics (restarts, CPU, memory, disk) come from kube-state-metrics and node-exporter.

Prometheus scrape job:

```yaml
- job_name: a2a-api
  kubernetes_sd_configs: [{ role: endpoints, namespaces: { names: [a2a-dev] } }]
  relabel_configs:
    - { source_labels: [__meta_kubernetes_service_name], regex: api, action: keep }
```

## Alerts

`infrastructure/observability/alerts.yaml` (12 rules), unit-tested by `alerts-test.yaml` in CI (`promtool test rules`).

| Alert | Fires when | First response |
|---|---|---|
| <a id="a2aapidown"></a>`A2AApiDown` | API not scrapeable for 2 min | `kubectl -n a2a-dev get pods`; `kubectl logs deploy/api`. CrashLoop with "unknown to this binary" means the schema is newer than the image (`rollback.md`); "instance lock" means two API processes |
| <a id="a2adatabaseunreachable"></a>`A2ADatabaseUnreachable` | API can't query Postgres for 2 min | Supabase status page and dashboard; check the CA and `verify-full` URL; check NetworkPolicy `database-egress` if the DB address changed |
| `A2APodRestarting` | ≥ 3 restarts in 15 min | `kubectl describe pod`: OOMKilled means raise the limit; probe failures mean check `/healthz` |
| <a id="a2aworkflowsfailing"></a>`A2AWorkflowsFailing` | ≥ 3 failed workflows in 30 min | group by `error_code`: `agent_*` means agents or network; `llm_*` means provider |
| `A2AAgentErrorRateHigh` | > 25 % failed A2A calls (≥ 4 calls) in 15 min | agent pod logs; `/api/agents` discovery status |
| `A2AProviderFailures` | ≥ 3 `llm_*` failures per agent and code in 30 min | provider status, key validity, spend limit, rate limits |
| <a id="a2aunauthorizedapprovalattempts"></a>`A2AUnauthorizedApprovalAttempts` | ≥ 5 rejected POSTs to `/api/eng/*` in 15 min | correlate with `eng_audit_events` and the ingress logs before acting; a new IP alone is not evidence of compromise |
| `A2ARequestErrorsHigh` | > 5 % 5xx (≥ 20 requests) in 10 min | API logs by `request_id` |
| `NodeMemoryPressure`, `NodeCpuSaturated`, `NodeDiskFillingUp` | sustained resource exhaustion | `kubectl top`; etcd snapshots and the audit log are the usual disk consumers |
| `A2ABackupMissing` | no backup success timestamp for 8 h | `backup-restore.md` |

Alert *delivery* (email, chat, phone) depends on the chosen stack and is **UNVERIFIED** until one exists.

## Logs

All four services log one JSON object per line to stdout (`LOG_FORMAT=json`): `timestamp`, `level`, `target`, `fields.message`, and spans carrying `request_id` (HTTP) and `workflow_id` (workflow execution). Failures add `error_code` and `stage`. The service name and environment come from the Kubernetes pod labels and namespace at collection time.

Never logged: API keys (config `Debug` impls redact them), database URLs, authorization headers, prompt or answer bodies at `info`. Agent-run IDs are stored in the database (`agent_runs`) but not yet emitted as a log field: a gap, low priority.

Retention: container logs rotate with K3s defaults on the node. In a log backend, keep 14 days for dev and 30 days for production.

## Stack options (owner decision D3)

| Profile | Runs on the node | Extra RAM (estimate) |
|---|---|---|
| Minimal | nothing; `kubectl top`, `kubectl logs`, plus a hosted Prometheus/Loki-compatible backend that Alloy or an agent pushes to | ~0.1–0.2 GiB (agent) |
| Self-hosted | Prometheus, Alertmanager, Grafana, Loki (single binary), Alloy, kube-state-metrics, node-exporter | 1–2 GiB, needs the 8 GiB node |

No monitoring stack is installed by this repository yet. Only the API metrics, alert rules and their tests exist.
