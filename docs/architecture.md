# Sparrow architecture

This is Sparrow's own architecture doc — distinct from the Nest framework's
(`docs/architecture.md` in `pacificnm/nest`). Written for a reader who
already knows Zabbix and is wondering why this doesn't look like it:
Sparrow borrows Zabbix's vocabulary (agent, server, trigger, problem) but
makes two deliberate architectural departures from Zabbix's model,
explained below — everything else here is groundwork for understanding
those two departures, not a generic tour.

## The three components

```text
  Monitored host
  +--------------------------------------------------------+
  |  CpuCollector   MemoryCollector   DiskCollector          |
  |  (sparrow-core's Collector trait, one process per host,  |
  |   each scheduled on its own interval)                    |
  |         \             |              /                  |
  |          '----------- Agent ---------'                  |
  |            (crates/agent, nest-cli host)                 |
  |     scheduler -> publisher -> Mosquitto                  |
  |     config_reload <- Mosquitto (retained config)          |
  +------------------|------------------|--------------------+
                      |                  |
       register/heartbeat/data    config subscribe
       (+ LWT fires on unclean    (retained: delivered
        disconnect)                immediately even to a
                      |             reconnecting agent)
                      v                  ^
              +----------------------------+
              |      Mosquitto broker       |
              +--------------|--------------+
                              | subscribe (ingest.rs)
                              v
              +----------------------------+
              |           Server            |
              |  (crates/server,            |
              |   nest-http-serve host)     |
              |                             |
              |  ingest ---------> Postgres |
              |  alerting (rules->problems) |
              |  offline_watch (LWT backstop)|
              |  AI Health Analyst           |
              |   (Ollama/Claude, nest-ai)  |
              |                             |
              |  PUT /api/agent_config ---->| (publishes the
              +--------------|--------------+  retained config
                              | HTTP API         message above)
                              v
              +----------------------------+
              |      Desktop dashboard      |
              |  (desktop/, nest-tauri host)|
              +----------------------------+
```

**Collector** — `sparrow_core::collector::Collector`, implemented by
`CpuCollector`/`MemoryCollector`/`DiskCollector` (see
[`docs/authoring-collectors.md`](authoring-collectors.md) for the trait
itself). Not a separate deployable or Nest host — a plain trait object the
Agent owns and calls on a schedule. Zabbix would call this the "item"
layer; Sparrow keeps it as a first-class Rust trait rather than a
server-side configuration concept, because *what* gets collected is a
compile-time decision here (see "adding a collector" in the authoring
guide), not something the server pushes down item-by-item.

**Agent** — `crates/agent`, a `nest-cli` host (Phase 6). Runs on every
monitored host: schedules each `Collector` on its own interval
(`AgentConfig.collector_intervals`, falling back to
`Collector::default_interval_secs()`), batches and publishes readings,
sends heartbeats, and subscribes to its own retained config topic to apply
server-pushed overrides live (`config_reload.rs`) without a restart.

**Server** — `crates/server`, a `nest-http-serve` host (Phase 7 onward).
The one long-running central process: ingests every agent's MQTT traffic
into Postgres, evaluates alerting rules against incoming data
(`alerting.rs`), runs the LWT-backstop offline sweep (`offline_watch.rs`),
serves the HTTP API every other surface talks to, and hosts the AI Health
Analyst (Phase 10, via `nest-ai`'s provider-agnostic `AiService` — Ollama
or Claude depending on configuration, not hardcoded to one).

There's a fourth surface built on top of these three, worth naming even
though the issue that asked for this document scoped it to three: the
**desktop dashboard** (`desktop/`, a `nest-tauri` host, Phase 11) — a
read/admin UI that talks to the Server's HTTP API. It doesn't participate
in the MQTT side of the architecture at all; it's a client of the Server
exactly the way an external `curl` call would be, just with a UI.

This mapping is deliberately different from the project plan's own inline
note (`docs/plans/sparrow-project-plan.md`, "1. Repo & workspace":
"`crates/core` ... `crates/agent` (`nest-cli`) ... `crates/server`
(`nest-http-serve`) ... `desktop/` (`nest-tauri`)") — that's four things,
not three, and doesn't name Collector as a component at all (it's
subsumed into `crates/core`). Both framings are consistent with the real
code; this document follows the phase-14 spec's own three-component
framing because it maps directly onto the two architectural departures
below, which are fundamentally about the Agent↔Server relationship, not
about the Desktop client.

## Topic taxonomy (Phase 4)

Every topic is scoped under `sparrow/agents/{host_id}/`, defined once in
`sparrow_core::transport::Topics` — nothing outside that module hand-formats
a topic string:

| Topic | Published by | Subscribed by | Retained |
|---|---|---|---|
| `sparrow/agents/{host_id}/register` | Agent, once at startup | Server (`ingest.rs`, wildcard `+`) | Yes |
| `sparrow/agents/{host_id}/heartbeat` | Agent, periodically | Server (wildcard `+`) | Yes (including the empty LWT payload) |
| `sparrow/agents/{host_id}/data` | Agent, per collector interval | Server (wildcard `+`) | No |
| `sparrow/agents/{host_id}/config` | Server, on `PUT /api/agent_config` | Agent (its own `host_id` only) | Yes |
| `sparrow/agents/{host_id}/command` | Server (reserved — no publisher wired yet) | Agent (its own `host_id` only) | — |

`register`/`heartbeat` are retained so a server that (re)starts after an
agent has already announced itself still knows that agent exists without
waiting for its next publish. `data` is deliberately *not* retained — a
metric reading has no meaning to a server that starts up hours later, so
retaining it would just be dead weight in the broker.

## Two deliberate departures from Zabbix

Zabbix's classic model: agents mostly sit passive (Zabbix server actively
polls them, "passive checks"), or on "active checks" an agent
periodically *asks* the server for its own item/interval configuration and
separately reports an availability heartbeat the server polls for.
Sparrow inverts both halves of that.

### Config push (retained message), not active-check polling

In Zabbix, an active-check agent asks the server "what should I be
collecting, and how often?" on a timer. Sparrow's Agent never asks — the
Server *pushes* configuration by publishing to the agent's own
`sparrow/agents/{host_id}/config` topic (`api/agent_config.rs`'s
`PUT /api/agent_config` handler), and crucially publishes it **retained**.
That single word is the whole mechanism: MQTT retains the last message on
a topic and delivers it immediately to any client that subscribes
afterward, even if that client wasn't connected when the message was
originally published. So an agent that's offline when an operator changes
its config isn't stuck running stale settings until its next poll — the
moment it (re)connects and subscribes to its own `config` topic
(`config_reload.rs`'s `Task::run`), the latest configuration is right
there waiting, no poll needed. This is *why* `Topics::config` exists as a
distinct topic from `data`/`heartbeat` at all — it's not "yet another
thing to subscribe to," it's the one topic where retention is the entire
point.

The corresponding real bug this design already caught (Issue 9.4): the
first version of `main.rs` spawned collectors from local config *and*
separately started `ConfigReload`, whose own bookkeeping of "what's
running" started empty — so the very first retained message a freshly
started agent received could never cancel anything, and would spawn
duplicates instead. The fix was giving `ConfigReload` sole ownership of
the collector lifecycle, waiting briefly for a retained message before
falling back to local defaults, rather than "spawn locally, then correct
later." Worth knowing if you're tempted to have some other component also
spawn collectors independently — the retained-message model only works if
exactly one thing owns "what's currently running."

### Last-Will-and-Testament, not availability polling

Zabbix's server polls each agent on an interval to check it's alive; a
missed poll (or several, past a threshold) is how it notices an agent went
away. Sparrow's Agent instead configures an MQTT **Last Will and
Testament** at connect time: an empty payload on its own `heartbeat`
topic, retained, that the *broker itself* publishes automatically the
moment that agent's TCP connection dies — cleanly or not. `ingest.rs`'s
heartbeat handler treats an empty payload as this exact signal (checked
via the topic's `host_id`, since there's no message body to deserialize)
and marks the host offline immediately. No polling loop, no missed-check
threshold to tune — the broker notices the disconnect the instant it
happens and says so on Sparrow's behalf.

This is *not* the only offline-detection mechanism, deliberately:
`offline_watch.rs`'s periodic sweep (Phase 7) exists specifically for the
one case an LWT cannot cover — an agent process that *hangs* without its
TCP connection ever actually dying (a frozen process still holding an
open, idle socket). No disconnect ever fires there, so the broker has no
Will to publish. The sweep polls `last_seen` on a fixed cadence and
compares it to a staleness threshold, catching exactly that gap. Every
other disconnect scenario — clean shutdown, network partition, killed
process — is already handled near-instantly by the LWT before the sweep
would ever matter. If you're wondering why both mechanisms exist rather
than picking one: they cover genuinely disjoint failure modes, not
redundant paths to the same signal.

## Related

- [`docs/authoring-collectors.md`](authoring-collectors.md) — the
  Collector trait, in depth.
- [`docs/plans/sparrow-project-plan.md`](plans/sparrow-project-plan.md) —
  the original phase-by-phase plan this whole project followed.
- `crates/sparrow-core/src/transport.rs` — the topic taxonomy's actual
  source of truth; this document summarizes it, that file defines it.
