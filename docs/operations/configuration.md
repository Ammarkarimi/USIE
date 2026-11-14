# Configuration

Both `usie-api` and `usie-worker` read environment variables. Invalid
values stop the process at startup with a message naming the variable.

| Variable | Default | Purpose |
|---|---|---|
| `USIE_DATABASE_URL` | — (required) | PostgreSQL connection URL. Migrations run automatically. |
| `USIE_DATA_DIR` | `.data` | Blob store (`blobs/`) and worker scratch space (`work/`). Must be shared by the API and workers. |
| `USIE_ENGINE_BIN` | `usie-engine` | Path to the analysis engine. Required by workers; optional for the API. |
| `USIE_HTTP_ADDR` | `:8080` | API listen address. |
| `USIE_WEB_DIR` | — | Directory of the built web app to serve. |
| `USIE_LOG_LEVEL` | `info` | `debug`, `info`, `warn`, `error`. Logs are JSON on stdout. |
| `USIE_SIGNUP_MODE` | `first-user` | `first-user`, `open` or `closed`. |
| `USIE_COOKIE_SECURE` | `true` | Set `false` only for plain-HTTP local use. |
| `USIE_SESSION_TTL` | `12h` | Session lifetime. |
| `USIE_ALLOWED_LOCAL_ROOTS` | — | `:`-separated directories local repositories may live under. Empty disables local import. |
| `USIE_GIT_ALLOW_PRIVATE_HOSTS` | `false` | Allow cloning from private/loopback addresses. |
| `USIE_GIT_TIMEOUT` | `10m` | Clone timeout. |
| `USIE_MAX_UPLOAD_BYTES` | 200 MiB | Upload size limit. |
| `USIE_MAX_REPO_FILES` | 200000 | Files per repository version. |
| `USIE_MAX_REPO_BYTES` | 2 GiB | Bytes per repository version. |
| `USIE_MAX_FILE_BYTES` | 2 MiB | Larger files are listed but not stored or parsed. |
| `USIE_WORKER_CONCURRENCY` | `1` | Jobs per worker process. |
| `USIE_JOB_LEASE` | `2m` | Job lease; heartbeats run at a third of it. |
| `USIE_PARSE_TIMEOUT` | `10s` | Per-file parse budget. |
| `USIE_ENGINE_TIMEOUT` | `30m` | Budget for one engine run. |
| `USIE_RATE_LIMIT_PER_MINUTE` | `600` | Requests per client IP. |
| `USIE_AUTH_RATE_LIMIT_PER_MINUTE` | `10` | Login/registration attempts per client IP. |
| `USIE_WORKER_METRICS_ADDR` | — | If set, the worker serves `/metrics` and `/healthz` here. |
