//! `list_hosts`/`get_host_items` Tauri commands — thin wrappers around
//! directly-testable `_via` functions (same split `crates/server/src/api/
//! agent_config.rs` uses), so the HTTP-call construction can be unit
//! tested against a mocked server without needing a Tauri test harness.

use nest_error::NestResult;
use sparrow_core::storage::{HostRow, MetricHistoryRow};

use crate::state::AppState;

#[tauri::command]
pub async fn list_hosts(state: tauri::State<'_, AppState>) -> NestResult<Vec<HostRow>> {
    list_hosts_via(&state).await
}

#[tauri::command]
pub async fn get_host_items(
    state: tauri::State<'_, AppState>,
    host_id: String,
) -> NestResult<Vec<MetricHistoryRow>> {
    get_host_items_via(&state, &host_id).await
}

async fn list_hosts_via(state: &AppState) -> NestResult<Vec<HostRow>> {
    state.http().get_json(&state.url("/api/hosts")).await
}

async fn get_host_items_via(state: &AppState, host_id: &str) -> NestResult<Vec<MetricHistoryRow>> {
    state
        .http()
        .get_json(&state.url(&format!("/api/hosts/{host_id}/items")))
        .await
}

#[cfg(test)]
mod tests {
    use nest_http_client::{HttpClientConfig, HttpClientService};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn state_for(server: &MockServer) -> AppState {
        let http = HttpClientService::new(HttpClientConfig::default()).expect("http client");
        AppState::new(http, server.uri())
    }

    #[tokio::test]
    async fn list_hosts_via_calls_the_expected_endpoint_and_parses_the_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/hosts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "host_id": "host-1", "hostname": "web-01", "online": true, "last_seen_ms": 1700000000000_i64 }
            ])))
            .mount(&server)
            .await;

        let hosts = list_hosts_via(&state_for(&server))
            .await
            .expect("list_hosts_via should succeed");

        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].host_id, "host-1");
        assert!(hosts[0].online);
    }

    #[tokio::test]
    async fn get_host_items_via_calls_the_expected_endpoint_and_parses_the_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/hosts/host-1/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "collector": "cpu",
                    "key": "cpu.usage_percent",
                    "value": "42.0",
                    "value_type": "float",
                    "tags": {},
                    "ts": 1700000000000_i64
                }
            ])))
            .mount(&server)
            .await;

        let items = get_host_items_via(&state_for(&server), "host-1")
            .await
            .expect("get_host_items_via should succeed");

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "cpu.usage_percent");
    }
}
