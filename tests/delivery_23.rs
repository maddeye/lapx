use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use lapx::{
    domain::*,
    http::{local_router, public_router},
    runtime::RaceRuntime,
    store::{DriverStats, SqliteStore, StoreError},
};
use tempfile::tempdir;
use tower::ServiceExt;

fn config(driver_ids: Vec<Option<i64>>, mode: FinishMode, laps: u32) -> RaceConfig {
    RaceConfig {
        lanes: driver_ids.len() as u8,
        driver_ids,
        start_sequence_ms: 10,
        restart_sequence_ms: 10,
        minimum_lap_time_ms: 1,
        finish_condition: FinishCondition::Laps(laps),
        finish_mode: mode,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}

fn start(store: &SqliteStore, race_id: &str, config: RaceConfig) {
    store
        .execute(race_id, Command::StartRace { config, at: 0 })
        .unwrap();
    store
        .execute(race_id, Command::AdvanceRace { to: 10 })
        .unwrap();
}

fn lap(store: &SqliteStore, race_id: &str, lane: u8, at: u64) {
    store
        .execute(
            race_id,
            Command::SensorTriggered {
                lane,
                at,
                edge: SignalEdge::Rising,
            },
        )
        .unwrap();
}

#[test]
fn driver_stats_from_completed_race() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    let ada = store.create_driver("Ada").unwrap();
    let grace = store.create_driver("Grace").unwrap();
    start(
        &store,
        "completed",
        config(vec![Some(ada.id), Some(grace.id)], FinishMode::Immediate, 2),
    );
    lap(&store, "completed", 1, 110);
    lap(&store, "completed", 2, 130);
    lap(&store, "completed", 1, 200);

    start(
        &store,
        "active",
        config(vec![Some(ada.id)], FinishMode::Immediate, 10),
    );

    let races = store.completed_races().unwrap();
    assert_eq!(races.len(), 1);
    assert_eq!(races[0].race_id, "completed");
    assert_eq!(races[0].results[0].driver_id, Some(ada.id));
    assert_eq!(races[0].results[0].best_lap_ms, Some(90));
    assert_eq!(
        store.driver_stats().unwrap(),
        vec![
            DriverStats {
                driver_id: ada.id,
                starts: 1,
                wins: 1,
                best_lap_ms: Some(90),
            },
            DriverStats {
                driver_id: grace.id,
                starts: 1,
                wins: 0,
                best_lap_ms: Some(120),
            },
        ]
    );
}

#[test]
fn correction_updates_driver_stats() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    let ada = store.create_driver("Ada").unwrap();
    let grace = store.create_driver("Grace").unwrap();
    start(
        &store,
        "race",
        config(vec![Some(ada.id), Some(grace.id)], FinishMode::Immediate, 1),
    );
    lap(&store, "race", 1, 110);
    assert_eq!(store.driver_stats().unwrap()[0].wins, 1);

    store
        .execute(
            "race",
            Command::CorrectLaps {
                lane: 2,
                delta_thousandths: 2_000,
                at: 111,
            },
        )
        .unwrap();

    let stats = store.driver_stats().unwrap();
    assert_eq!(stats[0].wins, 0);
    assert_eq!(stats[1].wins, 1);
    assert_eq!(
        store.completed_races().unwrap()[0].results[0].driver_id,
        Some(grace.id)
    );
}

#[test]
fn archived_assignment_rejection_is_atomic() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    let driver = store.create_driver("Ada").unwrap();
    store.archive_driver(driver.id).unwrap();

    assert!(matches!(
        store.execute(
            "race",
            Command::StartRace {
                config: config(vec![Some(driver.id)], FinishMode::Immediate, 1),
                at: 0,
            },
        ),
        Err(StoreError::DriverNotActive(id)) if id == driver.id
    ));
    assert!(store.events("race").unwrap().is_empty());
    assert!(matches!(
        store.execute(
            "missing",
            Command::StartRace {
                config: config(vec![Some(999)], FinishMode::Immediate, 1),
                at: 0,
            },
        ),
        Err(StoreError::DriverNotActive(999))
    ));
    assert!(store.events("missing").unwrap().is_empty());
}

#[test]
fn driver_assignment_shape_positive_and_unique_are_domain_rules() {
    for driver_ids in [vec![Some(1)], vec![Some(0), None], vec![Some(1), Some(1)]] {
        assert!(matches!(
            RaceEngine::new().handle(Command::StartRace {
                config: RaceConfig {
                    lanes: 2,
                    driver_ids,
                    start_sequence_ms: 1,
                    restart_sequence_ms: 1,
                    minimum_lap_time_ms: 1,
                    finish_condition: FinishCondition::Laps(1),
                    finish_mode: FinishMode::Immediate,
                    false_start_consequence: Consequence::Abort,
                    chaos_consequence: Consequence::Abort,
                },
                at: 0,
            }),
            Err(DomainError::InvalidDriverAssignments)
        ));
    }
}

#[test]
fn exact_ties_follow_lane_finished_append_order() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    let first = store.create_driver("First").unwrap();
    let second = store.create_driver("Second").unwrap();
    start(
        &store,
        "tie",
        config(
            vec![Some(first.id), Some(second.id)],
            FinishMode::AllCurrentLap,
            1,
        ),
    );
    lap(&store, "tie", 2, 110);
    lap(&store, "tie", 1, 110);

    let results = &store.completed_races().unwrap()[0].results;
    assert_eq!(results[0].lane, 2);
    assert_eq!(results[1].lane, 1);
    assert_eq!(
        results[0].corrected_laps_thousandths,
        results[1].corrected_laps_thousandths
    );
    assert_eq!(results[0].result_time_ms, results[1].result_time_ms);
    assert_eq!(store.driver_stats().unwrap()[1].wins, 1);
}

#[tokio::test]
async fn start_assignment_and_history_apis_are_local_only() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    let driver = store.create_driver("Ada").unwrap();
    let runtime = RaceRuntime::new(store.clone(), "race").await.unwrap();
    let response = local_router(runtime.clone())
        .oneshot(
            Request::post("/api/start")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "config": config(vec![Some(driver.id)], FinishMode::Immediate, 1) })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let events = store.events("race").unwrap();
    assert!(matches!(
        &events[0],
        Event::RaceConfigured { config, .. } if config.driver_ids == vec![Some(driver.id)]
    ));
    assert_eq!(
        serde_json::to_value(&events[0]).unwrap()["config"]["driver_ids"],
        serde_json::json!([driver.id])
    );

    for path in ["/api/race-history", "/api/driver-stats"] {
        assert_eq!(
            local_router(runtime.clone())
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            public_router(runtime.clone())
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
