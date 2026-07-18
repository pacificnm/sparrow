# Sparrow deployment

Deployment artifacts for running Sparrow outside a dev checkout. Grows across
Phase 12 (security hardening) and Phase 13 (packaging/deployment) — only
what those phases have actually delivered so far is listed below.

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
check, not this one's — this section only covers the build + unit file.

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

Verified for real, not assumed: ran the actual `sparrow-server` binary
(built via this same multi-stage Dockerfile) against Mosquitto (TLS +
ACLs, real generated certs and `mosquitto_passwd`-provisioned credentials)
and a real `pgvector/pgvector:pg16` container on a Docker network matching
Compose's own service-name-based DNS, migrations applied automatically,
and a message published as an ACL-scoped agent user flowed through
`ingest.rs` into Postgres and out `GET /api/hosts` correctly.
