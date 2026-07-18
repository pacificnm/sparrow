# Sparrow deployment

Deployment artifacts for running Sparrow outside a dev checkout. Grows across
Phase 12 (security hardening) and Phase 13 (packaging/deployment) — only
what those phases have actually delivered so far is listed below.

## Quick start (Issue 13.3 — both acceptance scenarios, verified end to end)

Run from the **nest repo root** (`apps/sparrow` is this checkout, cloned
per [`apps/README.md`](../../../apps/README.md)'s sibling-checkout
convention — see "Docker build context" below for why that matters):

```bash
# 1. Certs — CN/SAN must include "mosquitto" (the Compose service name),
#    not generate-certs.sh's own default "mosquitto.local".
apps/sparrow/deploy/mosquitto/generate-certs.sh mosquitto mosquitto localhost 127.0.0.1

# 2. Credentials — one call per principal: the server, and one per agent.
apps/sparrow/deploy/mosquitto/provision-user.sh sparrow-server <server-password>
apps/sparrow/deploy/mosquitto/provision-user.sh <agent-host-id> <agent-password>

# 3. Postgres secret (gitignored).
mkdir -p apps/sparrow/deploy/secrets
echo -n '<postgres-password>' > apps/sparrow/deploy/secrets/postgres_password.txt

# 4. Server config (gitignored) — fill in mqtt_password matching step 2's
#    sparrow-server password.
cp apps/sparrow/deploy/server.toml.example apps/sparrow/deploy/server.toml

# 5. Bring up the stack.
docker compose -f apps/sparrow/deploy/docker-compose.yml up
```

**`docker compose` not installed?** Confirmed this sandbox didn't have it —
neither the `docker-compose` standalone binary nor the `compose` CLI
plugin. Installed the official static plugin binary with no root needed:

```bash
mkdir -p ~/.docker/cli-plugins
curl -sL "https://github.com/docker/compose/releases/latest/download/docker-compose-linux-x86_64" \
  -o ~/.docker/cli-plugins/docker-compose
chmod +x ~/.docker/cli-plugins/docker-compose
```

**Scenario 1, verified for real** (not simulated): ran the actual
`docker compose up` against a genuinely clean checkout state (fresh certs,
fresh `passwd`, fresh secret, fresh `server.toml`) — all three services
came up with no manual intervention beyond the four steps above, migrations
applied automatically (`_nest_migrations`, `hosts`, `metric_history`,
`rules`, `problems`, `resolved_incidents`, `agent_configs` all present on
first boot), and `GET /api/hosts` responded `200 []` immediately.

For the agent side (Issue 13.1's `sparrow-agent.service`), point
`/etc/sparrow/agent.toml` at the same broker (`mqtt_tls_ca_file` set to
step 1's `ca.crt`, `mqtt_password` matching step 2's per-agent password —
**the agent had no TLS support at all until this issue**; see "Agent TLS"
below), then:

```bash
sudo systemctl enable --now sparrow-agent
sudo systemctl restart sparrow-agent   # scenario 2's actual check
```

**Scenario 2, verified for real**: no root available in the sandbox that
wrote this, so tested via `systemctl --user` instead of a system-wide
install — genuine systemd process supervision either way, just user- vs
system-scoped (the issue's own text anticipates a VM/container standing in
for "a real host," this is the same idea). Started the real compiled
binary under the real unit file's `ExecStart`/`Restart`/`RestartSec`
against the Compose stack above: it registered (`GET /api/hosts` showed
the host, `online: true`), `systemctl --user restart` gave it a new PID,
and afterward `GET /api/hosts` still showed **exactly one** row for that
`host_id` (no duplicate) with a **newer** `last_seen_ms` than before the
restart, and `GET /api/hosts/<id>/items` showed fresh metric timestamps
newer than the restart — both host status and metric publishing resumed
cleanly, nothing duplicated.

### Agent TLS (found while verifying this issue)

`AgentConfig` (`crates/agent/src/config.rs`) had `mqtt_password` (Issue
12.2) but no `mqtt_tls_ca_file` — `ServerConfig` got the equivalent field
in Issue 13.2, the agent never did. Since `deploy/mosquitto/mosquitto.conf`
is a TLS-only listener with no plaintext fallback, **the agent could not
connect to the deployed broker at all** before this fix. Added
`mqtt_tls_ca_file: Option<String>` to `AgentConfig` and wired it into
`build_mqtt_config` (`crates/agent/src/main.rs`) the same way
`crates/server/src/main.rs` already does.

## Mosquitto TLS + auth/ACLs (Issues 12.1, 12.2)

[`mosquitto/`](mosquitto/) — self-signed CA + server certificate generation
(`generate-certs.sh`), per-user credential provisioning
(`provision-user.sh`, wrapping `mosquitto_passwd` — a manual step by
design, not a self-service API), a broker ACL file (`acl.conf`) scoping
each agent to its own topics, and a `mosquitto.conf` wiring all of the
above (TLS listener on `8883`, `allow_anonymous false`, `password_file`,
`acl_file`). See [`mosquitto/README.md`](mosquitto/README.md) for the
exact commands and how to point `nest_mqtt::MqttConfig` /
`crates/agent`'s `AgentConfig` at it.

## Agent release build + systemd unit (Issue 13.1)

`crates/agent` needs no custom `[profile.release]` — checked the two
existing Nest CLI products (`pacificnm/airtable-sync`, `pacificnm/loon`)
directly on GitHub rather than inventing settings, and neither has one
either. `./build build` (rust profile) already produces a single static
binary via plain `cargo build --release` with no changes needed.

[`sparrow-agent.service`](sparrow-agent.service) — install as
`/etc/systemd/system/sparrow-agent.service`:

```bash
sudo useradd --system --no-create-home sparrow-agent
sudo cp target/release/sparrow-agent /usr/local/bin/sparrow-agent
sudo mkdir -p /etc/sparrow && sudo cp config.toml /etc/sparrow/agent.toml
sudo cp deploy/sparrow-agent.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now sparrow-agent
```

**Elevated permissions**: not needed. `sysinfo`'s Linux disk backend
(`sysinfo-0.39.6/src/unix/linux/disk.rs`, checked directly rather than
assumed) reads `/proc/mounts` (world-readable) and calls `statvfs()` per
mount — a standard POSIX call requiring only execute/traverse permission
on the mount point, not root or any capability, for ordinary local
filesystems (ext4, xfs, btrfs, …). `User=sparrow-agent` (no
`CAP_*`/`AmbientCapabilities`) is sufficient for cpu/memory/disk
collection on a standard host.

One real, source-verified caveat, distinct from a permissions concern:
sysinfo's own code comments warn that calling `statvfs()` on an NFS or
CIFS mount using the `hard` mount option **can hang**, not just fail —
if a target host has such a mount, disk collection could stall
indefinitely on it. Not something a service user/capability change can
fix; if a host has `hard`-mounted network filesystems, either mount them
`soft` (accepting the usual NFS-soft tradeoffs) or exclude them from
what `DiskCollector` enumerates (no such exclusion mechanism exists yet —
out of this issue's scope, worth its own issue if a real deployment hits
this).

Restart-survival (`systemctl restart sparrow-agent` picks back up
publishing without duplicate registration) is Issue 13.3's acceptance
check — see the "Quick start" section above for the verified result.

**Found running the real binary this exact way (Issue 13.2):** the phase-13
spec's own `ExecStart` sketch omitted the `run` subcommand — `nest-cli`
requires one of its registered subcommands explicitly, so
`sparrow-agent --config /etc/sparrow/agent.toml` alone fails immediately
with `error: 'sparrow-agent' requires a subcommand but one was not
provided`. `sparrow-agent.service`'s `ExecStart` already has the fix; if
you copied the old sketch by hand, add `run` to the end.

## Server release build + Dockerfile + docker-compose.yml (Issue 13.2)

`crates/server` had **no runnable binary at all** before this issue — every
phase since Phase 7 added library functions (`ingest.rs`, `alerting.rs`,
every `api/*.rs` route builder) but none of them were ever wired into a
`main.rs`, unlike `crates/agent` (Issue 6.6). `crates/server/src/main.rs`
and `crates/server/src/config.rs` are that wiring, built as part of this
issue since a Dockerfile can't package a binary that doesn't exist.
`ServerConfig`'s `[server]` TOML section mirrors `AgentConfig`'s shape
(`crates/agent/src/config.rs`): Postgres URL (password kept separate, see
below), Mosquitto host/port/credentials/TLS CA, HTTP bind address, and
Ollama base URL + completion/embedding models (Issue 10.1's research spike
already settled on Ollama; `nest_ai_ollama::OllamaProvider` for
completions, `sparrow_core::analyst::embedder::OllamaEmbedder` for
embeddings, since neither `nest_ai` nor `nest-ai-ollama` expose those).

### Docker build context is the nest repo root, not this checkout alone

`pacificnm/sparrow`'s real, committed `Cargo.toml` uses path dependencies
into a sibling `pacificnm/nest` checkout (`nest-ai = { path =
"../../core/crates/nest-ai" }`, etc. — checked directly on GitHub, no
`.cargo/config.toml` patch exists). That means `crates/server/Dockerfile`
cannot build from a context containing only this repo; it needs `core/`
and `modules/` two directories up, exactly per
[`apps/README.md`](../../../apps/README.md)'s already-documented
convention: clone `pacificnm/nest`, then clone `pacificnm/sparrow` into
`nest/apps/sparrow/`. Build (and `docker compose up`) from the **nest repo
root**:

```bash
docker compose -f apps/sparrow/deploy/docker-compose.yml up
```

No existing Nest product has a reference Dockerfile — checked
`pacificnm/loon` and `pacificnm/airtable-sync` directly on GitHub (neither
has one; loon's server ships a systemd unit only), so this is a fresh
multi-stage build (Rust build stage → `debian:bookworm-slim` runtime), not
a copied pattern.

### Before `docker compose up`

1. **Certs** — `deploy/mosquitto/generate-certs.sh mosquitto mosquitto localhost 127.0.0.1`.
   The CN/SAN **must include `mosquitto`** (the Compose service name the
   server connects to via `mqtt_broker_host = "mosquitto"` in
   `server.toml`), not just `generate-certs.sh`'s own default
   `mosquitto.local` — found running this for real: a cert issued only for
   `mosquitto.local` makes the server's own TLS handshake fail
   (`certificate not valid for name "mosquitto"`), which surfaces as an
   endless, silent `mqtt event loop error, reconnecting` in the server's
   logs (see `crates/agent/tests/broker_security_live.rs`'s module doc for
   why `nest_mqtt::MqttClient` never surfaces this more directly).
2. **Credentials** — `deploy/mosquitto/provision-user.sh sparrow-server <password>`
   and one more per agent `host_id`.
3. **Postgres secret** — `mkdir -p deploy/secrets && echo -n '<password>' > deploy/secrets/postgres_password.txt`
   (gitignored).
4. **Server config** — `cp deploy/server.toml.example deploy/server.toml`,
   fill in `mqtt_password` matching step 2's `sparrow-server` password
   (gitignored — this file ends up holding that password in plain text,
   unlike the Postgres password, which `database_password_file` keeps out
   of it entirely).

### Postgres image: `pgvector/pgvector:pg16`, not plain `postgres:16`

The phase-13 spec's own `docker-compose.yml` sketch says `image:
postgres:16`. Issue 10.4's `resolved_incidents` migration needs the
`vector` extension (`CREATE EXTENSION vector`, `embedding vector(768) NOT
NULL`) — plain `postgres:16` doesn't have it installed, and migrations
would fail the moment that one ran. Verified directly: ran the real
migration set against both images while writing this file. `pgvector/pgvector:pg16`
is a postgres:16-compatible image with the extension pre-installed —
`docker-compose.yml` already uses it.

### `server.key` is mode 644, not 600

Bind-mounting preserves the host file's exact owner/mode inside the
container. Mosquitto's own non-root runtime user can't open a `600` file
it doesn't own, and fails to start (`Unable to load server key file...
Permission denied`) even though the file is right there — found running
this for real. `generate-certs.sh` already writes `server.key` at `644`
(the accepted tradeoff for a bind-mount deployment: any local host process
can read it, unlike a real secrets-management setup) while keeping `ca.key`
at `600` (it's never mounted into the broker container, only used by
`generate-certs.sh` itself to sign a new cert). The same applies to
`provision-user.sh`'s `passwd` file, which it now `chmod 644`s for the same
reason.

### Startup ordering — why `docker-compose up` doesn't need a wait-for-postgres script

`depends_on` only guarantees `postgres`/`mosquitto` were *started* before
`server`, not that they're *ready* to accept connections yet — a weak
ordering guarantee. This is safe here specifically because both
`nest-data-postgres`'s `PostgresConnection::connect` (Phase 1) and
`nest_mqtt::MqttClient`'s background event loop retry internally: the
former with real backoff before giving up (`connect_retries`/
`connect_backoff_ms`), the latter forever, treating every connect error as
transient (see `crates/agent/tests/broker_security_live.rs`'s module doc).
`sparrow-server`'s own `main.rs` relies on exactly this — it doesn't
implement its own wait-for-postgres/wait-for-mosquitto logic, because it
doesn't need to.

Verified for real, not assumed, twice: once via a manual Docker network
standing in for Compose's mechanics (Issue 13.2), and again via the actual
`docker compose up` command against a genuinely clean checkout (Issue
13.3's "Quick start" section above) — both times migrations applied
automatically and a message published as an ACL-scoped agent user flowed
through `ingest.rs` into Postgres and out `GET /api/hosts` correctly.
