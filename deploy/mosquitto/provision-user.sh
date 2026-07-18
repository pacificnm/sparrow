#!/usr/bin/env bash
# Provisions (or updates) a single Mosquitto MQTT user's password (Issue
# 12.2). Manual mosquitto_passwd CLI step, not a self-service API endpoint —
# decided per the issue's own explicit recommendation ("lean toward this
# unless there's a concrete near-term need for self-service onboarding").
#
# Usage: deploy/mosquitto/provision-user.sh <username> [password]
#   username: the MQTT username — "sparrow-server" for the server, or an
#             agent's host_id (must match exactly what
#             crates/agent/src/main.rs's build_mqtt_config sets as the
#             agent's MQTT username, for acl.conf's `pattern ... %u` rules
#             to apply to it correctly).
#   password: if omitted, a random one is generated and printed once —
#             mosquitto_passwd stores only a salted hash, so there is no
#             way to recover a generated password afterward.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PASSWD_FILE="$SCRIPT_DIR/passwd"

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "Usage: $0 <username> [password]" >&2
  exit 1
fi

if ! command -v mosquitto_passwd >/dev/null 2>&1; then
  echo "error: mosquitto_passwd not found on PATH — install mosquitto (or mosquitto-clients on Debian/Ubuntu)" >&2
  exit 1
fi

username="$1"
if [[ -n "${2:-}" ]]; then
  password="$2"
  generated=0
else
  password="$(openssl rand -base64 24)"
  generated=1
fi

if [[ -f "$PASSWD_FILE" ]]; then
  # File exists: -b alone updates/adds this one entry, leaving every other
  # user in the file untouched (verified against mosquitto_passwd's own
  # docs — -c would overwrite the whole file instead, wiping every
  # previously-provisioned user, so it's deliberately not used here).
  mosquitto_passwd -b "$PASSWD_FILE" "$username" "$password"
else
  # First user: -c creates the file.
  mosquitto_passwd -b -c "$PASSWD_FILE" "$username" "$password"
fi

# mosquitto_passwd writes the file mode-0600, owned by whoever ran this
# script (root, if run via `docker run ... mosquitto_passwd` the way this
# repo's own testing does it) — found the hard way (Issue 13.2's real
# docker-compose smoke test): when password_file is bind-mounted into the
# actual broker container, Mosquitto's own non-root runtime user (not root)
# can't open a 0600 file it doesn't own, and fails to start ("Unable to
# open pwfile"). 644 is safe here — the file only ever contains salted
# hashes (mosquitto_passwd's own job), never a plaintext password.
chmod 644 "$PASSWD_FILE"

echo
echo "Provisioned MQTT user '$username' in $PASSWD_FILE"
if [[ "$generated" -eq 1 ]]; then
  echo "Generated password (save it now — it cannot be recovered from $PASSWD_FILE):"
  echo "  $password"
fi
echo "Restart Mosquitto, or send it SIGHUP, to pick up the change."
