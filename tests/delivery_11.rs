use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::{
        ActiveRace, Consequence, FinishCondition, FinishMode, RaceConfig, RaceControl, RaceStatus,
    },
    http::local_router as router,
    runtime::{RaceRuntime, StateSnapshot},
    store::SqliteStore,
};
use rusqlite::Connection;
use std::time::Duration;
use tempfile::tempdir;
use tower::ServiceExt;

fn config() -> RaceConfig {
    RaceConfig {
        lanes: 1,
        driver_ids: vec![None],
        start_sequence_ms: 1,
        restart_sequence_ms: 1,
        minimum_lap_time_ms: 100,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}

async fn post(app: axum::Router, path: &str, body: serde_json::Value) -> axum::response::Response {
    app.oneshot(
        Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn http_command_round_trip() {
    let dir = tempdir().unwrap();
    let app = router(
        RaceRuntime::new(
            SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
            "race",
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        post(
            app.clone(),
            "/api/start",
            serde_json::json!({"config": config()})
        )
        .await
        .status(),
        StatusCode::OK
    );
    tokio::time::advance(Duration::from_millis(101)).await;
    let response = post(
        app.clone(),
        "/api/sensor",
        serde_json::json!({"lane": 1, "edge": "rising"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot: StateSnapshot =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(snapshot.state.lane(1).unwrap().laps, 1);

    assert_eq!(
        post(app.clone(), "/api/pause", serde_json::json!({}))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        post(app.clone(), "/api/resume", serde_json::json!({}))
            .await
            .status(),
        StatusCode::OK
    );
    let chaos = post(
        app,
        "/api/chaos",
        serde_json::json!({"source": "race_control"}),
    )
    .await;
    let snapshot: StateSnapshot =
        serde_json::from_slice(&to_bytes(chaos.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(matches!(
        snapshot.state.status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Paused { .. },
            ..
        })
    ));
}

#[tokio::test]
async fn http_command_errors_have_explicit_statuses() {
    let dir = tempdir().unwrap();
    let app = router(
        RaceRuntime::new(
            SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
            "race",
        )
        .await
        .unwrap(),
    );
    let malformed = app
        .clone()
        .oneshot(
            Request::post("/api/start")
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    let mut invalid_config = config();
    invalid_config.lanes = 0;
    let invalid = post(
        app,
        "/api/start",
        serde_json::json!({"config": invalid_config}),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(
        !to_bytes(invalid.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn http_storage_error_statuses() {
    let corrupt_dir = tempdir().unwrap();
    let corrupt_path = corrupt_dir.path().join("lapx.db");
    let corrupt_app = router(
        RaceRuntime::new(SqliteStore::open(&corrupt_path).unwrap(), "race")
            .await
            .unwrap(),
    );
    Connection::open(&corrupt_path)
        .unwrap()
        .execute(
            "INSERT INTO race_events (race_id, sequence, event_type, schema_version, payload) VALUES ('race', 1, 'official_start', 1, '{\"type\":\"official_start\",\"at\":0}')",
            [],
        )
        .unwrap();
    let response = corrupt_app
        .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let busy_dir = tempdir().unwrap();
    let busy_path = busy_dir.path().join("lapx.db");
    let busy_app = router(
        RaceRuntime::new(SqliteStore::open(&busy_path).unwrap(), "race")
            .await
            .unwrap(),
    );
    let lock = Connection::open(&busy_path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let response = post(
        busy_app,
        "/api/start",
        serde_json::json!({"config": config()}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
