//! Shared helpers for `crates/server`'s live-broker/live-Postgres
//! integration tests.
//!
//! Each `tests/*.rs` file compiles as its own separate crate, so this can't
//! be `mod support;`-shared from `src/`. Mirrors the already-proven recipes
//! from `sparrow-agent/tests/support/mod.rs` (Mosquitto) and
//! `sparrow-core/src/storage.rs`'s own test module (Postgres + migrations)
//! rather than guessing at new ones.

use nest_data::DataModule;
use nest_data_postgres::{PostgresConfig, PostgresDataModule};
use sqlx::PgPool;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage};
use testcontainers_modules::postgres::Postgres as PostgresImage;
use testcontainers_modules::testcontainers::ContainerAsync as PostgresContainerAsync;

/// Holds a running Mosquitto container alive for the test's duration.
///
/// Used by `ingest_live.rs`, not `offline_watch_live.rs` — shared by
/// multiple `tests/*.rs` binaries, each using a different subset (same
/// reason `sparrow-agent/tests/support/mod.rs`'s `TestBroker` needs the
/// same allow).
#[allow(dead_code)]
pub struct TestBroker {
    pub container: ContainerAsync<GenericImage>,
    pub host: String,
    pub port: u16,
}

/// Starts a disposable Mosquitto broker and returns its host/port. The
/// stock `eclipse-mosquitto:2` image's default config already sets
/// `listener 1883` + `allow_anonymous true`.
#[allow(dead_code)]
pub async fn start_broker() -> TestBroker {
    let container = GenericImage::new("eclipse-mosquitto", "2")
        .with_exposed_port(1883.tcp())
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

/// Holds a running Postgres container (with Sparrow's migrations already
/// applied) alive for the test's duration.
pub struct TestDb {
    #[allow(dead_code)]
    pub container: PostgresContainerAsync<PostgresImage>,
    pub pool: PgPool,
}

/// Starts a disposable Postgres container, applies Sparrow's migrations,
/// and returns a fresh pool — same shape as
/// `sparrow-core/src/storage.rs`'s own `start_postgres_with_schema` test
/// helper (duplicated here, not imported: that one is private to
/// `sparrow-core`'s test module).
pub async fn start_postgres_with_schema() -> TestDb {
    let container = PostgresImage::default()
        .start()
        .await
        .expect("failed to start postgres testcontainer");
    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container port");
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

    nest_core::AppBuilder::new()
        .module(DataModule)
        .module(
            PostgresDataModule::new(PostgresConfig::new(url.clone()))
                .with_migrations(sparrow_core::migrations::all_migrations()),
        )
        .build()
        .expect("app with postgres migrations");

    let pool = PgPool::connect(&url).await.expect("fresh postgres pool");

    TestDb { container, pool }
}
