# Project Sparrow

A modular infrastructure monitoring system — think Zabbix/Nagios, rebuilt on
[`pacificnm/nest`](https://github.com/pacificnm/nest), our own Rust
application framework. Sparrow follows a pub/sub, IoT-style architecture:
lightweight **Agents** run on monitored hosts, publish metrics over MQTT, and
a central **Server** ingests, stores, evaluates, and (eventually) explains
what's going on.

> **Status:** All 14 phases implemented, tested, and documented. Sparrow is
> a working system — see [`docs/getting-started.md`](docs/getting-started.md)
> to build and run it.

---

## Why this exists

Existing tools (Zabbix, Nagios) are poll-based, heavyweight, and not built
around our own platform conventions. Sparrow reuses `nest`'s existing
building blocks — HTTP client/server, Postgres data layer, AI provider
abstraction, task runtime, CLI bootstrap — and adds the pieces `nest`
doesn't have yet (MQTT, a Claude-backed AI provider) as proper framework
modules, not one-off hacks bolted onto the product repo.

## Architecture

```
┌─────────────┐        MQTT (pub/sub)        ┌─────────────┐
│   Agent     │ ───────────────────────────▶ │   Server    │
│ (nest-cli)  │ ◀─────────────────────────── │(nest-http-  │
│             │      retained config push      │   serve)   │
└─────────────┘                                └─────────────┘
      │                                              │
 Collectors                                    Postgres (+ pgvector)
 (cpu/memory/disk)                             AI Health Analyst
```

- **Collectors** — modular metric producers (`cpu`, `memory`, `disk` to
  start). Pure, testable, no knowledge of transport or scheduling.
- **Agent** — a `nest-cli` binary. Runs collectors on their own intervals,
  publishes batched metrics + heartbeat over MQTT, applies live config
  pushed from the server via retained messages.
- **Server** — a `nest-http-serve` host. Ingests agent data, persists to
  Postgres, evaluates alerting rules, exposes a REST API, and runs the AI
  Health Analyst.

### Deliberate deviations from Zabbix's model

- **Retained MQTT config push** instead of active-check polling — an agent
  gets its latest config immediately on connect, no round trip needed.
- **MQTT Last-Will-and-Testament (LWT)** instead of availability polling —
  near-instant offline detection on unclean disconnect, backed by a periodic
  polling sweep as a backstop for the "hung but still connected" case.

## Repo layout

```
sparrow/
├── crates/
│   ├── sparrow-core/  # shared domain logic: Collector trait, topic taxonomy,
│   │                    storage, trigger/alerting model, AI analyst tools
│   ├── agent/         # nest-cli binary (sparrow-agent)
│   └── server/        # nest-http-serve binary (sparrow-server)
├── desktop/           # nest-tauri dashboard
└── deploy/            # systemd units, docker-compose, broker config
```

## Built on `nest`

Sparrow depends on existing, real `nest` modules:
`nest-core`, `nest-cli`, `nest-http-client`, `nest-http-serve`,
`nest-data-postgres`, `nest-task-runtime`, `nest-ai`, `nest-ai-ollama`,
`nest-claude`.

Two pieces of framework work are prerequisites for Sparrow itself and are
being built/hardened in the `nest` repo first:

| Module | What it is |
|---|---|
| `nest-mqtt` | New. `rumqttc`-backed MQTT client + module, mirroring `nest-http-client`'s shape. |
| `nest-ai-claude` | New. A thin `AiProvider` adapter wrapping `nest-claude`, so Ollama and Claude are swappable via config for the AI Health Analyst. |
| `nest-data-postgres` | Existing, hardened. Adds connect retry/backoff; test suite retrofitted to `testcontainers-rs`. |

## Phase plan

| # | Phase | Repo |
|---|---|---|
| 0 | Foundations & decisions (no code) | — |
| 1 | Harden + retrofit `nest-data-postgres` | `nest` |
| 2 | Build `nest-mqtt` | `nest` |
| 3 | Build `nest-ai-claude` | `nest` |
| 4 | Sparrow core contracts (`Collector`, topics, storage) | `sparrow` |
| 5 | Collectors (`cpu`, `memory`, `disk`) | `sparrow` |
| 6 | Agent | `sparrow` |
| 7 | Server | `sparrow` |
| 8 | Trigger / alerting engine | `sparrow` |
| 9 | Server → agent config push | `sparrow` |
| 10 | AI Health Analyst | `sparrow` |
| 11 | Desktop dashboard (`nest-tauri`) | `sparrow` |
| 12 | Security hardening (TLS, broker ACLs) | `sparrow` + `nest` |
| 13 | Packaging & deployment | `sparrow` |
| 14 | Testing, docs, polish | `sparrow` |

Phases 1–3 (framework prerequisites) are unblocked and come first. Phase 11
(desktop dashboard) is deliberately deferred until the core data path and
alerting are proven out.

Detailed, task-by-task specs for every phase live in this project's
knowledge area, written to be executable by a local low-cost model (Qwen via
Ollama) without further clarification.

## Key design decisions (locked, v5)

1. **`nest-data-postgres` tests get retrofitted to `testcontainers-rs`**,
   not just left alone — establishes one consistent testing convention
   (automatic container spin-up/teardown, no `DATABASE_URL` env var, no
   `--ignored` flag) across every new and touched module.
2. **Desktop dashboard is a later phase** — after the core data path and
   alerting are proven.
3. **`nest-ai-claude` is a proper adapter, not a hardcoded choice** — the AI
   Health Analyst is swappable between local Ollama and Claude via
   `nest-ai`'s `AiProvider` trait from day one.

## Testing convention

`testcontainers-rs` for all new and touched test infrastructure — Postgres,
Mosquitto, and pgvector-enabled Postgres containers spin up and tear down
automatically per test run. No manual environment setup, no `--ignored`
flags.

## AI Health Analyst

In scope for Phase 10. A tool-calling loop (built in Sparrow, not the
framework) that lets the model query Sparrow's own data —
`get_host_status`, `get_metric_history`, `get_active_problems`, and
`search_similar_incidents` (pgvector similarity search over past resolved
incidents) — to help explain and resolve Problems. Runs against whichever
`AiProvider` is configured (Ollama or Claude), no analyst code depends on
either provider directly.

## Working principles

- **Accuracy over assumption.** Specs are written against verified source
  (real function signatures, real file paths), not plausible-sounding
  guesses. Where something wasn't verified, the spec says so explicitly
  (`todo!()` / "check before writing this" markers) rather than fabricating
  detail.
- **Thoroughness before speed.** All 14 phase specs and the 23-issue
  framework-prerequisite issue list were written before any implementation
  began.
- **"Nest" always means `pacificnm/nest`** — our own framework, not Node's
  NestJS. Worth over-clarifying in docs and code comments.

## Status

All 14 phases are implemented and merged: collectors, agent, server,
alerting, config push, the AI Health Analyst, the desktop dashboard,
security hardening (TLS + broker ACLs), packaging/deployment, and this
documentation set. See [`docs/getting-started.md`](docs/getting-started.md)
to build and run every component, [`docs/architecture.md`](docs/architecture.md)
for how the pieces fit together, and [`docs/api-reference.md`](docs/api-reference.md)
for the HTTP API.
