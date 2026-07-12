use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::{Consequence, FinishCondition, FinishMode, RaceConfig, RaceStatus},
    http::{local_router, public_router},
    runtime::RaceRuntime,
    store::SqliteStore,
};
use std::time::Duration;
use tempfile::tempdir;
use tower::ServiceExt;

fn config() -> RaceConfig {
    RaceConfig {
        lanes: 2,
        driver_ids: vec![None; 2],
        start_sequence_ms: 50,
        restart_sequence_ms: 50,
        minimum_lap_time_ms: 100,
        finish_condition: FinishCondition::Laps(2),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::ResultTimePenaltyMs(1000),
    }
}

async fn post(app: &Router, path: &str, body: serde_json::Value) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "POST {path}: {}",
        String::from_utf8_lossy(&bytes)
    );
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // Every command response carries the full HttpState display schema.
    assert!(json["protocol_now"].is_u64(), "POST {path} protocol_now");
    assert!(
        json["race_clock_running"].is_boolean(),
        "POST {path} race_clock_running"
    );
    assert!(
        json.as_object().unwrap().contains_key("race_elapsed_ms"),
        "POST {path} race_elapsed_ms"
    );
    json
}

#[tokio::test]
async fn control_page_loads_locally_and_is_missing_publicly() {
    let dir = tempdir().unwrap();
    let runtime = RaceRuntime::new(
        SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
        "race",
    )
    .await
    .unwrap();
    let local = local_router(runtime.clone())
        .oneshot(Request::get("/control").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(local.status(), StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(local.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("lang=\"de\""));

    let public = public_router(runtime)
        .oneshot(Request::get("/control").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(public.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn complete_race_walkthrough_over_local_api() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let runtime = RaceRuntime::new(SqliteStore::open(&path).unwrap(), "race")
        .await
        .unwrap();
    let app = local_router(runtime);

    post(
        &app,
        "/api/start",
        serde_json::json!({ "config": config() }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(80)).await; // past the Startsequenz

    let lap = serde_json::json!({ "lane": 1, "edge": "rising" });
    tokio::time::sleep(Duration::from_millis(120)).await;
    post(&app, "/api/sensor", lap.clone()).await;

    post(&app, "/api/pause", serde_json::json!({})).await;
    post(&app, "/api/resume", serde_json::json!({})).await;
    tokio::time::sleep(Duration::from_millis(80)).await; // past the Wiederanlaufsequenz

    post(
        &app,
        "/api/chaos",
        serde_json::json!({ "source": { "lane": 2 } }),
    )
    .await;
    post(&app, "/api/resume", serde_json::json!({})).await;
    tokio::time::sleep(Duration::from_millis(80)).await;

    tokio::time::sleep(Duration::from_millis(120)).await;
    let finished = post(&app, "/api/sensor", lap).await;
    assert_eq!(finished["state"]["status"], "finished");

    // Post-race fractional Rundenkorrektur through the same surface.
    let corrected = post(
        &app,
        "/api/correct-laps",
        serde_json::json!({ "lane": 2, "delta_thousandths": 500 }),
    )
    .await;
    let lane2 = corrected["state"]["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["lane"] == 2)
        .unwrap();
    assert_eq!(lane2["corrected_laps_thousandths"], 500);
    let sequence = corrected["sequence"].as_u64().unwrap();

    // Durable: a fresh load reproduces the corrected state.
    drop(app);
    let reopened = SqliteStore::open(&path).unwrap().load("race").unwrap();
    assert_eq!(reopened.sequence, sequence);
    assert!(matches!(reopened.state.status, RaceStatus::Finished(_)));
    assert_eq!(
        reopened.state.lane(2).unwrap().corrected_laps_thousandths,
        500
    );
}
