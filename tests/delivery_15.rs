use lapx::{
    domain::{Command, Consequence, Event, FinishCondition, FinishMode, RaceConfig, SignalEdge},
    hardware::{
        HardwareConfig, InputConfig, LaneHardwareConfig, PullMode, RawEdge, RelayConfig,
        SimulationPowerOutput, SimulationTimingSource,
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
            captured_at: Instant::now(),
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
    let mut updates = runtime.subscribe();

    tokio::time::advance(Duration::from_millis(99)).await;
    let lock = Connection::open(dir.path().join("ordered.db")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: Instant::now(),
        })
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(2)).await;
    lock.execute_batch("COMMIT").unwrap();

    updates.recv().await.unwrap();
    updates.recv().await.unwrap();
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
