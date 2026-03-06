//! Router construction and server startup.

use axum::routing::get;
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::config::ApiConfig;
use crate::handlers;
use crate::state::AppState;
use crate::ws;

/// Builds the axum router with all routes and middleware.
pub fn create_router(state: AppState) -> Router {
    let ws_state = ws::WsState {
        event_bus: state.event_bus.clone(),
    };

    let api_routes = Router::new()
        .route("/status", get(handlers::system_status))
        .route("/pnl", get(handlers::pnl_summary))
        .route("/pnl/daily", get(handlers::pnl_daily))
        .route("/pnl/tokens", get(handlers::pnl_tokens))
        .route("/fills", get(handlers::list_fills))
        .route("/fills/{id}", get(handlers::get_fill))
        .route("/risk", get(handlers::risk_status));

    Router::new()
        .route("/health", get(handlers::health_report))
        .route("/health/live", get(handlers::health_live))
        .route("/health/ready", get(handlers::health_ready))
        .route("/ws", get(ws::ws_handler).with_state(ws_state.clone()))
        .nest("/api/v1", api_routes)
        .layer(TraceLayer::new_for_http())
        .layer(build_cors_layer(&ApiConfig::default()))
        .with_state(state)
}

/// Builds the router with a custom configuration for CORS.
pub fn create_router_with_config(state: AppState, config: &ApiConfig) -> Router {
    let ws_state = ws::WsState {
        event_bus: state.event_bus.clone(),
    };

    let api_routes = Router::new()
        .route("/status", get(handlers::system_status))
        .route("/pnl", get(handlers::pnl_summary))
        .route("/pnl/daily", get(handlers::pnl_daily))
        .route("/pnl/tokens", get(handlers::pnl_tokens))
        .route("/fills", get(handlers::list_fills))
        .route("/fills/{id}", get(handlers::get_fill))
        .route("/risk", get(handlers::risk_status));

    Router::new()
        .route("/health", get(handlers::health_report))
        .route("/health/live", get(handlers::health_live))
        .route("/health/ready", get(handlers::health_ready))
        .route("/ws", get(ws::ws_handler).with_state(ws_state.clone()))
        .nest("/api/v1", api_routes)
        .layer(TraceLayer::new_for_http())
        .layer(build_cors_layer(config))
        .with_state(state)
}

/// Starts the API server, binding to the configured address.
///
/// This function runs until the server is shut down or an error occurs.
pub async fn start_server(config: &ApiConfig, state: AppState) -> anyhow::Result<()> {
    if !config.enabled {
        info!("api server disabled, skipping startup");
        return Ok(());
    }

    let router = create_router_with_config(state, config);
    let bind_addr = config.bind_addr();

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!(addr = %bind_addr, "api server listening");

    axum::serve(listener, router).await?;

    Ok(())
}

/// Builds a CORS layer from configuration.
fn build_cors_layer(config: &ApiConfig) -> CorsLayer {
    if config.cors_origins.is_empty() {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        let origins: Vec<_> = config
            .cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState::with_defaults()
    }

    #[tokio::test]
    async fn health_live_returns_200() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_report_returns_200() {
        let state = test_state();
        state.health.register("test", false);

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_ready_returns_200() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn system_status_returns_200() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn pnl_summary_returns_200() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/pnl")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn pnl_daily_returns_200() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/pnl/daily")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn pnl_tokens_returns_200() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/pnl/tokens")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_fills_returns_200() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/fills")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_fills_with_limit() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/fills?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_fill_not_found() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/fills/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn risk_status_returns_200() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/risk")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cors_headers_present() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .header("origin", "http://localhost:3000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response
            .headers()
            .contains_key("access-control-allow-origin"));
    }

    #[tokio::test]
    async fn response_body_contains_success() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["data"]["alive"], true);
    }

    #[tokio::test]
    async fn error_body_format() {
        let app = create_router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/fills/missing-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], false);
        assert!(json["error"].as_str().unwrap().contains("missing-id"));
    }

    #[test]
    fn build_cors_empty_origins() {
        let config = ApiConfig::default();
        let _ = build_cors_layer(&config); // Should not panic.
    }

    #[test]
    fn build_cors_specific_origins() {
        let config = ApiConfig {
            cors_origins: vec!["http://localhost:3000".to_string()],
            ..Default::default()
        };
        let _ = build_cors_layer(&config); // Should not panic.
    }

    #[tokio::test]
    async fn server_disabled() {
        let config = ApiConfig {
            enabled: false,
            ..Default::default()
        };
        let result = start_server(&config, test_state()).await;
        assert!(result.is_ok());
    }
}
