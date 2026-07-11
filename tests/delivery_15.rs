use lapx::{
    domain::{Command, Consequence, Event, FinishCondition, FinishMode, RaceConfig, SignalEdge},
    hardware::{
        CapturedAt, HardwareConfig, InputConfig, LaneHardwareConfig, PullMode, RawEdge,
        RelayConfig, SimulationPowerOutput, SimulationTimingSource,
    },
    runtime::RaceRuntime,
    store::SqliteStore,
};
use rusqlite::Connection;
use std::time::Duration;
use tempfile::tempdir;
use tokio::time::Instant;

fn race_config() -> RaceConfig {
    RaceConfig {
        lanes: 2,
        start_sequence_ms: 1,
        restart_sequence_ms: 1,
        minimum_lap_time_ms: 100,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}

#[cfg(feature = "gpio")]
fn monotonic_now() -> Duration {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timestamp` is valid writable storage for one `timespec`.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) },
        0
    );
    Duration::new(timestamp.tv_sec as u64, timestamp.tv_nsec as u32)
}

fn hardware_config() -> HardwareConfig {
    HardwareConfig::new(vec![
        LaneHardwareConfig {
            lane: 1,
            input: InputConfig {
                bcm_pin: 17,
                active_edge: SignalEdge::Rising,
                pull: PullMode::Up,
            },
            relay: RelayConfig {
                bcm_pin: 22,
                active_high: true,
            },
        },
        LaneHardwareConfig {
            lane: 2,
            input: InputConfig {
                bcm_pin: 27,
                active_edge: SignalEdge::Falling,
                pull: PullMode::Down,
            },
            relay: RelayConfig {
                bcm_pin: 23,
                active_high: false,
            },
        },
    ])
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn simulation_timing_source_triggers_lap() {
    let dir = tempdir().unwrap();
    let timing = SimulationTimingSource::default();
    let power = SimulationPowerOutput::default();
    let runtime = RaceRuntime::with_hardware(
        SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
        "race",
        hardware_config(),
        timing.clone(),
        power.clone(),
    )
    .await
    .unwrap();
    runtime
        .apply(Command::StartRace {
            config: race_config(),
            at: 0,
        })
        .await
        .unwrap();
    runtime.apply(Command::AdvanceRace { to: 1 }).await.unwrap();
    assert_eq!(power.lane_power(), [true, true, false, false]);
    assert_eq!(
        runtime.hardware_snapshot().unwrap().output_levels,
        [Some(true), Some(false), None, None]
    );

    tokio::time::advance(Duration::from_millis(100)).await;
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: CapturedAt::Simulation(Instant::now()),
        })
        .unwrap();

    let snapshot = loop {
        tokio::task::yield_now().await;
        let snapshot = runtime.snapshot().await.unwrap();
        if snapshot.state.lane(1).unwrap().laps == 1 {
            break snapshot;
        }
    };
    assert_eq!(snapshot.state.lane(1).unwrap().laps, 1);
}

#[cfg(feature = "gpio")]
#[tokio::test]
async fn kernel_monotonic_capture_is_calibrated_to_protocol_time() {
    let dir = tempdir().unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        SqliteStore::open(dir.path().join("monotonic.db")).unwrap(),
        "race",
        hardware_config(),
        timing.clone(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: CapturedAt::KernelMonotonic(monotonic_now()),
        })
        .unwrap();
    runtime.snapshot().await.unwrap();

    let captured = runtime.hardware_snapshot().unwrap().latest_edges[0]
        .as_ref()
        .unwrap()
        .protocol_at
        .unwrap();
    assert!(captured <= runtime.protocol_now().unwrap());
}

#[tokio::test(start_paused = true)]
async fn captured_edge_precedes_an_elapsed_due_timer_with_its_original_timestamp() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("ordered.db")).unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        timing.clone(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    let mut config = race_config();
    config.start_sequence_ms = 100;
    config.false_start_consequence = Consequence::ResultTimePenaltyMs(1);
    runtime
        .apply(Command::StartRace { config, at: 0 })
        .await
        .unwrap();
    let now = runtime.protocol_now().unwrap();
    tokio::time::advance(Duration::from_millis(99 - now)).await;
    let lock = Connection::open(dir.path().join("ordered.db")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: CapturedAt::Simulation(Instant::now()),
        })
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(2)).await;
    lock.execute_batch("COMMIT").unwrap();

    runtime.snapshot().await.unwrap();
    tokio::task::yield_now().await;
    runtime.snapshot().await.unwrap();
    runtime.snapshot().await.unwrap();
    let events = store.events("race").unwrap();
    let captured = events
        .iter()
        .position(|event| matches!(event, Event::MeasurementCaptured { at: 99, .. }))
        .expect("edge kept its capture timestamp");
    let due = events
        .iter()
        .position(|event| matches!(event, Event::OfficialStart { at: 100 }))
        .expect("due event materialized");
    assert!(captured < due);
}

#[tokio::test(start_paused = true)]
async fn older_edge_arriving_during_settle_commits_first() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("capture-order.db")).unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        timing.clone(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    let mut config = race_config();
    config.start_sequence_ms = 100;
    config.false_start_consequence = Consequence::ResultTimePenaltyMs(1);
    runtime
        .apply(Command::StartRace { config, at: 0 })
        .await
        .unwrap();

    let older_at = runtime.protocol_now().unwrap();
    let older = CapturedAt::Simulation(Instant::now());
    tokio::time::advance(Duration::from_millis(1)).await;
    let newer_at = runtime.protocol_now().unwrap();
    let newer = CapturedAt::Simulation(Instant::now());
    timing
        .emit(RawEdge {
            lane: 2,
            edge: SignalEdge::Falling,
            captured_at: newer,
        })
        .unwrap();
    tokio::task::yield_now().await;
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: older,
        })
        .unwrap();
    runtime.snapshot().await.unwrap();

    let measurements: Vec<_> = store
        .events("race")
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            Event::MeasurementCaptured { lane, at, .. } => Some((lane, at)),
            _ => None,
        })
        .collect();
    assert_eq!(measurements, vec![(1, older_at), (2, newer_at)]);
}

#[tokio::test(start_paused = true)]
async fn snapshot_does_not_split_capture_ordering() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("snapshot-order.db")).unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        timing.clone(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    let mut config = race_config();
    config.start_sequence_ms = 100;
    config.false_start_consequence = Consequence::ResultTimePenaltyMs(1);
    runtime
        .apply(Command::StartRace { config, at: 0 })
        .await
        .unwrap();

    let older_at = runtime.protocol_now().unwrap();
    let older = CapturedAt::Simulation(Instant::now());
    tokio::time::advance(Duration::from_millis(1)).await;
    let newer_at = runtime.protocol_now().unwrap();
    timing
        .emit(RawEdge {
            lane: 2,
            edge: SignalEdge::Falling,
            captured_at: CapturedAt::Simulation(Instant::now()),
        })
        .unwrap();
    let snapshot = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.snapshot().await }
    });
    tokio::task::yield_now().await;
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: older,
        })
        .unwrap();
    snapshot.await.unwrap().unwrap();

    let measurements: Vec<_> = store
        .events("race")
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            Event::MeasurementCaptured { lane, at, .. } => Some((lane, at)),
            _ => None,
        })
        .collect();
    assert_eq!(measurements, vec![(1, older_at), (2, newer_at)]);
}

#[tokio::test(start_paused = true)]
async fn snapshot_first_still_waits_for_captured_edges() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("snapshot-first.db")).unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        timing.clone(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    let mut config = race_config();
    config.start_sequence_ms = 100;
    config.false_start_consequence = Consequence::ResultTimePenaltyMs(1);
    runtime
        .apply(Command::StartRace { config, at: 0 })
        .await
        .unwrap();

    let at = runtime.protocol_now().unwrap();
    let snapshot = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.snapshot().await }
    });
    tokio::task::yield_now().await;
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: CapturedAt::Simulation(Instant::now()),
        })
        .unwrap();
    let snapshot = snapshot.await.unwrap().unwrap();

    assert!(snapshot.sequence > 2);
    assert!(store.events("race").unwrap().iter().any(
        |event| matches!(event, Event::MeasurementCaptured { lane: 1, at: event_at, .. } if *event_at == at)
    ));
}

#[tokio::test(start_paused = true)]
async fn now_pause_is_ordered_between_edges_in_the_same_settle_batch() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("command-order.db")).unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        timing.clone(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    runtime
        .apply(Command::StartRace {
            config: race_config(),
            at: 0,
        })
        .await
        .unwrap();
    runtime.apply(Command::AdvanceRace { to: 1 }).await.unwrap();

    let older_at = runtime.protocol_now().unwrap();
    let older = CapturedAt::Simulation(Instant::now());
    tokio::time::advance(Duration::from_millis(1)).await;
    let pause_at = runtime.protocol_now().unwrap();
    let pause = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.apply_now(|at| Command::PauseRace { at }).await }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    let newer_at = runtime.protocol_now().unwrap();
    timing
        .emit(RawEdge {
            lane: 2,
            edge: SignalEdge::Falling,
            captured_at: CapturedAt::Simulation(Instant::now()),
        })
        .unwrap();
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: older,
        })
        .unwrap();
    pause.await.unwrap().unwrap();
    runtime.snapshot().await.unwrap();

    let events = store.events("race").unwrap();
    let older = events
        .iter()
        .position(|event| matches!(event, Event::MeasurementCaptured { lane: 1, at, .. } if *at == older_at))
        .unwrap();
    let pause = events
        .iter()
        .position(|event| matches!(event, Event::RacePaused { at } if *at == pause_at))
        .unwrap();
    let newer = events
        .iter()
        .position(|event| matches!(event, Event::MeasurementCaptured { lane: 2, at, .. } if *at == newer_at))
        .unwrap();
    assert!(older < pause && pause < newer);
}

#[tokio::test(start_paused = true)]
async fn edge_captured_just_before_due_is_drained_during_timer_settle() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("timer-settle.db")).unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        timing.clone(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    let mut config = race_config();
    config.start_sequence_ms = 100;
    config.false_start_consequence = Consequence::ResultTimePenaltyMs(1);
    runtime
        .apply(Command::StartRace { config, at: 0 })
        .await
        .unwrap();

    let now = runtime.protocol_now().unwrap();
    tokio::time::advance(Duration::from_millis(99 - now)).await;
    let captured = CapturedAt::Simulation(Instant::now());
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: captured,
        })
        .unwrap();
    runtime.snapshot().await.unwrap();
    runtime.snapshot().await.unwrap();

    let events = store.events("race").unwrap();
    let captured = events
        .iter()
        .position(|event| matches!(event, Event::MeasurementCaptured { at: 99, .. }))
        .unwrap();
    let due = events
        .iter()
        .position(|event| matches!(event, Event::OfficialStart { at: 100 }))
        .unwrap();
    assert!(captured < due);
}

#[tokio::test(start_paused = true)]
async fn normalized_edge_timestamp_survives_external_clock_reanchor() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("single-conversion.db")).unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        timing.clone(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    runtime
        .apply(Command::StartRace {
            config: race_config(),
            at: 0,
        })
        .await
        .unwrap();
    runtime.apply(Command::AdvanceRace { to: 1 }).await.unwrap();

    let expected = runtime.protocol_now().unwrap();
    let captured_at = CapturedAt::Simulation(Instant::now());
    store
        .execute("race", Command::PauseRace { at: 10_000 })
        .unwrap();
    let (_, emitted) = tokio::join!(
        biased;
        runtime.snapshot(),
        async {
            timing.emit(RawEdge {
                lane: 1,
                edge: SignalEdge::Falling,
                captured_at,
            })
        }
    );
    emitted.unwrap();
    runtime.snapshot().await.unwrap();

    assert_eq!(
        runtime.hardware_snapshot().unwrap().latest_edges[0]
            .as_ref()
            .unwrap()
            .protocol_at,
        Some(expected)
    );
}
