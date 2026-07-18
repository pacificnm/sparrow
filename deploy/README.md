# Sparrow deployment

Deployment artifacts for running Sparrow outside a dev checkout. Grows across
Phase 12 (security hardening) and Phase 13 (packaging/deployment) — this
file is expected to gain a `docker-compose.yml` and `systemd` units later;
only what those phases have actually delivered so far is listed below.

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
