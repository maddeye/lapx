use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::{Consequence, FinishCondition, FinishMode, Lifecycle, RaceConfig, RaceStatus},
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
        start_sequence_ms: 100,
        restart_sequence_ms: 50,
        minimum_lap_time_ms: 100,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}

#[tokio::test]
async fn http_state_returns_json() {
    let dir = tempdir().unwrap();
    let runtime = RaceRuntime::new(
        SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
        "race",
    )
    .await
    .unwrap();
    let response = router(runtime)
        .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot: StateSnapshot =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(snapshot.sequence, 0);
    assert!(matches!(snapshot.state.status, RaceStatus::Ready));
}

#[tokio::test(start_paused = true)]
async fn runtime_materializes_due_event() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let runtime = RaceRuntime::new(SqliteStore::open(&path).unwrap(), "race")
        .await
        .unwrap();
    let mut updates = runtime.subscribe();
    let app = router(runtime.clone());
    let response = app
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
    let started: StateSnapshot =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();

    tokio::time::advance(Duration::from_millis(100)).await;
    let due = loop {
        let snapshot = updates.recv().await.unwrap();
        if snapshot.sequence > started.sequence {
            break snapshot;
        }
    };
    assert!(matches!(
        due.state.status,
        RaceStatus::Active(lapx::domain::ActiveRace {
            lifecycle: Lifecycle::Running { .. },
            ..
        })
    ));
    let reopened = SqliteStore::open(&path).unwrap().load("race").unwrap();
    assert_eq!(reopened.sequence, due.sequence);
}

#[tokio::test(start_paused = true)]
async fn runtime_clock_does_not_advance_during_downtime() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let runtime = RaceRuntime::new(SqliteStore::open(&path).unwrap(), "race")
        .await
        .unwrap();
    let response = router(runtime.clone())
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
    drop(runtime);

    tokio::time::advance(Duration::from_secs(60)).await;
    let restarted = RaceRuntime::new(SqliteStore::open(&path).unwrap(), "race")
        .await
        .unwrap();
    let mut updates = restarted.subscribe();
    tokio::time::advance(Duration::from_millis(99)).await;
    tokio::task::yield_now().await;
    assert!(matches!(
        updates.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    tokio::time::advance(Duration::from_millis(1)).await;
    let due = updates.recv().await.unwrap();
    assert!(matches!(
        due.state.status,
        RaceStatus::Active(lapx::domain::ActiveRace {
            lifecycle: Lifecycle::Running { .. },
            ..
        })
    ));
}
