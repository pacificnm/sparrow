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

## Agent release build + systemd unit (Issue 13.1)

`crates/agent` needs no custom `[profile.release]` — checked the two
existing Nest CLI products (`pacificnm/airtable-sync`, `pacificnm/loon`)
directly on GitHub rather than inventing settings, and neither has one
either. `./build build` (rust profile) already produces a single static
binary via plain `cargo build --release` with no changes needed.

[`sparrow-agent.service`](sparrow-agent.service) — install as
`/etc/systemd/system/sparrow-agent.service`:

```bash
sudo useradd --system --no-create-home sparrow-agent
sudo cp target/release/sparrow-agent /usr/local/bin/sparrow-agent
sudo mkdir -p /etc/sparrow && sudo cp config.toml /etc/sparrow/agent.toml
sudo cp deploy/sparrow-agent.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now sparrow-agent
```

**Elevated permissions**: not needed. `sysinfo`'s Linux disk backend
(`sysinfo-0.39.6/src/unix/linux/disk.rs`, checked directly rather than
assumed) reads `/proc/mounts` (world-readable) and calls `statvfs()` per
mount — a standard POSIX call requiring only execute/traverse permission
on the mount point, not root or any capability, for ordinary local
filesystems (ext4, xfs, btrfs, …). `User=sparrow-agent` (no
`CAP_*`/`AmbientCapabilities`) is sufficient for cpu/memory/disk
collection on a standard host.

One real, source-verified caveat, distinct from a permissions concern:
sysinfo's own code comments warn that calling `statvfs()` on an NFS or
CIFS mount using the `hard` mount option **can hang**, not just fail —
if a target host has such a mount, disk collection could stall
indefinitely on it. Not something a service user/capability change can
fix; if a host has `hard`-mounted network filesystems, either mount them
`soft` (accepting the usual NFS-soft tradeoffs) or exclude them from
what `DiskCollector` enumerates (no such exclusion mechanism exists yet —
out of this issue's scope, worth its own issue if a real deployment hits
this).

Restart-survival (`systemctl restart sparrow-agent` picks back up
publishing without duplicate registration) is Issue 13.3's acceptance
check, not this one's — this section only covers the build + unit file.
