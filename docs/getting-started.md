# Getting started

The front door for Sparrow: how to build every component, and how to run
the whole system, from nothing. If you already know the architecture and
just need topic names or API routes, see
[`architecture.md`](architecture.md) / [`api-reference.md`](api-reference.md)
instead — this doc is about *doing*, not explaining.

## 1. What you're building

Sparrow is four things in one Cargo workspace, plus a broker and a
database it doesn't ship itself:

| Component | Path | What it is |
|---|---|---|
| `sparrow-core` | `crates/sparrow-core` | Shared library — `Collector` trait, topics, storage, alerting model. Not runnable on its own. |
| `sparrow-agent` | `crates/agent` | Binary. Runs on a monitored host, publishes metrics over MQTT. |
| `sparrow-server` | `crates/server` | Binary. Ingests MQTT, stores to Postgres, serves the HTTP API, runs alerting + the AI analyst. |
| Sparrow Desktop | `desktop/` | Tauri + React admin dashboard. Talks to the server's HTTP API only — not part of the MQTT side. |

Everything talks through **Mosquitto** (MQTT broker) and **Postgres**
(`pgvector/pgvector:pg16` specifically, not plain `postgres:16` — see
§4). Neither is part of this repo; you run them yourself, either as
plain local installs (§3, fastest path to a first working setup) or via
the provided Docker Compose stack (§4, closest to how this would
actually be deployed).

## 2. Prerequisites

- **Rust** (stable; `rustup` is fine) — builds all four Rust
  components.
- **Docker**, with the `compose` plugin (`docker compose version` should
  print something; if it doesn't, see the Troubleshooting entry in §7).
  Needed for Postgres/Mosquitto in the Compose stack (§4) and for
  `cargo test`'s own testcontainers-based tests (§5).
- **Node.js + npm** — only needed for `desktop/`.
- **This checkout must be nested inside a `pacificnm/nest` checkout**,
  at `nest/apps/sparrow/` — Sparrow's `Cargo.toml` uses path
  dependencies straight into `nest`'s `core/` and `modules/` (e.g.
  `nest-ai = { path = "../../core/crates/nest-ai" }`). If you cloned
  Sparrow standalone, none of the Rust components will build. See
  [`apps/README.md`](https://github.com/pacificnm/nest/blob/main/apps/README.md)
  in the `nest` repo for the exact clone layout:

  ```bash
  git clone https://github.com/pacificnm/nest.git
  git clone https://github.com/pacificnm/sparrow.git nest/apps/sparrow
  cd nest/apps/sparrow
  ```

  Everything below assumes you're running commands from inside this
  `apps/sparrow` checkout, unless stated otherwise.

## 3. Building each component

### The whole workspace at once

```bash
./build build    # cargo build --workspace
./build check     # cargo fmt --check + clippy -D warnings + cargo test, across the workspace
./build test      # cargo test --workspace
./build clean     # rm -rf target/
```

This is a `workspace`-profile `./build` (see
[`docs/build.md`](https://github.com/pacificnm/nest/blob/main/docs/build.md)
in the `nest` repo for what that means) — the same pattern the `nest`
framework repo uses for its own root, because like `nest` itself,
Sparrow has more than one binary and no single "the" thing to run.
`./build run` / `./build dev` are deliberately **not defined** at this
level for that reason — run a specific binary directly (below), or use
the deploy mechanisms in §4/§6.

### Just the agent, or just the server

```bash
cargo build --release -p sparrow-agent
cargo build --release -p sparrow-server
```

Produces `target/release/sparrow-agent` / `target/release/sparrow-server`
— plain, static binaries, no special `[profile.release]` needed.

### The desktop dashboard

```bash
cd desktop
./build build     # tsc -b && vite build, then a release Tauri binary
./build dev        # hot-reload dev mode (vite + tauri dev)
./build check      # fmt/clippy/test + a UI production build
```

`desktop/build` is new — nothing under `desktop/` had a `./build`
wrapper before this, even though `apps/README.md`'s own standard says
every desktop app gets one (`templates/desktop/build`). It's a
`tauri`-profile script, same shape as that template.

**Known gap, not fixed here:** `desktop/build check` (and
`build`/`npm run build --prefix ui`) currently fails at the `tsc -b`
step:

```
../../../../core/crates/nest-react-components/src/components/feedback/Popover.tsx:
error TS2322: ... Two different types with this name exist, but they are unrelated.
```

Verified cause: `@nest/components` (`desktop/ui`'s dependency on
`core/crates/nest-react-components`) is consumed via a raw relative
`path` in `package.json`, not an npm workspace — so npm installs two
independent copies of `@types/react` (one under `desktop/ui/node_modules`,
one under `nest-react-components`'s own `node_modules`), and TypeScript
treats same-named React types from the two copies as unrelated. Both
declare the same `^19.1.0` range, so it's a duplicate-install problem, not
a real incompatibility. This is a `nest`-repo-wide packaging gap (npm
workspaces don't span `core/` into product `apps/`), not something
introduced by or specific to Sparrow — fixing it means restructuring how
`nest`'s own JS packages are consumed, out of scope for this repo alone.
**`./build dev` / `npm run dev` are unaffected** (plain `vite`, no
`tsc -b` step) — this only blocks the production build path.
`cargo check -p sparrow-desktop` (the Rust side) compiles cleanly on its
own.

### Rust-side only, no npm

```bash
cargo check -p sparrow-desktop
```

Confirms the Tauri/Rust half of the desktop app independent of the
frontend build gap above.

## 4. Running the whole system (Docker Compose)

The fastest way to see all the pieces actually talk to each other. Full
detail (including every gotcha found running each step for real) is in
[`deploy/README.md`](../deploy/README.md); this is the condensed path.

Run every command below from the **nest repo root** (`docker compose`'s
build context needs `core/`/`modules/`, not just this checkout):

```bash
# 1. Certs for Mosquitto's TLS listener — CN/SAN must include "mosquitto"
#    (the Compose service name), not generate-certs.sh's own default.
apps/sparrow/deploy/mosquitto/generate-certs.sh mosquitto mosquitto localhost 127.0.0.1

# 2. Broker credentials — one per principal.
apps/sparrow/deploy/mosquitto/provision-user.sh sparrow-server <server-password>
apps/sparrow/deploy/mosquitto/provision-user.sh <agent-host-id> <agent-password>

# 3. Postgres secret (gitignored, never commit this).
mkdir -p apps/sparrow/deploy/secrets
echo -n '<postgres-password>' > apps/sparrow/deploy/secrets/postgres_password.txt

# 4. Server config (gitignored) — copy the example, then fill in
#    mqtt_password to match step 2's sparrow-server password.
cp apps/sparrow/deploy/server.toml.example apps/sparrow/deploy/server.toml

# 5. Bring it up.
docker compose -f apps/sparrow/deploy/docker-compose.yml up
```

That starts Mosquitto (TLS + ACLs), Postgres (`pgvector/pgvector:pg16` —
needed for `resolved_incidents`' `CREATE EXTENSION vector`; plain
`postgres:16` fails that migration), and `sparrow-server` — migrations
apply automatically, no manual DB setup step. Confirm it's up:

```bash
curl http://localhost:8080/api/hosts   # -> 200 []
```

No `sparrow-agent` runs in this stack by default — it's meant to run on
whatever host you're actually monitoring, not bundled into the
Compose file. Point one at this broker (§6) to see hosts show up.

**`docker compose` command not found?** See §7.

## 5. Running the test suite

```bash
cd apps/sparrow
cargo test --workspace
```

No manual setup needed first — every Postgres/Mosquitto-backed test
(including the end-to-end test in `crates/server/tests/e2e.rs`) uses
`testcontainers-rs` to spin up and tear down its own containers per run.
Docker just needs to be running and reachable; nothing else. If your
shell reports "permission denied" talking to the Docker socket, try
prefixing the command with `sg docker -c "..."` before assuming Docker
itself is unavailable — that's a shell group-membership issue, not a
missing feature, in at least one environment this was written from.

## 6. Running the agent against a real broker

Once you have a broker running (either the Compose stack in §4, or your
own Mosquitto), point an agent at it:

```toml
# agent.toml
[agent]
host_id = "web-01"
broker_host = "mosquitto.internal"   # or "mosquitto" if this agent is
                                       # itself inside the Compose network
broker_port = 8883
mqtt_password = "<the per-agent password from step 2 above>"
mqtt_tls_ca_file = "/path/to/deploy/mosquitto/certs/ca.crt"

# optional overrides — omit entirely to run every collector at its
# own default interval
disabled_collectors = ["disk"]
[agent.collector_intervals]
cpu = 10
```

```bash
cargo run --release -p sparrow-agent -- --config agent.toml run
```

`--config` is a **global** flag — it must come *before* `run`, not
after. `sparrow-agent --config agent.toml` with no subcommand, or
`sparrow-agent run --config agent.toml`, both fail immediately with
`requires a subcommand but one was not provided` /
`unexpected argument '--config'` respectively.

For a real, standalone host (not just a dev shell), install it as a
systemd service instead — see
[`deploy/sparrow-agent.service`](../deploy/sparrow-agent.service) and
the "Agent release build + systemd unit" section of
[`deploy/README.md`](../deploy/README.md) for the exact
`useradd`/`cp`/`systemctl enable --now` sequence. No elevated
permissions are needed for the collectors themselves (cpu/memory/disk
all read world-readable `/proc` and standard `statvfs()` calls).

Once running, confirm it registered:

```bash
curl http://localhost:8080/api/hosts
# -> [{"host_id": "web-01", "hostname": "...", "online": true, ...}]
```

## 7. Troubleshooting

Real failures found running each of these commands for real, not
theoretical:

| Symptom | Cause | Fix |
|---|---|---|
| Agent can't connect to the broker at all | No TLS CA configured (`deploy/mosquitto/mosquitto.conf` is TLS-only, no plaintext listener) | Set `mqtt_tls_ca_file` in `agent.toml`. |
| Server logs an endless `mqtt event loop error, reconnecting`, never surfaces why | TLS cert's CN/SAN doesn't include the hostname the client actually dials (e.g. cert issued for `mosquitto.local`, client connects to Compose service name `mosquitto`) | Regenerate the cert with the right SAN: `generate-certs.sh mosquitto mosquitto localhost 127.0.0.1`. |
| Mosquitto container won't start: `Unable to load server key file... Permission denied` | Bind-mounted `server.key`/`passwd` are `600`, unreadable by the broker's non-root user once mounted | `generate-certs.sh`/`provision-user.sh` already write these at `644` — re-run them if you hand-rolled the files differently. |
| Postgres migrations fail with `extension "vector" does not exist` | Using plain `postgres:16` instead of `pgvector/pgvector:pg16` | Use the pgvector image; `docker-compose.yml` already does. |
| `sparrow-agent`/`sparrow-server` exits immediately with `requires a subcommand` or `unexpected argument '--config'` | `--config` placed after the subcommand instead of before | `sparrow-agent --config agent.toml run`, not `sparrow-agent run --config agent.toml`. |
| `docker compose: command not found` | The Compose v2 plugin isn't installed | `mkdir -p ~/.docker/cli-plugins && curl -sL "https://github.com/docker/compose/releases/latest/download/docker-compose-linux-x86_64" -o ~/.docker/cli-plugins/docker-compose && chmod +x ~/.docker/cli-plugins/docker-compose` — no root needed. |
| `docker ...` says permission denied | Shell's group membership hasn't picked up `docker` group yet in this session | Try `sg docker -c "<command>"` before concluding Docker itself is unavailable. |
| `desktop/build check` / `npm run build --prefix desktop/ui` fails with a `tsc -b` type error in `Popover.tsx`/`Select.tsx` | Duplicate `@types/react` copies — see §3's "Known gap" note | Use `./build dev` / `npm run dev` for actual development; the production build path is blocked on a `nest`-repo-wide npm packaging gap. |

## 8. Where to go next

- Adding a new collector (metric type): [`authoring-collectors.md`](authoring-collectors.md).
- Understanding *why* Sparrow's Agent↔Server relationship works the way
  it does (config push, offline detection): [`architecture.md`](architecture.md).
- Every HTTP route the server exposes: [`api-reference.md`](api-reference.md).
- Deployment detail beyond the condensed version in §4/§6 (exact
  Mosquitto ACL rules, why `depends_on` doesn't need a wait-for-postgres
  script, the full list of things found wrong in the original phase
  specs and fixed): [`../deploy/README.md`](../deploy/README.md) and
  [`../deploy/mosquitto/README.md`](../deploy/mosquitto/README.md).
