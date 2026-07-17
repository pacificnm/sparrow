//! Live Postgres integration test for `OfflineWatch`'s real `Task::run`
//! loop (not just `HostRegistry::mark_stale_offline` directly, which
//! already has its own unit test in `sparrow-core`'s `storage.rs`) — seeds
//! a stale host and a fresh one, runs the sweep, asserts only the stale one
//! flips.
//!
//! Requires Docker. Run with `cargo test -p sparrow-server --test offline_watch_live`.

mod support;

use std::time::Duration;

use nest_core::AppBuilder;
use nest_task::TaskManager;
use nest_task_runtime::{TaskManagerConfig, TaskManagerService};
use sparrow_core::storage::HostRegistry;
use sparrow_server::offline_watch::OfflineWatch;

use support::start_postgres_with_schema;

#[tokio::test(flavor = "multi_thread")]
async fn offline_watch_marks_only_the_stale_host() {
    let db = start_postgres_with_schema().await;
    let registry = HostRegistry::new(db.pool.clone());

    let stale_host = "offline-watch-stale";
    let fresh_host = "offline-watch-fresh";
    registry
        .upsert_on_register(stale_host, "stale-host")
        .await
        .expect("register stale host");
    registry
        .upsert_on_register(fresh_host, "fresh-host")
        .await
        .expect("register fresh host");

    // upsert_on_register always sets last_seen to NOW(); backdate only the
    // stale host directly, same test-only approach storage.rs's own
    // mark_stale_offline test uses.
    sqlx::query("UPDATE hosts SET last_seen = NOW() - INTERVAL '1 hour' WHERE host_id = $1")
        .bind(stale_host)
        .execute(&db.pool)
        .await
        .expect("backdate stale host's last_seen");

    let app = AppBuilder::new()
        .build()
        .expect("empty app context")
        .context;
    let manager = TaskManagerService::new(
        tokio::runtime::Handle::current(),
        TaskManagerConfig::default(),
    );
    manager.set_context(app);

    // Fast cadence/threshold for the test — run_on_interval fires its first
    // tick immediately, so the sweep runs as soon as the task starts, not
    // after waiting out a real interval. 1s "stale" threshold means the
    // backdated (1 hour old) host is stale and the just-registered one
    // (last_seen ~= now) is not.
    manager
        .spawn(OfflineWatch::new(
            registry.clone(),
            Duration::from_millis(200),
            1,
        ))
        .await
        .expect("offline_watch should spawn");

    let stale_offline = wait_for_online(&registry, stale_host, false).await;
    assert!(!stale_offline, "the stale host should be marked offline");

    let fresh_online = registry
        .list()
        .await
        .expect("list should succeed")
        .into_iter()
        .find(|host| host.host_id == fresh_host)
        .expect("fresh host should still be present")
        .online;
    assert!(fresh_online, "the fresh host should be left untouched");
}

/// Polls `registry.list()` until `host_id`'s `online` flag matches
/// `expected` or a 5s deadline passes, returning the last observed value.
async fn wait_for_online(registry: &HostRegistry, host_id: &str, expected: bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let hosts = registry.list().await.expect("list should succeed");
        if let Some(host) = hosts.iter().find(|h| h.host_id == host_id) {
            if host.online == expected || tokio::time::Instant::now() >= deadline {
                return host.online;
            }
        } else if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
