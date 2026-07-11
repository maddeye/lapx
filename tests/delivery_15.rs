use lapx::{
    domain::{Command, Consequence, FinishCondition, FinishMode, RaceConfig, SignalEdge},
    hardware::{
        HardwareConfig, InputConfig, LaneHardwareConfig, PullMode, RawEdge, RelayConfig,
        SimulationPowerOutput, SimulationTimingSource,
    },
    runtime::RaceRuntime,
    store::SqliteStore,
};
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

    tokio::time::advance(Duration::from_millis(100)).await;
    timing
        .emit(RawEdge {
            lane: 1,
            bcm_pin: 17,
            edge: SignalEdge::Rising,
            captured_at: Instant::now(),
            level: true,
            active: true,
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
