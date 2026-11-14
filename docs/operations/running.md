# Running USIE

## Local development

Requirements: Rust 1.90+, Go 1.24+, Node 20+ with pnpm 10, git, and
PostgreSQL 14+ server binaries (`initdb`, `pg_ctl`).

```sh
make build      # engine (release), Go services, web app
make dev        # disposable PostgreSQL + API + one worker on http://127.0.0.1:8080
make test       # Rust, Go (integration tests included) and web tests
make lint
```

`scripts/test-db.sh start` creates a throwaway cluster under
`/tmp/usie-pg` listening on `127.0.0.1:54329` with trust authentication.
It is for development and tests only.

For front-end work, run `pnpm dev` (Vite on :5173, proxying `/api` to :8080)
alongside `make dev`.

## Containers

```sh
docker compose -f infrastructure/docker/compose.yaml up --build
```

starts PostgreSQL, the API (serving the web app on `127.0.0.1:8080`) and a
worker with dropped capabilities, a read-only root filesystem and memory and
process limits. The image and compose file are exercised by the CI `image`
job; they were not built in the environment where they were written.

## Health and metrics

| Endpoint | Meaning |
|---|---|
| `GET /healthz` | process is up |
| `GET /readyz` | database reachable |
| `GET /metrics` | Prometheus: `usie_http_requests_total`, `usie_http_request_duration_seconds`, `usie_jobs_queued`, `usie_jobs_running`, Go and process metrics |
| worker `/metrics` | `usie_worker_jobs_finished_total{outcome}`, `usie_worker_job_duration_seconds` (with `USIE_WORKER_METRICS_ADDR`) |

Every request is logged as one JSON line with request id, route, status,
duration, and the user and organization when authenticated. The request id
is returned in `X-Request-Id` and in error bodies.

## Operational behaviour

- **Worker crash or restart:** its jobs' leases expire and they are
  re-queued (up to `max_attempts`, default 3); a job out of attempts fails
  with an explicit error.
- **Shutdown:** `SIGTERM` stops claiming; running jobs are left to lease
  expiry and retried by another worker.
- **Permanent failures** (invalid URL, limit exceeded, hostile archive) are
  not retried.
- **Backups:** PostgreSQL and the data directory. Parsed IR in the blob store
  is a cache and can be regenerated; source blobs are needed to show file
  content for existing versions.
