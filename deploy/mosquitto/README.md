# Mosquitto TLS + auth/ACL setup (Issues 12.1, 12.2)

Self-signed CA + server certificate for Mosquitto's TLS listener. Self-signed
is fine for a self-hosted deployment — you control both the broker and every
client, so there's no third party to trust a CA on your behalf.

## 1. Generate certificates

```bash
cd deploy/mosquitto
./generate-certs.sh                                    # CN=mosquitto.local, SANs: mosquitto.local, localhost, 127.0.0.1
./generate-certs.sh mqtt.internal mqtt.internal 10.0.0.5  # custom hostname + extra SANs
```

This runs, in order (see `generate-certs.sh` for the exact invocations —
nothing here is hidden behind a wrapper you can't read):

1. `openssl genrsa -out ca.key 4096` — the CA's private key.
2. `openssl req -x509 -new -nodes -key ca.key -sha256 -days 3650 -subj "/CN=Sparrow MQTT CA" -out ca.crt` — a self-signed CA certificate, valid 10 years.
3. `openssl genrsa -out server.key 2048` — the broker's private key.
4. `openssl req -new -key server.key -subj "/CN=<hostname>" -out server.csr` — a certificate signing request for the broker.
5. `openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 365 -sha256 -extfile <(printf "subjectAltName=...")` — signs the CSR with the CA, embedding the Subject Alternative Names clients will actually connect to (required — TLS clients validate the hostname/IP they dialed against SAN entries, not just the CN).

Output lands in `certs/` (gitignored — every deployment generates its own,
never commit `ca.key`/`server.key`):

| File | Purpose |
|------|---------|
| `certs/ca.crt` | Distribute to every client that needs to verify the broker (`nest_mqtt::TlsConfig::from_ca_file`). Public, not a secret. |
| `certs/ca.key` | The CA's private key. **Keep secret**, mode 600 — anyone with this can mint certificates your clients will trust. Never mounted into the broker container. |
| `certs/server.crt` | Mosquitto's `certfile`. Public. |
| `certs/server.key` | Mosquitto's `keyfile`. Sensitive, but mode **644** — bind-mounted into the broker container, whose own non-root user needs to read it; a bind mount keeps the host file's exact owner/mode, so 600 makes Mosquitto fail to start ("Unable to load server key file... Permission denied"). Accepted tradeoff for a bind-mount deployment; use a real secrets-management setup if that's not acceptable for your environment. |

Verify the chain manually if you want to double-check before deploying:

```bash
openssl verify -CAfile certs/ca.crt certs/server.crt
```

## 2. Provision credentials

Every connecting client needs a username/password before the broker will
accept it (`allow_anonymous false`) — provision one per agent `host_id`,
plus one for the server:

```bash
cd deploy/mosquitto
./provision-user.sh sparrow-server               # random password, printed once
./provision-user.sh web-01 "$(openssl rand -base64 24)"  # or supply your own
```

`provision-user.sh` wraps `mosquitto_passwd -b passwd <username> <password>`
(`-c` only on the very first user, to create `passwd` — never on an
existing file, which would silently wipe every other provisioned user).
This is a **manual** step by design, not a self-service API endpoint —
decided per Issue 12.2's own instruction to lean toward the manual path for
a self-hosted v1 unless there's a concrete near-term need for self-service
onboarding. `passwd` is gitignored; re-run `provision-user.sh` on the
broker host whenever a new agent is added.

**The agent's MQTT username must be its own `host_id`** — that's what
`acl.conf`'s `pattern` rules (below) scope access to. Set
`mqtt_password` in the agent's `[agent]` config section (see
`crates/agent/src/config.rs`); `crates/agent/src/main.rs`'s
`build_mqtt_config` then sets `username = host_id` automatically. Without
`mqtt_password` set, the agent connects with no credentials at all, which a
broker configured per this directory will reject outright.

## 3. Configure Mosquitto

`mosquitto.conf` in this directory sets:

```
listener 8883
cafile /mosquitto/config/certs/ca.crt
certfile /mosquitto/config/certs/server.crt
keyfile /mosquitto/config/certs/server.key

allow_anonymous false
password_file /mosquitto/config/passwd
acl_file /mosquitto/config/acl.conf
```

Mount this directory's `mosquitto.conf`, `certs/`, `passwd`, and `acl.conf`
into the container (or copy them to wherever your Mosquitto install reads
config from) so those paths resolve. `passwd` must exist (step 2) before
Mosquitto will start with this config at all.

### ACL rules (`acl.conf`)

Scoped per the Phase 4 topic taxonomy: each agent can only write its own
`register`/`heartbeat`/`data` and read its own `config`/`command`; the
server reads everything and writes `config`/`command` for any agent.

The phase-12 spec's own sketch used `user agent-%u` for the per-agent
rules — verified against Mosquitto's real ACL file syntax
([`mosquitto-conf(5)`](https://mosquitto.org/man/mosquitto-conf-5.html))
that this is invalid: the `user <name>` directive matches an **exact
literal username only**, `%u` substitution doesn't work there. The
correct directive for "scope topic access to whatever username actually
connected" is `pattern`, which applies globally to every authenticated
user (not nested under a `user` line) — that's what `acl.conf` uses
instead. See the comments in that file for the full reasoning.

## 4. Configure the client (`nest_mqtt`)

```rust
use nest_mqtt::{MqttConfig, TlsConfig};

let config = MqttConfig::new("mqtt.internal", 8883, "my-client")
    .with_tls(TlsConfig::from_ca_file("deploy/mosquitto/certs/ca.crt")?);
```

Or via a config file's `[mqtt]` section:

```toml
[mqtt]
broker_host = "mqtt.internal"
broker_port = 8883
tls_ca_file = "deploy/mosquitto/certs/ca.crt"
```

`tls_client_cert_file` + `tls_client_key_file` (both required together) add
mutual TLS if you need it — not required for this deployment, since the CA
only needs to authenticate the broker to clients, not the other way around.

## Acceptance

A plaintext connection attempt to port `8883` must fail; a properly
TLS-and-credential-configured client connects; an agent authenticated as
`host-a` attempting to publish under `sparrow/agents/host-b/data` is
rejected by `acl.conf` — all covered by Issue 12.3's test suite in this
repo, not here.
