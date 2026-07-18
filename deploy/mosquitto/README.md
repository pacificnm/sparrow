# Mosquitto TLS setup (Issue 12.1)

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
| `certs/ca.key` | The CA's private key. **Keep secret** — anyone with this can mint certificates your clients will trust. |
| `certs/server.crt` | Mosquitto's `certfile`. Public. |
| `certs/server.key` | Mosquitto's `keyfile`. **Keep secret.** |

Verify the chain manually if you want to double-check before deploying:

```bash
openssl verify -CAfile certs/ca.crt certs/server.crt
```

## 2. Configure Mosquitto

`mosquitto.conf` in this directory sets:

```
listener 8883
cafile /mosquitto/config/certs/ca.crt
certfile /mosquitto/config/certs/server.crt
keyfile /mosquitto/config/certs/server.key
```

Mount this directory's `mosquitto.conf` and `certs/` into the container (or
copy them to wherever your Mosquitto install reads config from) so those
paths resolve. `allow_anonymous true` is left as-is here deliberately —
authentication (`password_file`) and per-agent authorization (`acl_file`)
are Issue 12.2's scope, not this one.

## 3. Configure the client (`nest_mqtt`)

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

A plaintext connection attempt to port `8883` must fail, and a client
configured with `ca.crt` above must connect successfully — covered by
Issue 12.3's test suite in this repo, not here.
