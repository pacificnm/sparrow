//! Shared helpers for `crates/agent`'s live-broker integration tests.
//!
//! Each `tests/*.rs` file compiles as its own separate crate, so this can't
//! be `mod support;`-shared from `src/` (that would pull `testcontainers`
//! into the library build). `nest-mqtt` has its own equivalent
//! (`src/test_support.rs`), but it's a private `mod`, not `pub`, so it isn't
//! reachable from here — this mirrors its exact, already-proven recipe
//! rather than guessing at a new one.

use nest_core::AppBuilder;
use nest_task_runtime::{TaskManagerConfig, TaskManagerService};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

use sparrow_agent::config::AgentConfig;

/// Holds a running Mosquitto container alive for the test's duration;
/// dropping it stops the container.
pub struct TestBroker {
    // Read directly (stop/start/re-check the port) by agent_integration.rs's
    // broker-restart test; config_reload_live.rs doesn't need it — shared
    // by multiple tests/*.rs binaries, each using a different subset.
    #[allow(dead_code)]
    pub container: ContainerAsync<GenericImage>,
    pub host: String,
    pub port: u16,
}

/// Starts a disposable Mosquitto broker and returns its host/port.
///
/// The stock `eclipse-mosquitto:2` image's default config already sets
/// `listener 1883` + `allow_anonymous true` (confirmed against a running
/// container by `nest-mqtt`'s own equivalent helper) — no config mount needed.
pub async fn start_broker() -> TestBroker {
    let container = GenericImage::new("eclipse-mosquitto", "2")
        .with_exposed_port(1883.tcp())
        // Mosquitto logs to stderr, not stdout. The version suffix in
        // "mosquitto version X.Y.Z running" changes across image updates,
        // so match on the stable "running" suffix instead of a pinned
        // version string.
        .with_wait_for(WaitFor::message_on_stderr("running"))
        .start()
        .await
        .expect("failed to start mosquitto testcontainer");
    let host = container
        .get_host()
        .await
        .expect("container host")
        .to_string();
    let port = container
        .get_host_port_ipv4(1883)
        .await
        .expect("container port");
    TestBroker {
        container,
        host,
        port,
    }
}

/// Same as [`start_broker`], but binds the container's `1883` to a fixed,
/// pre-reserved host port instead of letting Docker assign a random one.
///
/// Confirmed by hand against this sandbox's Docker daemon: a container
/// published with a *dynamic* (`0`) host port gets a **different** host port
/// after `stop()` + `start()` (same container, not recreated) — but a
/// container published with an explicit fixed host port keeps that exact
/// port across the same restart. Only `agent_resumes_publishing_after_
/// broker_restart` needs this (it restarts the container mid-test and
/// depends on the address staying reachable); `start_broker`'s dynamic port
/// is still the right default everywhere else, since it avoids this
/// function's port-reservation race entirely.
#[allow(dead_code)]
pub async fn start_broker_with_fixed_port() -> TestBroker {
    // Reserve a free port by asking the OS for one, then release it — a
    // small TOCTOU race (something else could grab the port before the
    // container binds it), but the standard, widely-used pattern for
    // fixed-port test infra, and short-lived enough here to be a
    // non-issue in practice.
    let host_port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("reserve a free port")
        .local_addr()
        .expect("local addr")
        .port();

    let container = GenericImage::new("eclipse-mosquitto", "2")
        .with_wait_for(WaitFor::message_on_stderr("running"))
        .with_mapped_port(host_port, 1883.tcp())
        .start()
        .await
        .expect("failed to start mosquitto testcontainer");
    let host = container
        .get_host()
        .await
        .expect("container host")
        .to_string();
    let port = container
        .get_host_port_ipv4(1883)
        .await
        .expect("container port");
    assert_eq!(
        port, host_port,
        "the container should be bound to the exact host port we reserved"
    );
    TestBroker {
        container,
        host,
        port,
    }
}

/// Builds an `AgentConfig` pointed at `broker`, with the given host id and
/// all collector intervals overridden to `interval_secs` so tests don't
/// have to wait out `disk`'s real 60s default.
pub fn test_agent_config(host_id: &str, broker: &TestBroker, interval_secs: u64) -> AgentConfig {
    AgentConfig {
        host_id: host_id.to_string(),
        broker_host: broker.host.clone(),
        broker_port: broker.port,
        collector_intervals: std::collections::BTreeMap::from([
            ("cpu".to_string(), interval_secs),
            ("memory".to_string(), interval_secs),
            ("disk".to_string(), interval_secs),
        ]),
        disabled_collectors: Vec::new(),
        mqtt_password: None,
    }
}

/// Builds a `TaskManagerService` with its `AppContext` already attached, the
/// same way `main.rs` does via `TaskRuntimeModule` — reused directly here
/// since these tests spawn real `Task`s (`CollectorTask`, `ConfigReload`)
/// that need a working manager, not a fake one.
pub fn test_task_manager() -> TaskManagerService {
    let app = AppBuilder::new()
        .build()
        .expect("empty app context")
        .context;
    let manager = TaskManagerService::new(
        tokio::runtime::Handle::current(),
        TaskManagerConfig { max_concurrent: 16 },
    );
    manager.set_context(app);
    manager
}

/// Polls `predicate` every `poll_interval` until it returns `true` or
/// `timeout` elapses, returning whether it succeeded. Used instead of a
/// fixed `sleep` so tests aren't flaky under slower CI/sandbox scheduling
/// while still failing promptly instead of hanging when something's
/// actually broken.
pub async fn wait_until(
    timeout: std::time::Duration,
    poll_interval: std::time::Duration,
    mut predicate: impl FnMut() -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(poll_interval).await;
    }
}
