//! Issue 12.3 — TLS + ACL enforcement against a real Mosquitto broker,
//! configured with the *actual* deployment files from `deploy/mosquitto/`
//! (loaded via `include_str!`, not re-typed here — these tests fail if the
//! real deployment files and this test suite drift apart).
//!
//! **Honest limitation, read before trusting these tests uncritically**:
//! `nest_mqtt::MqttClient::connect` never surfaces a connection failure to
//! its caller. The background event-loop task it spawns
//! (`nest-mqtt/src/client.rs::run_event_loop`) treats every `poll()` error —
//! including a broker refusing bad credentials, or a TLS handshake failing —
//! as transient, logs a `tracing::warn!`, and retries forever. There is no
//! public API on `MqttClient` to ask "did the last connection attempt
//! succeed or get rejected". So none of the tests below can assert "the
//! broker returned CONNACK code 5 (not authorized)" directly; instead they
//! assert the strongest thing actually observable through the existing
//! public API: a legitimate, independently-authenticated subscriber never
//! receives a message that a rejected client attempted to publish. That's
//! the practically meaningful property (no unauthorized data gets through),
//! but it doesn't distinguish "rejected at CONNECT" from "rejected at
//! PUBLISH due to an ACL violation" — which is fine here, since the ACL
//! test *needs* the PUBLISH-level distinction anyway (a cross-host publish
//! is authenticated, just not authorized).
//!
//! Run with `cargo test -p sparrow-agent --test broker_security_live --
//! --test-threads=1` (Docker required — each test starts its own broker
//! plus a throwaway passwd-provisioning container, so they're slow;
//! `--test-threads=1` avoids competing for the same host ports/resources).
//! All three tests here were run for real, repeatedly, while writing this
//! file — including catching two real bugs along the way:
//!
//! 1. `nest_mqtt`'s TLS support never installed a rustls `CryptoProvider`,
//!    which panics (in the background event-loop task, not visibly to the
//!    caller) the first time a real TLS handshake happens, if more than one
//!    crypto backend is linked into the binary (both `ring` and
//!    `aws-lc-rs` are, here) — fixed in `nest_mqtt::client.rs`
//!    (`ensure_crypto_provider_installed`), in the same `pacificnm/nest`
//!    branch as Issue 12.1's TLS support.
//! 2. An earlier draft of this file gave every test client the same literal
//!    `client_id`, which made the broker treat each new connection as the
//!    previous one reconnecting — disconnecting it ("session taken over")
//!    in an infinite loop that looked exactly like an ACL/auth problem but
//!    wasn't one. Every client below now gets a distinct `client_id`.

use std::time::Duration;

use nest_mqtt::{MqttClient, MqttConfig, MqttQos, TlsConfig};
use testcontainers::core::{ExecCommand, IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// The actual deployment files — not a copy. If `deploy/mosquitto/*` and
/// this test drift, this fails to compile or fails at runtime, rather than
/// silently testing something that no longer matches what's deployed.
const MOSQUITTO_CONF: &str = include_str!("../../../deploy/mosquitto/mosquitto.conf");
const ACL_CONF: &str = include_str!("../../../deploy/mosquitto/acl.conf");

const SPARROW_SERVER_PASSWORD: &str = "sparrow-server-test-password";
const HOST_A_PASSWORD: &str = "host-a-test-password";
const HOST_B_PASSWORD: &str = "host-b-test-password";

/// Generates a CA + server certificate, mirroring
/// `deploy/mosquitto/generate-certs.sh`'s exact `openssl` invocations (not
/// a different, drifted implementation) — see that script for the
/// documented rationale of each step.
async fn generate_certs(common_name: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let dir = std::env::temp_dir().join(format!(
        "sparrow-broker-security-test-certs-{}-{}",
        std::process::id(),
        common_name
    ));
    tokio::fs::create_dir_all(&dir)
        .await
        .expect("create temp cert dir");

    let ca_key = dir.join("ca.key");
    let ca_crt = dir.join("ca.crt");
    let server_key = dir.join("server.key");
    let server_csr = dir.join("server.csr");
    let server_crt = dir.join("server.crt");

    run_openssl(&["genrsa", "-out", path_str(&ca_key), "4096"]).await;
    run_openssl(&[
        "req",
        "-x509",
        "-new",
        "-nodes",
        "-key",
        path_str(&ca_key),
        "-sha256",
        "-days",
        "3650",
        "-subj",
        "/CN=Sparrow MQTT Test CA",
        "-out",
        path_str(&ca_crt),
    ])
    .await;
    run_openssl(&["genrsa", "-out", path_str(&server_key), "2048"]).await;
    run_openssl(&[
        "req",
        "-new",
        "-key",
        path_str(&server_key),
        "-subj",
        &format!("/CN={common_name}"),
        "-out",
        path_str(&server_csr),
    ])
    .await;
    run_openssl(&[
        "x509",
        "-req",
        "-in",
        path_str(&server_csr),
        "-CA",
        path_str(&ca_crt),
        "-CAkey",
        path_str(&ca_key),
        "-CAcreateserial",
        "-out",
        path_str(&server_crt),
        "-days",
        "365",
        "-sha256",
        "-extfile",
        &{
            // -extfile needs a real file, not process substitution (that's
            // a bash-ism generate-certs.sh uses interactively; this test
            // shells out to openssl directly, so write it to a plain file
            // instead of relying on a shell feature).
            let ext_file = dir.join("ext.cnf");
            tokio::fs::write(
                &ext_file,
                format!("subjectAltName=DNS:{common_name},DNS:localhost,IP:127.0.0.1"),
            )
            .await
            .expect("write extfile");
            path_str(&ext_file).to_string()
        },
    ])
    .await;

    let ca = tokio::fs::read(&ca_crt).await.expect("read ca.crt");
    let crt = tokio::fs::read(&server_crt).await.expect("read server.crt");
    let key = tokio::fs::read(&server_key).await.expect("read server.key");
    let _ = tokio::fs::remove_dir_all(&dir).await;
    (ca, crt, key)
}

fn path_str(path: &std::path::Path) -> &str {
    path.to_str().expect("utf8 temp path")
}

async fn run_openssl(args: &[&str]) {
    let output = tokio::process::Command::new("openssl")
        .args(args)
        .output()
        .await
        .expect("failed to run openssl - is it installed?");
    assert!(
        output.status.success(),
        "openssl {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Generates `mosquitto`'s `password_file` content for the given
/// `(username, password)` pairs by running `mosquitto_passwd` *inside a
/// throwaway `eclipse-mosquitto` container* — that image ships
/// `mosquitto_passwd`, so this doesn't require the test-runner host to have
/// it installed (it doesn't, in the sandbox that wrote this).
async fn generate_passwd_file(users: &[(&str, &str)]) -> Vec<u8> {
    let setup = GenericImage::new("eclipse-mosquitto", "2")
        .with_wait_for(WaitFor::message_on_stderr("running"))
        .start()
        .await
        .expect("failed to start mosquitto_passwd setup container");

    for (index, (username, password)) in users.iter().enumerate() {
        let mut cmd: Vec<String> = vec!["mosquitto_passwd".to_string(), "-b".to_string()];
        if index == 0 {
            // -c creates the file - only on the very first user, exactly
            // like provision-user.sh's own guard against wiping existing
            // entries.
            cmd.push("-c".to_string());
        }
        cmd.push("/mosquitto/config/passwd".to_string());
        cmd.push((*username).to_string());
        cmd.push((*password).to_string());

        let mut result = setup
            .exec(ExecCommand::new(cmd))
            .await
            .expect("exec mosquitto_passwd");
        // exit_code() reports None until the exec's output stream has been
        // drained (that's how testcontainers learns the process actually
        // exited) - read stdout first, even though we don't need its
        // content, purely to wait for completion.
        let output = result
            .stdout_to_vec()
            .await
            .expect("drain mosquitto_passwd stdout");
        let exit_code = result
            .exit_code()
            .await
            .expect("mosquitto_passwd exit code");
        assert_eq!(
            exit_code,
            Some(0),
            "mosquitto_passwd should succeed for {username}: {}",
            String::from_utf8_lossy(&output)
        );
    }

    let mut cat = setup
        .exec(ExecCommand::new(["cat", "/mosquitto/config/passwd"]))
        .await
        .expect("exec cat passwd");
    cat.stdout_to_vec()
        .await
        .expect("read generated passwd file")
}

struct SecuredBroker {
    #[allow(dead_code)]
    container: ContainerAsync<GenericImage>,
    host: String,
    tls_port: u16,
    ca_cert: Vec<u8>,
}

/// Starts Mosquitto with the *real* `deploy/mosquitto/mosquitto.conf` +
/// `acl.conf`, a freshly generated CA/server cert, and a passwd file
/// provisioned for `sparrow-server`, `host-a`, and `host-b`.
async fn start_secured_broker() -> SecuredBroker {
    let (ca, server_crt, server_key) = generate_certs("mosquitto-test").await;
    let passwd = generate_passwd_file(&[
        ("sparrow-server", SPARROW_SERVER_PASSWORD),
        ("host-a", HOST_A_PASSWORD),
        ("host-b", HOST_B_PASSWORD),
    ])
    .await;

    let container = GenericImage::new("eclipse-mosquitto", "2")
        .with_exposed_port(8883.tcp())
        .with_wait_for(WaitFor::message_on_stderr("running"))
        .with_copy_to(
            "/mosquitto/config/mosquitto.conf",
            MOSQUITTO_CONF.as_bytes().to_vec(),
        )
        .with_copy_to("/mosquitto/config/acl.conf", ACL_CONF.as_bytes().to_vec())
        .with_copy_to("/mosquitto/config/certs/ca.crt", ca.clone())
        .with_copy_to("/mosquitto/config/certs/server.crt", server_crt)
        .with_copy_to("/mosquitto/config/certs/server.key", server_key)
        .with_copy_to("/mosquitto/config/passwd", passwd)
        .start()
        .await
        .expect("failed to start secured mosquitto testcontainer");

    let host = container
        .get_host()
        .await
        .expect("container host")
        .to_string();
    let tls_port = container
        .get_host_port_ipv4(8883)
        .await
        .expect("container tls port");

    SecuredBroker {
        container,
        host,
        tls_port,
        ca_cert: ca,
    }
}

/// `client_id` must be unique per connection - reusing one across multiple
/// simultaneous clients (an earlier draft of this file hardcoded a single
/// literal for all of them) makes the broker treat each new connection as
/// the same client reconnecting, disconnecting the previous one ("session
/// taken over") in an infinite loop, which looks like a broker/ACL problem
/// but isn't one.
fn tls_config(broker: &SecuredBroker, client_id: &str) -> MqttConfig {
    MqttConfig::new(&broker.host, broker.tls_port, client_id)
        .with_tls(TlsConfig::new(broker.ca_cert.clone()))
}

/// The real ACL correctness test — the one worth the most effort per the
/// issue. `host-a` (correctly authenticated as itself) attempts to publish
/// under `host-b`'s data topic; a `sparrow-server`-authenticated subscriber
/// (allowed to read every agent's data topic) must never see it. A positive
/// control in the same test (`host-b` publishing to its *own* topic) proves
/// the ACL isn't just blocking everything, which would make the negative
/// assertion meaningless.
#[tokio::test(flavor = "multi_thread")]
async fn cross_host_publish_is_rejected_by_the_acl() {
    let broker = start_secured_broker().await;

    let observer_config = tls_config(&broker, "cross-host-observer")
        .with_credentials("sparrow-server", SPARROW_SERVER_PASSWORD);
    let observer = MqttClient::connect(&observer_config)
        .await
        .expect("client construction does not require a live connection");
    let host_b_messages = observer
        .subscribe("sparrow/agents/host-b/data", MqttQos::AtLeastOnce)
        .await
        .expect("subscribe");
    let mut host_b_messages = std::pin::pin!(host_b_messages);

    // Give the SUBSCRIBE a moment to actually land before anyone publishes -
    // same reasoning as nest-mqtt's own publish_and_subscribe_round_trip.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // host-a tries to publish under host-b's topic - authenticated, but not
    // authorized for this topic per acl.conf's `pattern write
    // sparrow/agents/%u/data` (substitutes host-a's own username, "host-a",
    // not "host-b").
    let host_a_config =
        tls_config(&broker, "cross-host-host-a").with_credentials("host-a", HOST_A_PASSWORD);
    let host_a_client = MqttClient::connect(&host_a_config)
        .await
        .expect("client construction does not require a live connection");
    host_a_client
        .publish(
            "sparrow/agents/host-b/data",
            b"forged data".to_vec(),
            MqttQos::AtLeastOnce,
            false,
        )
        .await
        .ok(); // may itself error, or may be silently dropped by the ACL - either is fine, the assertion below is what matters

    let forged_message_arrived = tokio::time::timeout(
        Duration::from_secs(3),
        futures_util::StreamExt::next(&mut host_b_messages),
    )
    .await
    .is_ok();
    assert!(
        !forged_message_arrived,
        "host-a must not be able to publish under host-b's data topic"
    );

    // Positive control: host-b publishing to its OWN topic must still work,
    // proving the ACL is scoping access correctly rather than denying
    // everything (which would make the assertion above trivially true for
    // the wrong reason).
    let host_b_config =
        tls_config(&broker, "cross-host-host-b").with_credentials("host-b", HOST_B_PASSWORD);
    let host_b_client = MqttClient::connect(&host_b_config)
        .await
        .expect("client construction does not require a live connection");
    host_b_client
        .publish(
            "sparrow/agents/host-b/data",
            b"real data".to_vec(),
            MqttQos::AtLeastOnce,
            false,
        )
        .await
        .expect("host-b publishing to its own data topic should succeed");

    let real_message = tokio::time::timeout(
        Duration::from_secs(5),
        futures_util::StreamExt::next(&mut host_b_messages),
    )
    .await
    .expect("timed out waiting for host-b's own legitimate publish")
    .expect("message stream ended unexpectedly");
    assert_eq!(real_message.payload, b"real data");
}

/// A connection attempt with wrong credentials never gets data through.
/// See the file-level doc comment for why this can't assert a specific
/// CONNACK rejection code through `nest_mqtt::MqttClient`'s current API.
#[tokio::test(flavor = "multi_thread")]
async fn wrong_credentials_are_rejected() {
    let broker = start_secured_broker().await;

    let observer_config = tls_config(&broker, "wrong-creds-observer")
        .with_credentials("sparrow-server", SPARROW_SERVER_PASSWORD);
    let observer = MqttClient::connect(&observer_config)
        .await
        .expect("client construction does not require a live connection");
    let messages = observer
        .subscribe("sparrow/agents/host-a/data", MqttQos::AtLeastOnce)
        .await
        .expect("subscribe");
    let mut messages = std::pin::pin!(messages);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let bad_config = tls_config(&broker, "wrong-creds-bad-client")
        .with_credentials("host-a", "definitely-the-wrong-password");
    let bad_client = MqttClient::connect(&bad_config)
        .await
        .expect("client construction does not require a live connection");
    bad_client
        .publish(
            "sparrow/agents/host-a/data",
            b"should never arrive".to_vec(),
            MqttQos::AtLeastOnce,
            false,
        )
        .await
        .ok();

    let arrived = tokio::time::timeout(
        Duration::from_secs(3),
        futures_util::StreamExt::next(&mut messages),
    )
    .await
    .is_ok();
    assert!(
        !arrived,
        "a client with the wrong password must not reach the broker"
    );
}

/// TLS: a plaintext connection attempt to the TLS-only listener port fails
/// (no data gets through); a properly-configured TLS client connects and
/// publishes successfully (proven directly by
/// `cross_host_publish_is_rejected_by_the_acl`'s positive control above, so
/// this test only covers the plaintext-fails half).
#[tokio::test(flavor = "multi_thread")]
async fn plaintext_connection_to_the_tls_listener_fails() {
    let broker = start_secured_broker().await;

    let observer_config = tls_config(&broker, "plaintext-test-observer")
        .with_credentials("sparrow-server", SPARROW_SERVER_PASSWORD);
    let observer = MqttClient::connect(&observer_config)
        .await
        .expect("client construction does not require a live connection");
    let messages = observer
        .subscribe("sparrow/agents/host-a/data", MqttQos::AtLeastOnce)
        .await
        .expect("subscribe");
    let mut messages = std::pin::pin!(messages);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // No .with_tls(...) - a plain TCP client dialing the TLS-only port.
    let plaintext_config = MqttConfig::new(&broker.host, broker.tls_port, "plaintext-client")
        .with_credentials("host-a", HOST_A_PASSWORD);
    let plaintext_client = MqttClient::connect(&plaintext_config)
        .await
        .expect("client construction does not require a live connection");
    plaintext_client
        .publish(
            "sparrow/agents/host-a/data",
            b"should never arrive".to_vec(),
            MqttQos::AtLeastOnce,
            false,
        )
        .await
        .ok();

    let received = tokio::time::timeout(
        Duration::from_secs(3),
        futures_util::StreamExt::next(&mut messages),
    )
    .await
    .is_ok();
    assert!(
        !received,
        "a plaintext client must not reach the TLS-only listener"
    );
}
