use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::{Consequence, FinishCondition, FinishMode, RaceConfig},
    http::{local_router, public_router},
    runtime::RaceRuntime,
    store::SqliteStore,
};
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

fn config() -> RaceConfig {
    RaceConfig {
        lanes: 2,
        start_sequence_ms: 100,
        restart_sequence_ms: 50,
        minimum_lap_time_ms: 100,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}

async fn runtime() -> Arc<RaceRuntime> {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    // Keep the tempdir alive for the whole process; tests only need one db each.
    std::mem::forget(dir);
    RaceRuntime::new(SqliteStore::open(path).unwrap(), "race")
        .await
        .unwrap()
}

#[tokio::test]
async fn public_api_is_read_only() {
    let runtime = runtime().await;

    // Every mutating or local-only route must be structurally absent.
    let denied_posts = [
        "/api/start",
        "/api/sensor",
        "/api/pause",
        "/api/resume",
        "/api/chaos",
        "/api/correct-laps",
    ];
    for path in denied_posts {
        let response = public_router(runtime.clone())
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "public POST {path} must be 404"
        );
    }
    for path in ["/debug", "/hardware", "/api/hardware", "/control"] {
        let response = public_router(runtime.clone())
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "public GET {path} must be 404"
        );
    }

    // Public reads still work.
    let response = public_router(runtime.clone())
        .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn local_router_keeps_all_routes() {
    let runtime = runtime().await;
    let response = local_router(runtime.clone())
        .oneshot(
            Request::post("/api/start")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "config": config() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    for path in ["/api/state", "/debug", "/hardware"] {
        let response = local_router(runtime.clone())
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "local GET {path} must exist"
        );
    }
}

#[tokio::test]
async fn state_reports_race_elapsed_ms() {
    let runtime = runtime().await;
    let app = local_router(runtime.clone());
    let ready = state_json(&app).await;
    assert!(ready["race_elapsed_ms"].is_null());

    let response = local_router(runtime.clone())
        .oneshot(
            Request::post("/api/start")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "config": config() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let running = state_json(&app).await;
    let elapsed = running["race_elapsed_ms"].as_u64().unwrap();
    assert!(elapsed >= 100, "elapsed {elapsed} should have started");
}

async fn state_json(app: &axum::Router) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}
