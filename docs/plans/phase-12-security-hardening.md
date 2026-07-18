# Phase 12 Task Spec — Security Hardening

**Repo:** `pacificnm/sparrow` (broker config) + `pacificnm/nest` (`nest-mqtt` config surface extension, if needed)
**Prerequisite:** Phase 2 (`nest-mqtt`), Phase 6 (agent), Phase 7 (server) — this phase assumes a working plaintext deployment already exists and is being hardened, not built fresh.

## Scope

Two layers: (1) transport security (TLS on the Mosquitto connection), (2)
authorization (each agent can only publish/subscribe to its own topics).
Both are broker-config-plus-client-config changes, not new application code
— `nest-mqtt`'s config surface (Phase 2: username/password already present
in `MqttConfig`) was deliberately built with this phase in mind.

### 1. TLS

- Mosquitto: generate a CA + server cert (self-signed is fine for a
  self-hosted deployment; document the exact `openssl` commands used, don't
  just say "generate a cert" and leave it to guesswork), configure
  `listener 8883` with `cafile`/`certfile`/`keyfile` in `mosquitto.conf`.
- `nest-mqtt` client side: **check whether `rumqttc`'s `MqttOptions` already
  exposes TLS configuration** (it does, via `MqttOptions::set_transport` with
  a `Transport::Tls(...)` variant per `rumqttc`'s general design — confirm
  the exact 0.25 API against `cargo doc` before writing this, the same "check
  don't guess" instruction from Phase 2). If `nest_mqtt::MqttConfig` (Phase
  2) doesn't yet expose a way to plumb TLS options through, that's a small
  addition to the framework module (add `tls: Option<TlsConfig>` to
  `MqttConfig`, mirroring the shape of `LastWillConfig`), not a Sparrow-side
  workaround.

### 2. Broker ACLs

Mosquitto's `acl_file` mechanism, scoped per the topic taxonomy from Phase
4:

```
# Each agent (authenticated via its own username, matching host_id) can only
# touch its own topics. `pattern`, not `user <name>` + bare `topic` — verified
# against Mosquitto's real ACL file syntax (mosquitto-conf(5)) that `%u`
# substitution is only valid in `pattern` lines. `user <name>` matches an
# exact literal username only, so a `user agent-%u` line (an earlier version
# of this sketch had one) is not valid Mosquitto config at all. `pattern`
# rules apply globally to every connecting user, not just ones without a
# more specific `user` block, so this isn't nested under one.
pattern write sparrow/agents/%u/register
pattern write sparrow/agents/%u/heartbeat
pattern write sparrow/agents/%u/data
pattern read sparrow/agents/%u/config
pattern read sparrow/agents/%u/command

# The server has broader access — reads everything, writes config/commands.
# Exact-match on the literal username "sparrow-server", additive on top of
# the pattern rules above (which also apply to sparrow-server itself, but
# only grant it the harmless, never-used "sparrow/agents/sparrow-server/…"
# topics — not access to any real agent's topics).
user sparrow-server
topic read sparrow/agents/+/register
topic read sparrow/agents/+/heartbeat
topic read sparrow/agents/+/data
topic write sparrow/agents/+/config
topic write sparrow/agents/+/command
```

`%u` is Mosquitto's ACL-file substitution for the connecting username — set
each agent's MQTT `client_id`/username to its `host_id` at connect time
(Phase 6's `AgentConfig` already has `host_id`; wire it into
`MqttConfig::client_id`/`username` when constructing the agent's MQTT
connection, if not already done that way).

Per-agent passwords: Mosquitto's `password_file` (hashed via
`mosquitto_passwd`), generated at agent-provisioning time — this implies a
small provisioning step (a server-side "register a new agent, get back
credentials" flow) that doesn't exist yet in any prior phase. Decide during
this phase whether that's a manual `mosquitto_passwd` CLI step (fine for a
self-hosted v1) or a proper API endpoint (`POST /api/agents/provision`) —
lean toward the manual step for v1 unless there's a concrete near-term need
for self-service agent onboarding; don't build API surface speculatively.

## Tests

- A connection attempt with wrong/missing credentials is rejected by the
  broker (this is a broker-config test, not really a Rust unit test — verify
  manually or via a small integration test that spins up Mosquitto with the
  ACL file via `testcontainers`, attempts a connect with bad credentials,
  asserts it fails).
- An agent authenticated as `host-a` attempting to publish under
  `sparrow/agents/host-b/data` is rejected — this is the actual ACL
  correctness test, and the one worth spending real effort on; a
  misconfigured ACL file that accidentally allows cross-host publishing
  would be a real security bug, not just a test gap.
- TLS: a plaintext connection attempt to the TLS-only listener port fails;
  a properly-configured TLS client connects successfully.

**Acceptance:** all three test categories above pass under `testcontainers`
where feasible; the cross-host-publish-rejection test in particular should
not be skipped or deferred — flag it explicitly if it can't be automated for
some reason, don't just leave it as a manual-only check without saying so.

## Explicit "do not" list

- Do not skip the cross-host ACL rejection test — it's the one test that
  actually proves this phase did anything.
- Do not add agent self-service provisioning API speculatively — decide the
  provisioning approach explicitly per the note above, default to the manual
  path unless there's a stated reason not to.
- Do not guess `rumqttc`'s TLS config API — check `cargo doc` for the pinned
  0.25 version first, same discipline as every other `rumqttc` detail
  flagged in Phase 2.
