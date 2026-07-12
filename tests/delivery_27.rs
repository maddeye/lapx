use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use lapx::{
    domain::{
        Command, Consequence, Event, FinishCondition, FinishMode, RaceConfig, RaceStatus,
        SignalEdge,
    },
    hardware::{
        CapturedAt, HardwareConfig, HardwareError, InputConfig, LaneHardwareConfig, PowerOutput,
        PullMode, RawEdge, RelayConfig, SimulationTimingSource,
    },
    http::{local_router, public_router},
    runtime::{RaceRuntime, StateSnapshot},
    store::{SqliteStore, StoreError},
};
use rusqlite::Connection;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::tempdir;
use tower::ServiceExt;

fn config() -> RaceConfig {
    RaceConfig {
        lanes: 1,
        driver_ids: vec![None],
        start_sequence_ms: 1,
        restart_sequence_ms: 1,
        minimum_lap_time_ms: 1,
        finish_condition: FinishCondition::Laps(1),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}

fn hardware_config() -> HardwareConfig {
    HardwareConfig::new(vec![LaneHardwareConfig {
        lane: 1,
        input: InputConfig {
            bcm_pin: 17,
            active_edge: SignalEdge::Rising,
            pull: PullMode::Off,
        },
        relay: RelayConfig {
            bcm_pin: 22,
            active_high: true,
        },
    }])
    .unwrap()
}

async fn finish(runtime: &RaceRuntime) -> StateSnapshot {
    runtime
        .apply(Command::StartRace {
            config: config(),
            at: 0,
        })
        .await
        .unwrap();
    runtime.apply(Command::AdvanceRace { to: 1 }).await.unwrap();
    runtime
        .apply(Command::SensorTriggered {
            lane: 1,
            edge: SignalEdge::Rising,
            at: 2,
        })
        .await
        .unwrap()
}

async fn post(
    app: axum::Router,
    path: &str,
    body: &'static str,
    content_type: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::post(path);
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    app.oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn current_race_switch_is_atomic_fresh_and_survives_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("switch.db");
    let store = SqliteStore::open(&path).unwrap();
    let runtime = RaceRuntime::new(store.clone(), "fallback").await.unwrap();
    assert_eq!(runtime.snapshot().await.unwrap().race_id, "fallback");

    let premature = runtime.next_race("fallback", "next").await.unwrap_err();
    assert!(matches!(
        premature,
        lapx::runtime::RuntimeError::Store(StoreError::RaceNotTerminal(_))
    ));
    let terminal = finish(&runtime).await;
    assert!(matches!(terminal.state.status, RaceStatus::Finished(_)));

    let conflict = runtime.next_race("stale", "next").await.unwrap_err();
    assert!(matches!(
        conflict,
        lapx::runtime::RuntimeError::Store(StoreError::CurrentRaceConflict { .. })
    ));
    assert!(matches!(
        runtime.next_race("fallback", "  ").await.unwrap_err(),
        lapx::runtime::RuntimeError::Store(StoreError::InvalidRaceId)
    ));

    let next = runtime.next_race("fallback", "next").await.unwrap();
    assert_eq!((next.race_id.as_str(), next.sequence), ("next", 0));
    assert!(matches!(next.state.status, RaceStatus::Ready));
    assert!(store.events("next").unwrap().is_empty());
    assert!(matches!(
        store.events("fallback").unwrap().last(),
        Some(Event::RaceFinished { .. })
    ));
    assert_eq!(store.completed_races().unwrap()[0].race_id, "fallback");

    drop(runtime);
    let restarted = RaceRuntime::new(SqliteStore::open(path).unwrap(), "ignored")
        .await
        .unwrap();
    assert_eq!(restarted.snapshot().await.unwrap(), next);
}

#[tokio::test]
async fn aborted_race_can_switch_without_a_reset_event() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("aborted.db")).unwrap();
    let runtime = RaceRuntime::new(store.clone(), "aborted").await.unwrap();
    runtime
        .apply(Command::StartRace {
            config: config(),
            at: 0,
        })
        .await
        .unwrap();
    runtime.apply(Command::AdvanceRace { to: 1 }).await.unwrap();
    let aborted = runtime
        .apply(Command::TriggerChaos {
            source: lapx::domain::ChaosSource::Lane(1),
            at: 2,
        })
        .await
        .unwrap();
    assert!(matches!(aborted.state.status, RaceStatus::Aborted));
    assert_eq!(
        runtime.next_race("aborted", "next").await.unwrap().sequence,
        0
    );
    assert!(store.events("next").unwrap().is_empty());
}

#[tokio::test]
async fn switch_rejects_a_previously_used_race_id() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("fresh.db")).unwrap();
    let runtime = RaceRuntime::new(store.clone(), "current").await.unwrap();
    finish(&runtime).await;
    store
        .execute(
            "used",
            Command::StartRace {
                config: config(),
                at: 0,
            },
        )
        .unwrap();
    assert!(matches!(
        runtime.next_race("current", "used").await,
        Err(lapx::runtime::RuntimeError::Store(StoreError::RaceAlreadyExists(id))) if id == "used"
    ));
    assert_eq!(runtime.snapshot().await.unwrap().race_id, "current");
}

#[tokio::test]
async fn race_aware_publication_http_and_sse_accept_only_the_switch_boundary() {
    let dir = tempdir().unwrap();
    let runtime = RaceRuntime::new(
        SqliteStore::open(dir.path().join("arbiter.db")).unwrap(),
        "old",
    )
    .await
    .unwrap();
    let terminal = finish(&runtime).await;
    let mut updates = runtime.subscribe();
    let app = local_router(runtime.clone());
    let public = public_router(runtime.clone());
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/state/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut stream = response.into_body();
    let initial = stream.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(String::from_utf8_lossy(&initial).contains("\"race_id\":\"old\""));

    let switched = runtime.next_race("old", "new").await.unwrap();
    assert!(switched.follows(&terminal));
    assert!(!terminal.follows(&switched));
    assert_eq!(updates.recv().await.unwrap(), switched);
    let event = stream.frame().await.unwrap().unwrap().into_data().unwrap();
    let event = String::from_utf8_lossy(&event);
    assert!(event.contains("id: 0"));
    assert!(event.contains("\"race_id\":\"new\""));

    let state = public
        .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let snapshot: StateSnapshot =
        serde_json::from_slice(&to_bytes(state.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!((snapshot.race_id.as_str(), snapshot.sequence), ("new", 0));
}

#[tokio::test]
async fn next_race_is_local_only_and_pause_resume_require_an_empty_json_object() {
    let dir = tempdir().unwrap();
    let runtime = RaceRuntime::new(
        SqliteStore::open(dir.path().join("csrf.db")).unwrap(),
        "race",
    )
    .await
    .unwrap();
    let local = local_router(runtime.clone());
    let public = public_router(runtime);

    for path in ["/api/pause", "/api/resume"] {
        for (body, content_type) in [
            ("", None),
            ("null", Some("application/json")),
            ("", Some("application/x-www-form-urlencoded")),
            ("x=1", Some("application/x-www-form-urlencoded")),
        ] {
            assert_eq!(
                post(local.clone(), path, body, content_type).await.status(),
                StatusCode::BAD_REQUEST
            );
        }
    }
    assert_ne!(
        post(public, "/api/next-race", "{}", Some("application/json"))
            .await
            .status(),
        StatusCode::OK
    );
}

#[derive(Clone, Default)]
struct RecordingPower(Arc<Mutex<Vec<[bool; 4]>>>);

impl PowerOutput for RecordingPower {
    fn set_lane_power(&mut self, lanes: [bool; 4]) -> Result<(), HardwareError> {
        self.0.lock().unwrap().push(lanes);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_hardware_edges_retry_indefinitely_once_in_order_with_original_timestamps() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("edge-retry.db");
    let store = SqliteStore::open(&path).unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        timing.clone(),
        RecordingPower::default(),
    )
    .await
    .unwrap();
    let mut long_race = config();
    long_race.finish_condition = FinishCondition::Laps(100);
    runtime
        .apply(Command::StartRace {
            config: long_race,
            at: 0,
        })
        .await
        .unwrap();
    runtime.apply(Command::AdvanceRace { to: 1 }).await.unwrap();

    let lock = Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let first_at = runtime.protocol_now().unwrap();
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: CapturedAt::Simulation(tokio::time::Instant::now()),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let second_at = runtime.protocol_now().unwrap();
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: CapturedAt::Simulation(tokio::time::Instant::now()),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(180)).await;
    lock.execute_batch("COMMIT").unwrap();

    tokio::time::timeout(Duration::from_secs(2), runtime.snapshot())
        .await
        .unwrap()
        .unwrap();
    let captured: Vec<_> = store
        .events("race")
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            Event::MeasurementCaptured { at, .. } => Some(at),
            _ => None,
        })
        .collect();
    assert_eq!(captured, vec![first_at, second_at]);
    assert_eq!(
        runtime.hardware_snapshot().unwrap().latest_edges[0]
            .as_ref()
            .unwrap()
            .protocol_at,
        Some(second_at)
    );
}

#[derive(Clone, Default)]
struct FailOncePower {
    calls: Arc<Mutex<Vec<[bool; 4]>>>,
    fail: Arc<AtomicBool>,
}

impl PowerOutput for FailOncePower {
    fn set_lane_power(&mut self, lanes: [bool; 4]) -> Result<(), HardwareError> {
        self.calls.lock().unwrap().push(lanes);
        if lanes.iter().any(|level| *level) && self.fail.swap(false, Ordering::SeqCst) {
            Err(HardwareError::new("post-commit relay failure"))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn captured_edge_is_not_retried_after_a_possible_post_commit_power_error() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("edge-power.db")).unwrap();
    let timing = SimulationTimingSource::default();
    let power = FailOncePower::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        timing.clone(),
        power.clone(),
    )
    .await
    .unwrap();
    let mut long_race = config();
    long_race.finish_condition = FinishCondition::Laps(100);
    runtime
        .apply(Command::StartRace {
            config: long_race,
            at: 0,
        })
        .await
        .unwrap();
    runtime.apply(Command::AdvanceRace { to: 1 }).await.unwrap();
    power.fail.store(true, Ordering::SeqCst);

    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: CapturedAt::Simulation(tokio::time::Instant::now()),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let captured = || {
        store
            .events("race")
            .unwrap()
            .into_iter()
            .filter(|event| matches!(event, Event::MeasurementCaptured { .. }))
            .count()
    };
    assert_eq!(captured(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(captured(), 1);
    assert!(
        runtime
            .hardware_snapshot()
            .unwrap()
            .last_error
            .unwrap()
            .contains("post-commit relay failure")
    );
}

#[tokio::test]
async fn hardware_stays_off_during_switch_and_selector_recovery() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("hardware-switch.db");
    let store = SqliteStore::open(&path).unwrap();
    let power = RecordingPower::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "old",
        hardware_config(),
        SimulationTimingSource::default(),
        power.clone(),
    )
    .await
    .unwrap();
    runtime
        .apply(Command::StartRace {
            config: config(),
            at: 0,
        })
        .await
        .unwrap();
    runtime.apply(Command::AdvanceRace { to: 1 }).await.unwrap();
    assert_eq!(
        power.0.lock().unwrap().last(),
        Some(&[true, false, false, false])
    );
    assert!(runtime.next_race("old", "too-soon").await.is_err());
    assert_eq!(
        power.0.lock().unwrap().last(),
        Some(&[true, false, false, false])
    );
    runtime
        .apply(Command::SensorTriggered {
            lane: 1,
            edge: SignalEdge::Rising,
            at: 2,
        })
        .await
        .unwrap();
    let before_switch = power.0.lock().unwrap().len();
    runtime.next_race("old", "new").await.unwrap();
    assert!(
        power.0.lock().unwrap()[before_switch..]
            .iter()
            .all(|outputs| *outputs == [false; 4])
    );

    runtime
        .apply(Command::StartRace {
            config: config(),
            at: 3,
        })
        .await
        .unwrap();
    runtime.apply(Command::AdvanceRace { to: 4 }).await.unwrap();
    drop(runtime);

    let recovered_power = RecordingPower::default();
    let recovered = RaceRuntime::with_hardware(
        SqliteStore::open(path).unwrap(),
        "ignored",
        hardware_config(),
        SimulationTimingSource::default(),
        recovered_power.clone(),
    )
    .await
    .unwrap();
    assert_eq!(recovered.snapshot().await.unwrap().race_id, "new");
    assert!(
        recovered_power
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|outputs| *outputs == [false; 4])
    );
}
