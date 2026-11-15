# Deploying on Vercel and Fly.io

The web application is static and is served by Vercel. The backend (API,
analysis worker, PostgreSQL) needs long-running processes, git, the Rust
engine and a persistent disk, which Vercel Functions do not provide, so it
runs on Fly.io. Vercel proxies `/api/*` to Fly, so the browser talks to one
origin and session cookies stay first-party.

```
browser ──▶ Vercel (apps/web/dist) ──/api/*──▶ Fly.io machine
                                               ├─ usie-api
                                               ├─ usie-worker ─▶ usie-engine
                                               └─ PostgreSQL (Unix socket only)
                                               volume /data: database, blobs
```

## Backend (Fly.io)

`fly.toml` at the repository root builds the `single-machine` stage of
`infrastructure/docker/Dockerfile` remotely and mounts a volume at `/data`.
The entrypoint (`infrastructure/docker/fly-start.sh`) starts PostgreSQL with
no TCP listener, then runs the API and worker as an unprivileged user. If
either process exits, the machine exits and Fly restarts it.

```sh
fly apps create usie-f52a60d5
fly volumes create usie_data --region iad --size 10
fly deploy --remote-only
```

To use an external PostgreSQL instead (for example a managed one), set the
secret `fly secrets set USIE_DATABASE_URL=postgres://…`; the embedded
database is then not started.

This is a **single-machine** deployment: one volume, no failover. Fly takes
daily volume snapshots; restore them to recover. Scaling out requires an
external database and an object-storage blob backend (ADR 0004).

### First deployment checklist (run on your own computer)

1. Install the Fly CLI: `brew install flyctl` (macOS),
   `curl -L https://fly.io/install.sh | sh` (Linux) or
   `iwr https://fly.io/install.ps1 -useb | iex` (Windows PowerShell).
2. `fly auth login` (or `fly auth signup`), then add a payment card under
   Billing in the Fly dashboard.
3. `git clone --branch ccr-352cfc47-nqw12y https://github.com/ammarkarimi/usie.git && cd usie`
4. `fly apps create usie-f52a60d5` — the name must match `fly.toml` and the
   rewrite in `apps/web/vercel.json`.
5. `fly volumes create usie_data --region iad --size 10` (accept the
   single-volume warning).
6. `fly deploy --remote-only` (the first build compiles the Rust engine and
   takes a while).
7. `fly machine list` must show exactly one machine; otherwise
   `fly scale count 1`.
8. `curl https://usie-f52a60d5.fly.dev/readyz` should return
   `{"status":"ready"}`; use `fly logs` if not.
9. Immediately create the first account at https://usie-eight.vercel.app:
   in `first-user` mode the first registration becomes the owner.
10. Optional — AI answers: `fly secrets set OPENAI_API_KEY=<key>` (Fly
    restarts the machine). Without it, the Ask page says AI is not enabled.
    Answers are generated asynchronously and polled, so Vercel's proxy
    timeout does not limit them.

## Frontend (Vercel)

A Vercel project with root directory `apps/web` uses `apps/web/vercel.json`:
it installs the pnpm workspace, builds the app, rewrites `/api/*` to the Fly
app and everything else to `index.html`, and sets the same security headers
as the API.

## Settings specific to this topology

| Setting | Value | Why |
|---|---|---|
| `USIE_TRUSTED_PROXY_HOPS` | `2` | Clients reach the API through Vercel and the Fly proxy. |
| `USIE_MAX_UPLOAD_BYTES` | `4000000` | Uploads pass through Vercel's proxy. |
| `auto_stop_machines` | `off` | The worker runs in the same machine. |

Sign-in is also rate limited per account, independent of client addresses.
