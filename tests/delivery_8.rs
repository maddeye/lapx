use lapx::{domain::*, store::SqliteStore};
use std::process::{Command as ProcessCommand, Stdio};
use tempfile::tempdir;

fn config(condition: FinishCondition) -> RaceConfig {
    RaceConfig {
        lanes: 2,
        driver_ids: vec![None; 2],
        start_sequence_ms: 100,
        restart_sequence_ms: 50,
        minimum_lap_time_ms: 100,
        finish_condition: condition,
        finish_mode: FinishMode::AllCurrentLap,
        false_start_consequence: Consequence::ResultTimePenaltyMs(10),
        chaos_consequence: Consequence::ResultTimePenaltyMs(20),
    }
}

fn running(condition: FinishCondition) -> Vec<Event> {
    let mut events = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(condition),
            at: 0,
        })
        .unwrap();
    events.extend(
        RaceEngine::replay(&events)
            .unwrap()
            .handle(Command::AdvanceRace { to: 100 })
            .unwrap(),
    );
    events
}

fn issue(events: &mut Vec<Event>, command: Command) -> Vec<Event> {
    let emitted = RaceEngine::replay(events).unwrap().handle(command).unwrap();
    events.extend(emitted.clone());
    emitted
}

fn cli(db: &std::path::Path, name: &str, input: serde_json::Value) -> RaceState {
    let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_lapxctl"))
        .args([name, "--json", "-"])
        .env("LAPX_DB", db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn rennpause_freezes_elapsed_time_and_intended_power() {
    let mut events = running(FinishCondition::TimeMs(1_000));
    assert_eq!(
        issue(&mut events, Command::PauseRace { at: 400 }),
        vec![Event::RacePaused { at: 400 }]
    );
    let paused = RaceEngine::replay(&events).unwrap();
    assert!(matches!(
        paused.state().status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Paused { paused_at: 400 },
            ..
        })
    ));
    assert_eq!(paused.state().race_elapsed_ms(900), Some(300));
    assert_eq!(paused.state().intended_lane_power(1), Some(false));
    assert!(
        paused
            .handle(Command::AdvanceRace { to: 2_000 })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn measurements_during_rennpause_use_protocol_timestamps() {
    let mut events = running(FinishCondition::Laps(10));
    issue(&mut events, Command::PauseRace { at: 250 });
    let first = issue(
        &mut events,
        Command::SensorTriggered {
            lane: 1,
            at: 300,
            edge: SignalEdge::Rising,
        },
    );
    let second = issue(
        &mut events,
        Command::SensorTriggered {
            lane: 1,
            at: 450,
            edge: SignalEdge::Rising,
        },
    );
    assert!(matches!(
        first.last(),
        Some(Event::ValidLap {
            lap_time_ms: 200,
            ..
        })
    ));
    assert!(matches!(
        second.last(),
        Some(Event::ValidLap {
            lap_time_ms: 150,
            ..
        })
    ));
    let state = RaceEngine::replay(&events).unwrap();
    assert_eq!(state.state().lane(1).unwrap().laps, 2);
    assert_eq!(state.state().race_elapsed_ms(900), Some(150));
    assert_eq!(state.state().intended_lane_power(1), Some(false));
}

#[test]
fn measurements_remain_valid_during_restart_for_each_lifecycle() {
    let mut starting = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(FinishCondition::Laps(10)),
            at: 0,
        })
        .unwrap();
    issue(&mut starting, Command::PauseRace { at: 25 });
    issue(&mut starting, Command::ResumeRace { at: 50 });
    let false_start = issue(
        &mut starting,
        Command::SensorTriggered {
            lane: 1,
            at: 75,
            edge: SignalEdge::Rising,
        },
    );
    assert!(matches!(
        false_start.as_slice(),
        [
            Event::MeasurementCaptured { .. },
            Event::FalseStartDetected { .. },
            Event::ConsequenceApplied { .. }
        ]
    ));

    let mut running = running(FinishCondition::Laps(10));
    issue(&mut running, Command::PauseRace { at: 250 });
    issue(&mut running, Command::ResumeRace { at: 300 });
    let lap = issue(
        &mut running,
        Command::SensorTriggered {
            lane: 1,
            at: 320,
            edge: SignalEdge::Rising,
        },
    );
    assert!(matches!(
        lap.last(),
        Some(Event::ValidLap {
            lap_time_ms: 220,
            ..
        })
    ));
    assert!(matches!(
        RaceEngine::replay(&running).unwrap().state().status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Restarting {
                paused_at: 250,
                restart_due_at: 350,
            },
            ..
        })
    ));
}

#[test]
fn restart_sequence_resumes_suspended_phase_and_shifts_time_limit() {
    let mut events = running(FinishCondition::TimeMs(1_000));
    issue(&mut events, Command::PauseRace { at: 400 });
    assert_eq!(
        issue(&mut events, Command::ResumeRace { at: 900 }),
        vec![Event::RestartSequencePlanned {
            due_at: 950,
            at: 900,
        }]
    );
    let restarting = RaceEngine::replay(&events).unwrap();
    assert!(matches!(
        restarting.state().status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Restarting {
                paused_at: 400,
                restart_due_at: 950,
            },
            ..
        })
    ));
    assert_eq!(restarting.state().intended_lane_power(1), Some(false));

    assert_eq!(
        issue(&mut events, Command::AdvanceRace { to: 950 }),
        vec![Event::RaceResumed { at: 950 }]
    );
    let resumed = RaceEngine::replay(&events).unwrap();
    assert!(matches!(
        resumed.state().status,
        RaceStatus::Active(ActiveRace {
            lifecycle: Lifecycle::Running { .. },
            control: RaceControl::Live,
            accumulated_pause_ms: 550,
            ..
        })
    ));
    assert_eq!(resumed.state().race_elapsed_ms(950), Some(300));
    assert_eq!(resumed.state().intended_lane_power(1), Some(true));
    assert!(
        resumed
            .handle(Command::AdvanceRace { to: 1_649 })
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        resumed
            .handle(Command::AdvanceRace { to: 1_650 })
            .unwrap()
            .first(),
        Some(Event::FinishConditionReached { at: 1_650, .. })
    ));
}

#[test]
fn resume_sequence_restores_initial_starting_phase() {
    let mut events = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(FinishCondition::Laps(10)),
            at: 0,
        })
        .unwrap();
    issue(&mut events, Command::PauseRace { at: 25 });
    issue(&mut events, Command::ResumeRace { at: 75 });
    let resumed = issue(&mut events, Command::AdvanceRace { to: 125 });
    assert_eq!(resumed, vec![Event::RaceResumed { at: 125 }]);
    assert!(matches!(
        RaceEngine::replay(&events).unwrap().state().status,
        RaceStatus::Active(ActiveRace {
            lifecycle: Lifecycle::Starting { start_due_at: 200 },
            control: RaceControl::Live,
            ..
        })
    ));
    assert!(matches!(
        issue(&mut events, Command::AdvanceRace { to: 200 }).as_slice(),
        [Event::OfficialStart { at: 200 }]
    ));
    assert_eq!(
        RaceEngine::replay(&events)
            .unwrap()
            .state()
            .race_elapsed_ms(300),
        Some(100)
    );
}

#[test]
fn pause_is_valid_while_finishing() {
    let mut events = running(FinishCondition::TimeMs(100));
    issue(&mut events, Command::AdvanceRace { to: 200 });
    assert!(matches!(
        RaceEngine::replay(&events).unwrap().state().status,
        RaceStatus::Active(ActiveRace {
            lifecycle: Lifecycle::Finishing { .. },
            ..
        })
    ));
    issue(&mut events, Command::PauseRace { at: 210 });
    issue(&mut events, Command::ResumeRace { at: 220 });
    let emitted = issue(
        &mut events,
        Command::SensorTriggered {
            lane: 1,
            at: 250,
            edge: SignalEdge::Rising,
        },
    );
    assert!(matches!(
        emitted.as_slice(),
        [
            Event::MeasurementCaptured { .. },
            Event::ValidLap {
                lap_time_ms: 150,
                ..
            },
            Event::LaneFinished { lane: 1, .. }
        ]
    ));
    assert!(matches!(
        RaceEngine::replay(&events).unwrap().state().status,
        RaceStatus::Active(ActiveRace {
            lifecycle: Lifecycle::Finishing { .. },
            control: RaceControl::Restarting {
                paused_at: 210,
                restart_due_at: 270,
            },
            ..
        })
    ));
}

#[test]
fn malformed_pause_and_restart_replay_chains_are_rejected() {
    let mut events = running(FinishCondition::Laps(10));
    events.push(Event::RestartSequencePlanned {
        due_at: 250,
        at: 200,
    });
    assert_eq!(
        RaceEngine::replay(&events).unwrap_err(),
        DomainError::InvalidEventOrder
    );

    let mut incomplete = running(FinishCondition::Laps(10));
    incomplete.push(Event::ChaosTriggered {
        source: ChaosSource::RaceControl,
        at: 200,
    });
    assert_eq!(
        RaceEngine::replay(&incomplete).unwrap_err(),
        DomainError::InvalidEventOrder
    );
}

#[test]
fn pause_and_resume_cli_commands_are_durable() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("lapx.db");
    cli(
        &db,
        "start",
        serde_json::json!({
            "race_id": "race",
            "at": 0,
            "config": config(FinishCondition::Laps(10)),
        }),
    );
    cli(
        &db,
        "advance",
        serde_json::json!({"race_id": "race", "to": 100}),
    );
    let paused = cli(
        &db,
        "pause",
        serde_json::json!({"race_id": "race", "at": 200}),
    );
    assert!(matches!(
        paused.status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Paused { .. },
            ..
        })
    ));
    let restarting = cli(
        &db,
        "resume",
        serde_json::json!({"race_id": "race", "at": 300}),
    );
    assert!(matches!(
        restarting.status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Restarting {
                restart_due_at: 350,
                ..
            },
            ..
        })
    ));
    cli(
        &db,
        "advance",
        serde_json::json!({"race_id": "race", "to": 350}),
    );
    assert!(matches!(
        SqliteStore::open(&db).unwrap().load("race").unwrap().status,
        RaceStatus::Active(ActiveRace {
            lifecycle: Lifecycle::Running { .. },
            accumulated_pause_ms: 150,
            ..
        })
    ));
}

#[test]
fn restart_sequence_overflow_is_rejected() {
    let mut events = running(FinishCondition::Laps(10));
    issue(&mut events, Command::PauseRace { at: u64::MAX - 100 });
    assert_eq!(
        RaceEngine::replay(&events)
            .unwrap()
            .handle(Command::ResumeRace { at: u64::MAX - 25 }),
        Err(DomainError::InvalidDuration)
    );
}
