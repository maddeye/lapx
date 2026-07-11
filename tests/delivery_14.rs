use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::{
        Command, Consequence, FinishCondition, FinishMode, Lifecycle, RaceConfig, RaceStatus,
    },
    http::router,
    runtime::{RaceRuntime, StateSnapshot},
    store::SqliteStore,
};
use rusqlite::Connection;
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
    let app = router(
        RaceRuntime::new(SqliteStore::open(&path).unwrap(), "race")
            .await
            .unwrap(),
    );
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
        .await
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
    assert!(page.contains("snapshot.sequence <= sequence"));
    assert!(page.contains("sequence = snapshot.sequence"));
}

#[tokio::test(start_paused = true)]
async fn external_store_commit_is_published_and_started_automatically() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let store = SqliteStore::open(&path).unwrap();
    let runtime = RaceRuntime::new(store.clone(), "race").await.unwrap();
    let mut updates = runtime.subscribe();
    tokio::task::yield_now().await;
    runtime.snapshot().await.unwrap();

    let committed = tokio::task::spawn_blocking(move || {
        store.execute(
            "race",
            Command::StartRace {
                config: config(),
                at: 10_000,
            },
        )
    })
    .await
    .unwrap()
    .unwrap();

    tokio::time::advance(Duration::from_millis(100)).await;
    let external = updates.recv().await.unwrap();
    assert_eq!(external.sequence, committed.sequence);
    assert!(runtime.protocol_now().unwrap() >= 10_000);

    tokio::time::advance(Duration::from_millis(100)).await;
    loop {
        let snapshot = updates.recv().await.unwrap();
        if matches!(
            snapshot.state.status,
            RaceStatus::Active(lapx::domain::ActiveRace {
                lifecycle: Lifecycle::Running { .. },
                ..
            })
        ) {
            assert!(snapshot.sequence > committed.sequence);
            break;
        }
    }
}

#[tokio::test]
async fn immediate_command_uses_the_durable_protocol_head() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let store = SqliteStore::open(&path).unwrap();
    let runtime = RaceRuntime::new(store.clone(), "race").await.unwrap();

    tokio::task::spawn_blocking(move || {
        store.execute(
            "race",
            Command::StartRace {
                config: config(),
                at: 10_000,
            },
        )
    })
    .await
    .unwrap()
    .unwrap();

    let snapshot = runtime
        .apply_now(|to| Command::AdvanceRace { to })
        .await
        .expect("runtime timestamp raced behind an external commit");
    assert!(snapshot.state.last_event_at.unwrap() >= 10_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_timestamp_reanchors_an_advancing_clock() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let store = SqliteStore::open(&path).unwrap();
    let runtime = RaceRuntime::new(store.clone(), "race").await.unwrap();
    let mut updates = runtime.subscribe();

    tokio::task::spawn_blocking(move || {
        store.execute(
            "race",
            Command::StartRace {
                config: config(),
                at: 10_000,
            },
        )
    })
    .await
    .unwrap()
    .unwrap();

    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let snapshot = updates.recv().await.unwrap();
            if matches!(
                snapshot.state.status,
                RaceStatus::Active(lapx::domain::ActiveRace {
                    lifecycle: Lifecycle::Running { .. },
                    ..
                })
            ) {
                break;
            }
        }
    })
    .await
    .expect("clock did not advance after external timestamp re-anchor");
    assert!(runtime.protocol_now().unwrap() > 10_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn due_worker_recovers_after_sqlite_write_lock() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let runtime = RaceRuntime::new(SqliteStore::open(&path).unwrap(), "race")
        .await
        .unwrap();
    let mut updates = runtime.subscribe();
    runtime
        .apply(Command::StartRace {
            config: config(),
            at: 0,
        })
        .await
        .unwrap();
    updates.recv().await.unwrap();

    let lock = Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    lock.execute_batch("COMMIT").unwrap();

    let recovered = tokio::time::timeout(Duration::from_millis(750), async {
        loop {
            let snapshot = updates.recv().await.unwrap();
            if matches!(
                snapshot.state.status,
                RaceStatus::Active(lapx::domain::ActiveRace {
                    lifecycle: Lifecycle::Running { .. },
                    ..
                })
            ) {
                break snapshot;
            }
        }
    })
    .await
    .expect("due worker permanently exited after a transient SQLite lock");
    assert!(recovered.sequence > 1);
}
