# Server API reference

`crates/server`'s HTTP API — every route Phases 7–11 added, checked
directly against the real route handlers in `crates/server/src/api/`
before being written here (not auto-generated: checked the Nest ecosystem
first, per this issue's own instruction — `pacificnm/loon`'s
`server/docs/02-api-reference.md` is hand-written markdown in this same
shape, not generated from doc comments or an OpenAPI tool, so this follows
that same established convention rather than introducing a new one).

All routes are mounted under `/api` (`RouteGroup::new("/api")` in every
`api/*.rs` module). Errors follow `nest-http-serve`'s standard error body
shape (`ServeError`) — not repeated per endpoint below since it's uniform
across the whole API, not endpoint-specific behavior.

## Handler modules

```text
crates/server/src/api/
├── hosts.rs         # host listing + latest metric values
├── history.rs       # historical metric values for one key
├── problems.rs       # open Problems (Phase 8)
├── agent_config.rs   # per-agent collector overrides (Phase 9)
└── analyst.rs         # AI Health Analyst (Phase 10)
```

---

## Hosts (`api/hosts.rs`)

### `GET /api/hosts`

**Handler:** `list_hosts`

**Purpose:** Lists every registered host and its current online/offline
status. Backed by `HostRegistry::list` (`sparrow_core::storage`) — the
same `hosts` table `ingest.rs`'s register/heartbeat loops and
`offline_watch.rs`'s sweep write to.

**Response:** `Vec<HostRow>`

```json
[
  {
    "host_id": "web-01",
    "hostname": "web-01.internal",
    "online": true,
    "last_seen_ms": 1700000000000
  }
]
```

### `GET /api/hosts/:id/items`

**Handler:** `get_latest_items`

**Purpose:** The most recent value for every metric `key` a host has ever
reported — one row per distinct key, not a full history (see
`/api/hosts/:id/items/:key/history` below for that). Backed by
`MetricHistory::latest_items`.

**Response:** `Vec<MetricHistoryRow>`

```json
[
  {
    "collector": "cpu",
    "key": "cpu.usage_percent",
    "value": "14.93",
    "value_type": "float",
    "tags": {},
    "ts": 1700000000000
  }
]
```

---

## History (`api/history.rs`)

### `GET /api/hosts/:id/items/:key/history`

**Handler:** `get_history`

**Purpose:** Historical readings for one specific `(host_id, key)` pair,
newest first. Backed by `MetricHistory::history`.

**Query parameters:**

| Parameter | Type | Default | Notes |
|---|---|---|---|
| `from_ms` | integer | none | Inclusive lower bound (Unix millis). |
| `to_ms` | integer | none | Inclusive upper bound (Unix millis). |
| `limit` | integer | `1000` | Clamped into `1..=10000` — a request for more than the hard max silently gets the max, not an error. Only a non-numeric value is rejected. |

**Response:** `Vec<MetricHistoryRow>` (same shape as `/api/hosts/:id/items` above).

---

## Problems (`api/problems.rs`)

### `GET /api/problems`

**Handler:** `list_problems`

**Purpose:** Every currently **open** Problem (Phase 8's `alerting.rs`
opens/resolves these against seeded `rules`), joined with its owning
rule's `severity` — `sparrow_core::trigger::Problem` itself has no
`severity` field (that lives on `Rule`), so this endpoint's response type
(`OpenProblem`) is a superset built specifically for this route, not the
same type `alerting.rs`'s own internals use.

**Query parameters:**

| Parameter | Type | Default | Notes |
|---|---|---|---|
| `host_id` | string | none (all hosts) | Filters to one host's open Problems. |

**Response:** `Vec<OpenProblem>`

```json
[
  {
    "id": 1,
    "rule_id": 3,
    "host_id": "web-01",
    "status": "open",
    "opened_at": 1700000000000,
    "resolved_at": null,
    "last_value": 95.0,
    "severity": "critical"
  }
]
```

---

## Agent config (`api/agent_config.rs`)

### `GET /api/hosts/:id/config`

**Handler:** `get_agent_config`

**Purpose:** The current collector overrides for one agent. Returns
sensible defaults (`disabled_collectors: []`, `collector_intervals: {}`) if
no override row exists yet for that host — never a 404 for an
unconfigured-but-registered host.

**Response:** `AgentConfigOverride`

```json
{
  "disabled_collectors": ["disk"],
  "collector_intervals": { "cpu": 5 }
}
```

### `PUT /api/hosts/:id/config`

**Handler:** `update_agent_config`

**Purpose:** Upserts (replaces, not merges — a `PUT` with an empty
`disabled_collectors` clears any previously-disabled collectors for that
host) the override row, **and** publishes the new config as a **retained**
MQTT message on `sparrow/agents/{id}/config` (Phase 9's config-push
mechanism — see [`docs/architecture.md`](architecture.md) for why
retained is the whole point). An agent that's offline at the moment of
this call still receives the new config the instant it next connects and
subscribes — no polling on the agent's side.

**Request body:** `AgentConfigOverride` (same shape as the `GET` response above).

**Response:** the same `AgentConfigOverride` that was upserted.

---

## AI Health Analyst (`api/analyst.rs`)

### `POST /api/analyst/run`

**Handler:** `run_analysis_handler`

**Purpose:** Runs Sparrow's AI Health Analyst (Phase 10) — either against
a free-form `question`, or, if `question` is omitted and `host_id` is
present, synthesizes a prompt from that host's currently open Problem(s)
(the desktop dashboard's "explain this Problem" quick action, Issue 11.3).
Backed by `nest-ai`'s provider-agnostic `AiService` — the configured
provider (Ollama or Claude) is not part of the request; it's a server-side
configuration concern (`ServerConfig.ollama_*`, see
[`../deploy/README.md`](../deploy/README.md)).

**Request body:** `RunAnalysisRequest`

```json
{
  "host_id": "web-01",
  "question": null,
  "mode": "quick"
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `host_id` | string \| null | one of `host_id`/`question` must be present | Used to synthesize a prompt from open Problems when `question` is omitted. |
| `question` | string \| null | see above | Takes priority over `host_id`-based synthesis when present. |
| `mode` | `"quick"` \| `"report"` | no, defaults to `"quick"` | Both currently behave identically — no provider-agnostic way to request extended thinking effort exists yet in `nest-ai`'s `CompletionRequest` (see `sparrow_core::analyst::loop`'s own doc comment). |

**Response:** `RunAnalysisResponse`

```json
{ "response": "Disk usage on web-01 is at 95%, tripping the configured threshold..." }
```

The model may call one of four tools (host status, metric history, active
Problems, similar past incidents via embedding search) before answering —
that's internal to `run_analysis`'s tool-calling loop
(`sparrow_core::analyst::loop`/`tools`), invisible at the HTTP layer; the
response is always plain text either way.
