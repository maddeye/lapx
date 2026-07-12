use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use lapx::{
    domain::{Consequence, FinishCondition, FinishMode, RaceConfig},
    http::local_router as router,
    runtime::{RaceRuntime, StateSnapshot},
    store::SqliteStore,
};
use tempfile::tempdir;
use tower::ServiceExt;

fn config() -> RaceConfig {
    RaceConfig {
        lanes: 1,
        driver_ids: vec![None],
        start_sequence_ms: 1_000,
        restart_sequence_ms: 500,
        minimum_lap_time_ms: 100,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::ResultTimePenaltyMs(1),
        chaos_consequence: Consequence::Abort,
    }
}

fn max_id(chunk: &[u8]) -> Option<u64> {
    String::from_utf8_lossy(chunk)
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .filter_map(|id| id.parse().ok())
        .max()
}

async fn next_chunk(body: &mut Body) -> Vec<u8> {
    body.frame()
        .await
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap()
        .to_vec()
}

async fn start(app: axum::Router) -> StateSnapshot {
    let response = app
        .oneshot(
            Request::post("/api/start")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"config": config()}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn sse_emits_state_after_command() {
    let dir = tempdir().unwrap();
    let app = router(
        RaceRuntime::new(
            SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
            "race",
        )
        .await
        .unwrap(),
    );
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/state/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body();
    assert_eq!(max_id(&next_chunk(&mut body).await), Some(0));

    let committed = start(app).await;
    assert_eq!(
        max_id(&next_chunk(&mut body).await),
        Some(committed.sequence)
    );
}

#[tokio::test]
async fn sse_connection_race_never_loses_a_commit() {
    let dir = tempdir().unwrap();
    let app = router(
        RaceRuntime::new(
            SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
            "race",
        )
        .await
        .unwrap(),
    );
    let connect = app.clone().oneshot(
        Request::get("/api/state/stream")
            .body(Body::empty())
            .unwrap(),
    );
    let command = start(app);
    let (response, committed) = tokio::join!(connect, command);
    let mut body = response.unwrap().into_body();
    let first = max_id(&next_chunk(&mut body).await).unwrap();
    let observed = if first >= committed.sequence {
        first
    } else {
        max_id(&next_chunk(&mut body).await).unwrap()
    };
    assert!(observed >= committed.sequence);
}

#[tokio::test]
async fn sse_lag_reloads_the_full_current_state() {
    let dir = tempdir().unwrap();
    let app = router(
        RaceRuntime::new(
            SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
            "race",
        )
        .await
        .unwrap(),
    );
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/state/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body();
    start(app.clone()).await;
    let mut latest = 0;
    for _ in 0..20 {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/sensor")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"lane":1,"edge":"rising"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let snapshot: StateSnapshot =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        latest = snapshot.sequence;
    }

    assert_eq!(max_id(&next_chunk(&mut body).await), Some(0));
    assert_eq!(max_id(&next_chunk(&mut body).await), Some(latest));
}
