# Enabling verification runs

Verification builds and tests AI-proposed changes on workers, inside a
bubblewrap sandbox (ADR 0011). It is **off by default**.

## Requirements on each worker host

1. `bwrap` (package `bubblewrap`) and `prlimit` (package `util-linux`). The
   USIE image installs both.
2. Unprivileged user namespaces. Check with
   `unshare --user --map-root-user true`; on some distributions this needs
   `sysctl kernel.unprivileged_userns_clone=1` (Debian) or
   `kernel.apparmor_restrict_unprivileged_userns=0` (Ubuntu 24.04+). In
   containers, the runtime's seccomp profile may block `unshare`; Docker's
   default profile allows it only with `--security-opt seccomp=unconfined`,
   which weakens the container — prefer running workers on a VM.
3. The toolchains of the profiles you want, visible in the sandbox: under
   `/usr`, or elsewhere and listed in `USIE_SANDBOX_READONLY_PATHS`
   (for example `/opt/go:/opt/jdk`). The USIE image includes `python3`
   only, so only the Python profiles run out of the box.
4. Run the worker as an unprivileged user (the image does), so the process
   limit applies.

## Enable

Set on the workers:

```sh
USIE_SANDBOX=bubblewrap
# optional
USIE_SANDBOX_TIMEOUT=10m
USIE_SANDBOX_MEMORY_BYTES=8589934592
USIE_SANDBOX_READONLY_PATHS=/opt/go
```

At start, the worker runs `true` in the sandbox. If that fails, it logs
`verification sandbox unavailable` with the reason, keeps analyzing
repositories, and verification runs fail with that reason. Workers report
their sandbox and toolchains as capabilities; the Changes page shows which
profiles can run.

On Fly.io (`docs/operations/deploy-vercel-fly.md`):
`fly secrets set USIE_SANDBOX=bubblewrap`. Fly Machines are Firecracker VMs
with their own kernel; whether unprivileged user namespaces are available
there has not been tested by this project — check the worker log after
deploying.

## Network

`USIE_SANDBOX_NETWORK` is `false` by default: sandboxed code cannot reach
the network, so builds that must download dependencies fail (offline flags
make the failure immediate). Setting it to `true` lets tested code download
dependencies — and send the repository anywhere or reach internal services.
Only enable it where worker egress is otherwise restricted.

## What a run does

1. Writes the version's stored sources to a job directory (binary and
   oversize files are not stored by the analysis, so they are missing; the
   run reports how many).
2. With *baseline* (the default), runs the profile on the unchanged tree.
3. Applies the proposal's files and runs the profile again.
4. Stores each step's exit code, duration, clipped output and parsed test
   counts, and the tests fixed or newly failing.

At most three runs per organization are queued or running at once.
