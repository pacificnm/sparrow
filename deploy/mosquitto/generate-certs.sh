#!/usr/bin/env bash
# Generates a self-signed CA + server certificate for Mosquitto's TLS
# listener (Issue 12.1). Self-signed is fine for a self-hosted deployment —
# distribute ca.crt to every client that needs to verify the broker.
#
# Usage: deploy/mosquitto/generate-certs.sh [common-name] [san...]
#   common-name: server cert CN, default "mosquitto.local"
#   san...:      extra Subject Alternative Names (DNS or IP), space-separated,
#                default "localhost 127.0.0.1"
#
# Example (broker reachable as mqtt.internal and 10.0.0.5):
#   deploy/mosquitto/generate-certs.sh mqtt.internal mqtt.internal 10.0.0.5
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CERTS_DIR="$SCRIPT_DIR/certs"

COMMON_NAME="${1:-mosquitto.local}"
shift || true
SANS=("$@")
if [[ ${#SANS[@]} -eq 0 ]]; then
  SANS=("$COMMON_NAME" "localhost" "127.0.0.1")
fi

mkdir -p "$CERTS_DIR"
cd "$CERTS_DIR"

if [[ -f ca.key ]]; then
  echo "error: $CERTS_DIR/ca.key already exists — remove certs/ first if you want to regenerate" >&2
  exit 1
fi

echo "Generating CA + server certificate for CN=$COMMON_NAME in $CERTS_DIR"

# 1. CA private key.
openssl genrsa -out ca.key 4096

# 2. CA self-signed certificate, 10 years — this is the file to distribute
#    to every client (nest_mqtt's TlsConfig::from_ca_file / rumqttc's
#    TlsConfiguration::Simple.ca) so it can verify the server cert below.
openssl req -x509 -new -nodes -key ca.key -sha256 -days 3650 \
  -subj "/CN=Sparrow MQTT CA" -out ca.crt

# 3. Server private key.
openssl genrsa -out server.key 2048

# 4. Server certificate signing request.
openssl req -new -key server.key -subj "/CN=$COMMON_NAME" -out server.csr

# 5. Server certificate, signed by the CA, 1 year, with the SAN list
#    Mosquitto's TLS handshake needs (clients validate the hostname they
#    dialed against this, not just the CN).
san_list=""
for san in "${SANS[@]}"; do
  if [[ "$san" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    entry="IP:$san"
  else
    entry="DNS:$san"
  fi
  san_list="${san_list:+$san_list,}$entry"
done
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out server.crt -days 365 -sha256 \
  -extfile <(printf "subjectAltName=%s" "$san_list")

rm -f server.csr

# ca.key is never mounted into the broker container (only needed here, to
# sign a cert) - stays maximally restricted. server.key IS mounted (it's
# Mosquitto's `keyfile`) - found the hard way (Issue 13.2's real
# docker-compose smoke test) that a bind-mounted 0600 file keeps its exact
# host owner/mode inside the container, and Mosquitto's own non-root
# runtime user can't open a file it doesn't own, so the broker fails to
# start ("Unable to load server key file... Permission denied") even
# though the file is right there. 644 is the accepted tradeoff for a
# bind-mount deployment (any local process on the host can read it) unless
# you're using a real secrets-management setup — still far better than
# committing it to git, which .gitignore already prevents.
chmod 600 ca.key
chmod 644 server.key ca.crt server.crt

echo
echo "Done. Generated in $CERTS_DIR:"
echo "  ca.crt      - distribute to clients (nest_mqtt TlsConfig ca_cert)"
echo "  ca.key      - broker CA private key, keep secret, do not distribute"
echo "  server.crt  - Mosquitto's cafile pairs with server.crt/server.key below"
echo "  server.key  - Mosquitto's keyfile. Broker-readable (644), not 600 —"
echo "                bind-mounted into the container, whose own non-root"
echo "                user needs to read it; see this script's own comment"
echo "                above for why."
echo
echo "Verify: openssl verify -CAfile $CERTS_DIR/ca.crt $CERTS_DIR/server.crt"
