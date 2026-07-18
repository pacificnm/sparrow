# Sparrow deployment

Deployment artifacts for running Sparrow outside a dev checkout. Grows across
Phase 12 (security hardening) and Phase 13 (packaging/deployment) — this
file is expected to gain a `docker-compose.yml` and `systemd` units later;
only what those phases have actually delivered so far is listed below.

## Mosquitto TLS (Issue 12.1)

[`mosquitto/`](mosquitto/) — self-signed CA + server certificate generation
(`generate-certs.sh`) and an example `mosquitto.conf` with a TLS listener on
`8883`. See [`mosquitto/README.md`](mosquitto/README.md) for the exact
`openssl` commands and how to point `nest_mqtt::MqttConfig` at it.

Broker ACLs and per-agent credential provisioning (Issue 12.2) aren't
configured yet.
