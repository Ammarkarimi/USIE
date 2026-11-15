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
| `OPENAI_API_KEY` | — | Enables AI answers through OpenAI (API process only). Store it as a secret. |
| `ANTHROPIC_API_KEY` | — | Alternative: AI answers through Claude, used when no OpenAI key is set. |
| `USIE_AI_PROVIDER` | `openai` if `OPENAI_API_KEY` is set, else `anthropic` if `ANTHROPIC_API_KEY` is set, else `none` | `openai`, `anthropic` or `none`. |
| `USIE_AI_MODEL` | `gpt-6-astra` (OpenAI) / `claude-opus-5-5` (Anthropic) | Model identifier sent to the provider. It must support structured outputs. |
| `USIE_AI_EFFORT` | `medium` | Reasoning effort: `low`, `medium`, `high`, `xhigh` or `max` (OpenAI also accepts `none` and `minimal` where the model does). `off` sends none, for models without reasoning. Higher costs more and takes longer. |
| `USIE_AI_DAILY_TOKEN_BUDGET` | `2000000` | Tokens per organization per UTC day (input, output and cache reads). |
| `USIE_AI_USER_REQUESTS_PER_HOUR` | `30` | Questions per user per hour. |
| `USIE_AI_MAX_CONCURRENT` | `4` | Answers generated at the same time by one API process. |
