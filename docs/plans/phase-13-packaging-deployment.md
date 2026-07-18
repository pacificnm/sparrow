# Phase 13 Task Spec — Packaging & Deployment

**Repo:** `pacificnm/sparrow`
**Prerequisite:** Phases 4–9 at minimum (a working agent + server); Phase 12 ideally done first if this deployment is meant to face anything other than a fully trusted local network.

## Scope

Ops/packaging work — least architecturally novel phase, most "follow
existing convention" of the whole plan. No new application logic.

### Agent release build

`./build` (rust profile) produces a single static binary per the app
standard's CLI layout — confirm `crates/agent`'s `Cargo.toml` release
profile settings (`opt-level`, `lto`, `strip`) match whatever convention
other Nest CLI products use (check an existing product's release profile
rather than inventing settings independently).

`systemd` unit (`deploy/sparrow-agent.service`):

```ini
[Unit]
Description=Sparrow monitoring agent
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/sparrow-agent --config /etc/sparrow/agent.toml
Restart=on-failure
RestartSec=5
User=sparrow-agent
# RESOLVED (Issue 13.1): no elevated permissions needed. sysinfo's Linux
# disk backend (checked its source directly, sysinfo-0.39.6's
# src/unix/linux/disk.rs) reads /proc/mounts and calls statvfs() per
# mount - a standard POSIX call needing only execute/traverse permission
# on the mount point, not root or a capability, for ordinary local
# filesystems. The one real caveat found is unrelated to permissions:
# sysinfo's own source comments warn statvfs() on an NFS/CIFS mount using
# the `hard` option can hang, not just fail - see deploy/README.md's
# "Agent release build + systemd unit" section for what that implies for
# hosts with such mounts.

[Install]
WantedBy=multi-user.target
```

### Server release build + Docker/compose

`deploy/docker-compose.yml`:

```yaml
services:
  mosquitto:
    image: eclipse-mosquitto:2
    volumes:
      - ./mosquitto.conf:/mosquitto/config/mosquitto.conf
      - ./acl.conf:/mosquitto/config/acl.conf   # from Phase 12
    ports:
      - "8883:8883"

  postgres:
    image: postgres:16
    environment:
      POSTGRES_DB: sparrow
      POSTGRES_USER: sparrow
      POSTGRES_PASSWORD_FILE: /run/secrets/postgres_password
    secrets:
      - postgres_password
    volumes:
      - sparrow-postgres-data:/var/lib/postgresql/data

  server:
    build: ./crates/server
    depends_on:
      - mosquitto
      - postgres
    environment:
      DATABASE_URL: postgresql://sparrow@postgres/sparrow # password via secret, not inline
    ports:
      - "8080:8080"
    secrets:
      - postgres_password

secrets:
  postgres_password:
    file: ./secrets/postgres_password.txt

volumes:
  sparrow-postgres-data:
```

**Explicit note:** the `server` service's `Dockerfile` (under
`crates/server/`) needs a multi-stage build (Rust build stage → slim runtime
image) — check whether other Nest products' server surfaces (per the app
standard's "Server" host-matrix row) already have a reference Dockerfile to
copy the pattern from, rather than inventing a Rust-Docker build pipeline
from scratch for this phase alone.

### Documented startup ordering

Phase 1's retry/backoff work on `nest-data-postgres`'s `PostgresConnection::connect`
(and this phase's equivalent expectation for `nest-mqtt`'s connect, if TLS/auth
from Phase 12 add connection latency) is exactly what makes `depends_on`'s
weak ordering guarantee (container *started*, not container *ready*) safe to
rely on here — cross-reference that in this phase's own docs so a future
reader understands why `docker-compose up` doesn't need a manual
wait-for-postgres script.

## Tests / acceptance

- `docker-compose up` from a clean checkout brings up broker + Postgres +
  server with no manual intervention, migrations apply automatically on
  first boot (per Phase 4's `PostgresDataModule::with_migrations`).
- A `systemd`-managed agent (tested in a VM or container standing in for a
  "real host," not on the CI runner directly) starts, registers, and
  survives a `systemctl restart` cleanly (picks back up publishing after
  restart, no duplicate registration issues).

**Acceptance:** the above two scenarios both succeed from a documented,
repeatable set of steps in `deploy/README.md` — the actual deliverable of
this phase is as much the documentation as the config files themselves,
since ops steps that only work "if you remember the undocumented step" are
exactly the failure mode this phase exists to prevent.

## Explicit "do not" list

- Do not inline the Postgres password in `docker-compose.yml` — use Docker secrets or an env file excluded from version control, consistent with the security posture Phase 12 established.
- Do not invent release-profile build settings independently — check existing Nest product conventions first.
- Do not skip the "restart cleanly" acceptance check for the agent — a monitoring agent that can't survive its own service restart cleanly defeats the purpose.
