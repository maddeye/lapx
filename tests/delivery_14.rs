use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::{Consequence, FinishCondition, FinishMode, RaceConfig, RaceStatus},
    http::router,
    runtime::{RaceRuntime, StateSnapshot},
    store::SqliteStore,
};
use std::time::Duration;
use tempfile::tempdir;
use tower::ServiceExt;

fn config() -> RaceConfig {
    RaceConfig {
        lanes: 2,
        start_sequence_ms: 10,
        restart_sequence_ms: 20,
        minimum_lap_time_ms: 100,
        finish_condition: FinishCondition::Laps(1),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::ResultTimePenaltyMs(50),
        chaos_consequence: Consequence::LanePowerOffMs(75),
    }
}

async fn post(app: axum::Router, path: &str, body: serde_json::Value) -> StateSnapshot {
    let response = app
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test(start_paused = true)]
async fn http_correction_updates_finished_race_and_reopened_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let app = router(RaceRuntime::new(SqliteStore::open(&path).unwrap(), "race").unwrap());
    let started = post(
        app.clone(),
        "/api/start",
        serde_json::json!({"config": config()}),
    )
    .await;
    assert_eq!(started.state.config(), Some(&config()));
    tokio::time::advance(Duration::from_millis(110)).await;
    let finished = post(
        app.clone(),
        "/api/sensor",
        serde_json::json!({"lane": 1, "edge": "rising"}),
    )
    .await;
    assert!(matches!(finished.state.status, RaceStatus::Finished(_)));

    let corrected = post(
        app,
        "/api/correct-laps",
        serde_json::json!({"lane": 2, "delta_thousandths": 500}),
    )
    .await;
    assert_eq!(
        corrected.state.lane(2).unwrap().corrected_laps_thousandths,
        500
    );
    let reopened = SqliteStore::open(&path).unwrap().load("race").unwrap();
    assert_eq!(reopened, corrected);
}

#[tokio::test]
async fn debug_page_exposes_every_race_config_value() {
    let dir = tempdir().unwrap();
    let response = router(
        RaceRuntime::new(
            SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
            "race",
        )
        .unwrap(),
    )
    .oneshot(Request::get("/debug").body(Body::empty()).unwrap())
    .await
    .unwrap();
    let page = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    for field in [
        "lanes",
        "start_sequence_ms",
        "restart_sequence_ms",
        "minimum_lap_time_ms",
        "finish_condition",
        "finish_mode",
        "false_start_consequence",
        "chaos_consequence",
        "delta_thousandths",
    ] {
        assert!(page.contains(field), "missing {field}");
    }
    for value in [
        "laps",
        "time_ms",
        "immediate",
        "leader_lap",
        "all_current_lap",
        "abort",
        "result_time_penalty_ms",
        "lane_power_off_ms",
    ] {
        assert!(page.contains(value), "missing {value}");
    }
}
