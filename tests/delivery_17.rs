use lapx::{
    domain::{
        ActiveRace, Command, Consequence, Event, FinishCondition, FinishMode, RaceConfig,
        RaceControl, RaceStatus, SignalEdge,
    },
    hardware::{
        HardwareConfig, HardwareError, InputConfig, LaneHardwareConfig, PowerOutput, PullMode,
        RawEdge, RelayConfig, SimulationPowerOutput, SimulationTimingSource,
    },
    runtime::{RaceRuntime, RuntimeError},
    store::SqliteStore,
};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::tempdir;

fn race_config(finish: FinishCondition, chaos: Consequence) -> RaceConfig {
    RaceConfig {
        lanes: 2,
        start_sequence_ms: 10,
        restart_sequence_ms: 10,
        minimum_lap_time_ms: 100,
        finish_condition: finish,
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: chaos,
    }
}

fn hardware_config() -> HardwareConfig {
    HardwareConfig::new(
        [(1, 17, 22), (2, 27, 23)]
            .into_iter()
            .map(|(lane, input, relay)| LaneHardwareConfig {
                lane,
                input: InputConfig {
                    bcm_pin: input,
                    active_edge: SignalEdge::Rising,
                    pull: PullMode::Off,
                },
                relay: RelayConfig {
                    bcm_pin: relay,
                    active_high: true,
                },
            })
            .collect(),
    )
    .unwrap()
}

async fn runtime_with_power(store: SqliteStore, power: SimulationPowerOutput) -> Arc<RaceRuntime> {
    RaceRuntime::with_hardware(
        store,
        "race",
        hardware_config(),
        SimulationTimingSource::default(),
        power,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn relay_power_follows_race_state_penalty_abort_and_finish() {
    let dir = tempdir().unwrap();
    let power = SimulationPowerOutput::default();
    let runtime = runtime_with_power(
        SqliteStore::open(dir.path().join("penalty.db")).unwrap(),
        power.clone(),
    )
    .await;
    runtime
        .apply(Command::StartRace {
            config: race_config(FinishCondition::Laps(10), Consequence::LanePowerOffMs(50)),
            at: 0,
        })
        .await
        .unwrap();
    assert_eq!(power.lane_power(), [false; 4]);
    runtime
        .apply(Command::AdvanceRace { to: 10 })
        .await
        .unwrap();
    assert_eq!(power.lane_power(), [true, true, false, false]);
    runtime.apply(Command::PauseRace { at: 20 }).await.unwrap();
    assert_eq!(power.lane_power(), [false; 4]);
    runtime.apply(Command::ResumeRace { at: 30 }).await.unwrap();
    runtime
        .apply(Command::AdvanceRace { to: 40 })
        .await
        .unwrap();
    assert_eq!(power.lane_power(), [true, true, false, false]);
    runtime
        .apply(Command::TriggerChaos {
            source: lapx::domain::ChaosSource::Lane(1),
            at: 50,
        })
        .await
        .unwrap();
    assert_eq!(power.lane_power(), [false; 4]);
    runtime.apply(Command::ResumeRace { at: 60 }).await.unwrap();
    runtime
        .apply(Command::AdvanceRace { to: 70 })
        .await
        .unwrap();
    assert_eq!(power.lane_power(), [false, true, false, false]);
    runtime
        .apply(Command::AdvanceRace { to: 100 })
        .await
        .unwrap();
    assert_eq!(power.lane_power(), [true, true, false, false]);

    let abort_power = SimulationPowerOutput::default();
    let abort = runtime_with_power(
        SqliteStore::open(dir.path().join("abort.db")).unwrap(),
        abort_power.clone(),
    )
    .await;
    abort
        .apply(Command::StartRace {
            config: race_config(FinishCondition::Laps(10), Consequence::Abort),
            at: 0,
        })
        .await
        .unwrap();
    abort.apply(Command::AdvanceRace { to: 10 }).await.unwrap();
    abort
        .apply(Command::TriggerChaos {
            source: lapx::domain::ChaosSource::Lane(1),
            at: 20,
        })
        .await
        .unwrap();
    assert_eq!(abort_power.lane_power(), [false; 4]);

    let finish_power = SimulationPowerOutput::default();
    let finish = runtime_with_power(
        SqliteStore::open(dir.path().join("finish.db")).unwrap(),
        finish_power.clone(),
    )
    .await;
    finish
        .apply(Command::StartRace {
            config: race_config(FinishCondition::Laps(1), Consequence::Abort),
            at: 0,
        })
        .await
        .unwrap();
    finish.apply(Command::AdvanceRace { to: 10 }).await.unwrap();
    finish
        .apply(Command::SensorTriggered {
            lane: 1,
            at: 110,
            edge: SignalEdge::Rising,
        })
        .await
        .unwrap();
    assert_eq!(finish_power.lane_power(), [false; 4]);
}

#[derive(Clone, Default)]
struct RecordingPower {
    calls: Arc<Mutex<Vec<[bool; 4]>>>,
    fail_power_on: Arc<AtomicBool>,
}

impl PowerOutput for RecordingPower {
    fn set_lane_power(&mut self, lanes: [bool; 4]) -> Result<(), HardwareError> {
        self.calls.lock().unwrap().push(lanes);
        if self.fail_power_on.load(Ordering::SeqCst) && lanes.iter().any(|on| *on) {
            Err(HardwareError::new("relay write failed"))
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct PowerGate {
    armed: bool,
    entered: bool,
    released: bool,
}

#[derive(Clone, Default)]
struct BlockingPower {
    levels: Arc<Mutex<[bool; 4]>>,
    gate: Arc<(Mutex<PowerGate>, Condvar)>,
}

impl PowerOutput for BlockingPower {
    fn set_lane_power(&mut self, lanes: [bool; 4]) -> Result<(), HardwareError> {
        let (lock, changed) = &*self.gate;
        let mut gate = lock.lock().unwrap();
        if gate.armed && lanes.iter().any(|level| *level) {
            gate.armed = false;
            gate.entered = true;
            changed.notify_all();
            while !gate.released {
                gate = changed.wait(gate).unwrap();
            }
        }
        *self.levels.lock().unwrap() = lanes;
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn power_failure_reports_committed_state_broadcasts_and_fails_safe_once() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let power = RecordingPower::default();
    power.fail_power_on.store(true, Ordering::SeqCst);
    let calls = power.calls.clone();
    let runtime = RaceRuntime::with_hardware(
        SqliteStore::open(&path).unwrap(),
        "race",
        hardware_config(),
        SimulationTimingSource::default(),
        power,
    )
    .await
    .unwrap();
    runtime
        .apply(Command::StartRace {
            config: race_config(FinishCondition::Laps(10), Consequence::Abort),
            at: 0,
        })
        .await
        .unwrap();
    let mut updates = runtime.subscribe();
    let error = runtime
        .apply(Command::AdvanceRace { to: 10 })
        .await
        .unwrap_err();
    let committed = match error {
        RuntimeError::PowerAfterCommit { snapshot, .. } => snapshot,
        other => panic!("unexpected error: {other}"),
    };
    let broadcast = updates.recv().await.unwrap();
    assert_eq!(broadcast, *committed);
    assert!(matches!(
        SqliteStore::open(&path)
            .unwrap()
            .load("race")
            .unwrap()
            .status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Paused { .. },
            ..
        })
    ));
    let monitor = runtime.hardware_snapshot().unwrap();
    assert_eq!(monitor.commanded_outputs, [false; 4]);
    assert!(monitor.last_error.unwrap().contains("relay write failed"));
    assert_eq!(calls.lock().unwrap().last(), Some(&[false; 4]));

    let call_count = calls.lock().unwrap().len();
    tokio::time::advance(Duration::from_millis(500)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        calls.lock().unwrap().len(),
        call_count,
        "must not blindly retry"
    );
}

#[tokio::test(start_paused = true)]
async fn power_fault_stays_off_through_refresh_edge_and_due_until_resume() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("latched.db")).unwrap();
    let timing = SimulationTimingSource::default();
    let power = RecordingPower::default();
    power.fail_power_on.store(true, Ordering::SeqCst);
    let calls = power.calls.clone();
    let runtime = RaceRuntime::with_hardware(
        store,
        "race",
        hardware_config(),
        timing.clone(),
        power.clone(),
    )
    .await
    .unwrap();
    runtime
        .apply(Command::StartRace {
            config: race_config(FinishCondition::Laps(10), Consequence::Abort),
            at: 0,
        })
        .await
        .unwrap();
    let error = runtime
        .apply(Command::AdvanceRace { to: 10 })
        .await
        .unwrap_err();
    assert!(matches!(error, RuntimeError::PowerAfterCommit { .. }));
    let failed_calls = calls.lock().unwrap().len();

    tokio::time::advance(Duration::from_millis(100)).await;
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: tokio::time::Instant::now(),
        })
        .unwrap();
    runtime
        .apply(Command::AdvanceRace { to: 200 })
        .await
        .unwrap();
    runtime.snapshot().await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), failed_calls);
    assert_eq!(
        runtime.hardware_snapshot().unwrap().commanded_outputs,
        [false; 4]
    );

    power.fail_power_on.store(false, Ordering::SeqCst);
    tokio::time::advance(Duration::from_millis(200)).await;
    runtime.snapshot().await.unwrap();
    assert_eq!(calls.lock().unwrap().len(), failed_calls);

    runtime
        .apply(Command::ResumeRace { at: 210 })
        .await
        .unwrap();
    assert_eq!(calls.lock().unwrap().last(), Some(&[false; 4]));
    runtime
        .apply(Command::AdvanceRace { to: 220 })
        .await
        .unwrap();
    assert_eq!(
        calls.lock().unwrap().last(),
        Some(&[true, true, false, false])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_external_pause_is_not_overwritten_by_a_stale_power_projection() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("guarded.db")).unwrap();
    let power = BlockingPower::default();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        SimulationTimingSource::default(),
        power.clone(),
    )
    .await
    .unwrap();
    runtime
        .apply(Command::StartRace {
            config: race_config(FinishCondition::Laps(10), Consequence::Abort),
            at: 0,
        })
        .await
        .unwrap();
    power.gate.0.lock().unwrap().armed = true;

    let advancing = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.apply(Command::AdvanceRace { to: 10 }).await })
    };
    tokio::task::spawn_blocking({
        let gate = power.gate.clone();
        move || {
            let (lock, changed) = &*gate;
            let mut state = lock.lock().unwrap();
            while !state.entered {
                state = changed.wait(state).unwrap();
            }
        }
    })
    .await
    .unwrap();

    let pausing = tokio::task::spawn_blocking({
        let store = store.clone();
        move || store.execute("race", Command::PauseRace { at: 20 })
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(
        !pausing.is_finished(),
        "external writer bypassed the guarded projection"
    );
    {
        let (lock, changed) = &*power.gate;
        let mut state = lock.lock().unwrap();
        state.released = true;
        changed.notify_all();
    }

    advancing.await.unwrap().unwrap();
    let paused = pausing.await.unwrap().unwrap();
    let projected = runtime.snapshot().await.unwrap();
    assert_eq!(projected, paused);
    assert_eq!(*power.levels.lock().unwrap(), [false; 4]);
}

#[tokio::test]
async fn startup_all_lanes_off_and_recovered_race_requires_resume() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let store = SqliteStore::open(&path).unwrap();
    let started = store
        .execute(
            "race",
            Command::StartRace {
                config: race_config(FinishCondition::Laps(10), Consequence::Abort),
                at: 0,
            },
        )
        .unwrap();
    let running = store
        .execute("race", Command::AdvanceRace { to: 10 })
        .unwrap();
    assert!(running.sequence > started.sequence);
    let power = RecordingPower::default();
    let calls = power.calls.clone();
    let runtime = RaceRuntime::with_hardware(
        store.clone(),
        "race",
        hardware_config(),
        SimulationTimingSource::default(),
        power,
    )
    .await
    .unwrap();
    assert_eq!(calls.lock().unwrap().first(), Some(&[false; 4]));
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|output| *output == [false; 4])
    );
    let recovered = runtime.snapshot().await.unwrap();
    assert!(recovered.sequence > running.sequence);
    assert!(matches!(
        recovered.status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Paused { paused_at: 10 },
            ..
        })
    ));
    assert!(matches!(
        store.events("race").unwrap().last(),
        Some(Event::RacePaused { .. })
    ));
}

#[tokio::test]
async fn recovered_paused_race_is_preserved_without_an_extra_commit() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("paused.db")).unwrap();
    store
        .execute(
            "race",
            Command::StartRace {
                config: race_config(FinishCondition::Laps(10), Consequence::Abort),
                at: 0,
            },
        )
        .unwrap();
    store
        .execute("race", Command::AdvanceRace { to: 10 })
        .unwrap();
    let paused = store
        .execute("race", Command::PauseRace { at: 20 })
        .unwrap();

    let runtime = runtime_with_power(store, SimulationPowerOutput::default()).await;
    let recovered = runtime.snapshot().await.unwrap();
    assert_eq!(recovered.sequence, paused.sequence);
    assert!(matches!(
        recovered.status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Paused { paused_at: 20 },
            ..
        })
    ));
}

#[tokio::test]
async fn recovery_pause_from_restarting_preserves_original_paused_at() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    store
        .execute(
            "race",
            Command::StartRace {
                config: race_config(FinishCondition::Laps(10), Consequence::Abort),
                at: 0,
            },
        )
        .unwrap();
    store
        .execute("race", Command::AdvanceRace { to: 10 })
        .unwrap();
    store
        .execute("race", Command::PauseRace { at: 20 })
        .unwrap();
    store
        .execute("race", Command::ResumeRace { at: 30 })
        .unwrap();

    let runtime = runtime_with_power(store, SimulationPowerOutput::default()).await;
    assert!(matches!(
        runtime.snapshot().await.unwrap().status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Paused { paused_at: 20 },
            ..
        })
    ));
}

#[tokio::test(start_paused = true)]
async fn periodic_refresh_synchronizes_external_commits() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    let power = SimulationPowerOutput::default();
    let runtime = runtime_with_power(store.clone(), power.clone()).await;
    runtime
        .apply(Command::StartRace {
            config: race_config(FinishCondition::Laps(10), Consequence::Abort),
            at: 0,
        })
        .await
        .unwrap();
    runtime
        .apply(Command::AdvanceRace { to: 10 })
        .await
        .unwrap();
    assert_eq!(power.lane_power(), [true, true, false, false]);
    let mut updates = runtime.subscribe();
    store
        .execute("race", Command::PauseRace { at: 20 })
        .unwrap();

    tokio::time::advance(Duration::from_millis(100)).await;
    updates.recv().await.unwrap();
    assert_eq!(power.lane_power(), [false; 4]);
}
