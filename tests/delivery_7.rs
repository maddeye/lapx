use lapx::{domain::*, store::SqliteStore};
use std::process::Command as ProcessCommand;
use tempfile::tempdir;

fn config(consequence: Consequence) -> RaceConfig {
    RaceConfig {
        lanes: 2,
        driver_ids: vec![None; 2],
        start_sequence_ms: 1_000,
        restart_sequence_ms: 500,
        minimum_lap_time_ms: 3_000,
        finish_condition: FinishCondition::Laps(1),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: consequence,
        chaos_consequence: Consequence::ResultTimePenaltyMs(200),
    }
}

fn start(consequence: Consequence) -> Vec<Event> {
    RaceEngine::new()
        .handle(Command::StartRace {
            config: config(consequence),
            at: 100,
        })
        .unwrap()
}

fn cli(db: &std::path::Path, name: &str, input: serde_json::Value) -> RaceState {
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_lapxctl"))
        .args([name, "--json", "-"])
        .env("LAPX_DB", db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.to_string().as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn false_start() {
    let mut events = start(Consequence::ResultTimePenaltyMs(250));
    let engine = RaceEngine::replay(&events).unwrap();
    let emitted = engine
        .handle(Command::SensorTriggered {
            lane: 1,
            at: 200,
            edge: SignalEdge::Rising,
        })
        .unwrap();

    assert_eq!(
        emitted,
        vec![
            Event::MeasurementCaptured {
                lane: 1,
                at: 200,
                edge: SignalEdge::Rising,
            },
            Event::FalseStartDetected { lane: 1, at: 200 },
            Event::ConsequenceApplied {
                lane: 1,
                consequence: Consequence::ResultTimePenaltyMs(250),
                at: 200,
            },
        ]
    );
    events.extend(emitted);
    events.extend(
        RaceEngine::replay(&events)
            .unwrap()
            .handle(Command::SensorTriggered {
                lane: 1,
                at: 300,
                edge: SignalEdge::Rising,
            })
            .unwrap(),
    );
    assert_eq!(
        RaceEngine::replay(&events)
            .unwrap()
            .state()
            .lane(1)
            .unwrap()
            .result_time_penalty_ms,
        500
    );
}

#[test]
fn messereignis_during_startsequence_cuts_only_that_lanes_intended_power() {
    let mut events = start(Consequence::LanePowerOffMs(300));
    let emitted = RaceEngine::replay(&events)
        .unwrap()
        .handle(Command::SensorTriggered {
            lane: 2,
            at: 200,
            edge: SignalEdge::Falling,
        })
        .unwrap();
    assert!(matches!(
        emitted.as_slice(),
        [
            Event::MeasurementCaptured { lane: 2, .. },
            Event::FalseStartDetected { lane: 2, .. },
            Event::ConsequenceApplied {
                lane: 2,
                consequence: Consequence::LanePowerOffMs(300),
                ..
            }
        ]
    ));
    events.extend(emitted);
    let state = RaceEngine::replay(&events).unwrap();
    assert_eq!(state.state().lane(2).unwrap().power_off_until, Some(500));
    assert_eq!(state.state().intended_lane_power(1), Some(false));
    assert_eq!(state.state().intended_lane_power(2), Some(false));
}

#[test]
fn abort_consequence_is_terminal() {
    let mut events = start(Consequence::Abort);
    events.extend(
        RaceEngine::replay(&events)
            .unwrap()
            .handle(Command::SensorTriggered {
                lane: 1,
                at: 200,
                edge: SignalEdge::Rising,
            })
            .unwrap(),
    );
    let engine = RaceEngine::replay(&events).unwrap();
    assert!(matches!(engine.state().status, RaceStatus::Aborted));
    assert_eq!(engine.state().intended_lane_power(1), Some(false));
    assert_eq!(
        engine.handle(Command::AdvanceRace { to: 10_000 }),
        Err(DomainError::InvalidPhase)
    );
}

#[test]
fn invalid_consequence_durations_and_overflow_are_rejected() {
    let mut zero_restart = config(Consequence::Abort);
    zero_restart.restart_sequence_ms = 0;
    assert_eq!(
        RaceEngine::new().handle(Command::StartRace {
            config: zero_restart,
            at: 0,
        }),
        Err(DomainError::InvalidDuration)
    );

    for consequence in [
        Consequence::ResultTimePenaltyMs(0),
        Consequence::LanePowerOffMs(0),
    ] {
        let mut bad_chaos = config(Consequence::Abort);
        bad_chaos.chaos_consequence = consequence;
        for bad in [config(consequence), bad_chaos] {
            assert_eq!(
                RaceEngine::new().handle(Command::StartRace { config: bad, at: 0 }),
                Err(DomainError::InvalidDuration)
            );
        }
    }

    let mut overflow_config = config(Consequence::LanePowerOffMs(2));
    overflow_config.start_sequence_ms = 3;
    let events = RaceEngine::new()
        .handle(Command::StartRace {
            config: overflow_config,
            at: u64::MAX - 3,
        })
        .unwrap();
    let engine = RaceEngine::replay(&events).unwrap();
    assert_eq!(
        engine.handle(Command::SensorTriggered {
            lane: 1,
            at: u64::MAX - 1,
            edge: SignalEdge::Rising,
        }),
        Err(DomainError::InvalidDuration)
    );
}

#[test]
fn false_start_cli_uses_the_durable_command_path() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("lapx.db");
    cli(
        &db,
        "start",
        serde_json::json!({
            "race_id": "race",
            "at": 100,
            "config": config(Consequence::ResultTimePenaltyMs(250)),
        }),
    );
    let state = cli(
        &db,
        "sensor",
        serde_json::json!({
            "race_id": "race",
            "lane": 1,
            "at": 200,
            "edge": "rising",
        }),
    );
    assert_eq!(state.lane(1).unwrap().result_time_penalty_ms, 250);
    assert_eq!(
        SqliteStore::open(&db)
            .unwrap()
            .events("race")
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn result_penalty_is_added_only_after_corrected_laps_are_compared() {
    let mut race_config = config(Consequence::ResultTimePenaltyMs(10));
    race_config.start_sequence_ms = 100;
    race_config.minimum_lap_time_ms = 100;
    race_config.finish_mode = FinishMode::AllCurrentLap;
    let mut events = RaceEngine::new()
        .handle(Command::StartRace {
            config: race_config,
            at: 0,
        })
        .unwrap();
    for command in [
        Command::SensorTriggered {
            lane: 1,
            at: 5,
            edge: SignalEdge::Rising,
        },
        Command::AdvanceRace { to: 100 },
        Command::SensorTriggered {
            lane: 1,
            at: 200,
            edge: SignalEdge::Rising,
        },
        Command::SensorTriggered {
            lane: 2,
            at: 201,
            edge: SignalEdge::Rising,
        },
    ] {
        events.extend(
            RaceEngine::replay(&events)
                .unwrap()
                .handle(command)
                .unwrap(),
        );
    }
    let standings = RaceEngine::replay(&events).unwrap().state().standings();
    assert_eq!(standings[0].lane, 2);
    assert_eq!(standings[0].corrected_laps_thousandths, 1_000);
    assert_eq!(standings[0].result_time_ms, Some(101));
    assert_eq!(standings[1].result_time_ms, Some(110));
}

#[test]
fn malformed_false_start_replay_chains_are_rejected() {
    let mut incomplete = start(Consequence::ResultTimePenaltyMs(10));
    incomplete.push(Event::MeasurementCaptured {
        lane: 1,
        at: 200,
        edge: SignalEdge::Rising,
    });
    assert_eq!(
        RaceEngine::replay(&incomplete).unwrap_err(),
        DomainError::InvalidEventOrder
    );

    let mut wrong = incomplete;
    wrong.extend([
        Event::FalseStartDetected { lane: 1, at: 200 },
        Event::ConsequenceApplied {
            lane: 2,
            consequence: Consequence::ResultTimePenaltyMs(10),
            at: 200,
        },
    ]);
    assert_eq!(
        RaceEngine::replay(&wrong).unwrap_err(),
        DomainError::InvalidEventOrder
    );
}
