use lapx::{domain::*, store::SqliteStore};
use std::process::{Command as ProcessCommand, Stdio};
use tempfile::tempdir;

fn config(chaos_consequence: Consequence) -> RaceConfig {
    RaceConfig {
        lanes: 2,
        driver_ids: vec![None; 2],
        start_sequence_ms: 100,
        restart_sequence_ms: 50,
        minimum_lap_time_ms: 100,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::ResultTimePenaltyMs(10),
        chaos_consequence,
    }
}

fn running(chaos_consequence: Consequence) -> Vec<Event> {
    let mut events = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(chaos_consequence),
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
fn lane_chaos_pauses_then_applies_configured_consequence() {
    let mut events = running(Consequence::ResultTimePenaltyMs(250));
    let emitted = issue(
        &mut events,
        Command::TriggerChaos {
            source: ChaosSource::Lane(2),
            at: 200,
        },
    );
    assert_eq!(
        emitted,
        vec![
            Event::ChaosTriggered {
                source: ChaosSource::Lane(2),
                at: 200,
            },
            Event::RacePaused { at: 200 },
            Event::ConsequenceApplied {
                lane: 2,
                consequence: Consequence::ResultTimePenaltyMs(250),
                at: 200,
            },
        ]
    );
    let state = RaceEngine::replay(&events).unwrap();
    assert!(matches!(
        state.state().status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Paused { .. },
            ..
        })
    ));
    assert_eq!(state.state().lane(2).unwrap().result_time_penalty_ms, 250);
}

#[test]
fn race_control_chaos_never_applies_a_consequence() {
    for configured in [
        Consequence::Abort,
        Consequence::ResultTimePenaltyMs(250),
        Consequence::LanePowerOffMs(250),
    ] {
        let mut events = running(configured);
        let emitted = issue(
            &mut events,
            Command::TriggerChaos {
                source: ChaosSource::RaceControl,
                at: 200,
            },
        );
        assert_eq!(
            emitted,
            vec![
                Event::ChaosTriggered {
                    source: ChaosSource::RaceControl,
                    at: 200,
                },
                Event::RacePaused { at: 200 },
            ]
        );
        assert!(matches!(
            RaceEngine::replay(&events).unwrap().state().status,
            RaceStatus::Active(ActiveRace {
                control: RaceControl::Paused { .. },
                ..
            })
        ));
    }
}

#[test]
fn chaos_while_paused_or_restarting_retains_the_original_pause() {
    let mut events = running(Consequence::ResultTimePenaltyMs(250));
    issue(&mut events, Command::PauseRace { at: 200 });
    for (at, resume_first) in [(250, false), (320, true)] {
        if resume_first {
            issue(&mut events, Command::ResumeRace { at: 300 });
        }
        let emitted = issue(
            &mut events,
            Command::TriggerChaos {
                source: ChaosSource::Lane(1),
                at,
            },
        );
        assert!(matches!(
            emitted.as_slice(),
            [
                Event::ChaosTriggered { .. },
                Event::RacePaused { .. },
                Event::ConsequenceApplied { .. }
            ]
        ));
        assert!(matches!(
            RaceEngine::replay(&events).unwrap().state().status,
            RaceStatus::Active(ActiveRace {
                control: RaceControl::Paused { paused_at: 200 },
                ..
            })
        ));
    }
    let state = RaceEngine::replay(&events).unwrap();
    assert_eq!(state.state().lane(1).unwrap().result_time_penalty_ms, 500);
    assert!(
        state
            .handle(Command::AdvanceRace { to: 350 })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn lane_power_off_expiry_is_deterministic_and_does_not_repower_while_paused() {
    let mut events = running(Consequence::LanePowerOffMs(300));
    issue(
        &mut events,
        Command::TriggerChaos {
            source: ChaosSource::Lane(1),
            at: 200,
        },
    );
    let paused = RaceEngine::replay(&events).unwrap();
    assert_eq!(paused.state().lane(1).unwrap().power_off_until, Some(500));
    assert_eq!(paused.state().intended_lane_power(1), Some(false));

    assert_eq!(
        issue(&mut events, Command::AdvanceRace { to: 500 }),
        vec![Event::LanePowerOffExpired { lane: 1, at: 500 }]
    );
    let expired = RaceEngine::replay(&events).unwrap();
    assert_eq!(expired.state().lane(1).unwrap().power_off_until, None);
    assert_eq!(expired.state().intended_lane_power(1), Some(false));

    issue(&mut events, Command::ResumeRace { at: 600 });
    issue(&mut events, Command::AdvanceRace { to: 650 });
    assert_eq!(
        RaceEngine::replay(&events)
            .unwrap()
            .state()
            .intended_lane_power(1),
        Some(true)
    );
}

#[test]
fn aborting_lane_chaos_is_terminal() {
    let mut events = running(Consequence::Abort);
    let emitted = issue(
        &mut events,
        Command::TriggerChaos {
            source: ChaosSource::Lane(1),
            at: 200,
        },
    );
    assert!(matches!(
        emitted.as_slice(),
        [
            Event::ChaosTriggered { .. },
            Event::RacePaused { .. },
            Event::ConsequenceApplied {
                consequence: Consequence::Abort,
                ..
            }
        ]
    ));
    assert!(matches!(
        RaceEngine::replay(&events).unwrap().state().status,
        RaceStatus::Aborted
    ));
}

#[test]
fn due_events_are_ordered_by_timestamp_type_and_lane() {
    let mut race_config = config(Consequence::Abort);
    race_config.start_sequence_ms = 1_000;
    race_config.false_start_consequence = Consequence::LanePowerOffMs(900);
    let mut events = RaceEngine::new()
        .handle(Command::StartRace {
            config: race_config,
            at: 0,
        })
        .unwrap();
    for lane in [2, 1] {
        issue(
            &mut events,
            Command::SensorTriggered {
                lane,
                at: 100,
                edge: SignalEdge::Rising,
            },
        );
    }
    assert_eq!(
        issue(&mut events, Command::AdvanceRace { to: 1_000 }),
        vec![
            Event::LanePowerOffExpired { lane: 1, at: 1_000 },
            Event::LanePowerOffExpired { lane: 2, at: 1_000 },
            Event::OfficialStart { at: 1_000 },
        ]
    );
}

#[test]
fn invalid_lane_chaos_and_malformed_consequence_chain_are_rejected() {
    let events = running(Consequence::ResultTimePenaltyMs(10));
    assert_eq!(
        RaceEngine::replay(&events)
            .unwrap()
            .handle(Command::TriggerChaos {
                source: ChaosSource::Lane(3),
                at: 200,
            }),
        Err(DomainError::InvalidLane)
    );

    let mut malformed = events;
    malformed.extend([
        Event::ChaosTriggered {
            source: ChaosSource::Lane(1),
            at: 200,
        },
        Event::RacePaused { at: 200 },
        Event::ConsequenceApplied {
            lane: 2,
            consequence: Consequence::ResultTimePenaltyMs(10),
            at: 200,
        },
    ]);
    assert_eq!(
        RaceEngine::replay(&malformed).unwrap_err(),
        DomainError::InvalidEventOrder
    );
}

#[test]
fn chaos_cli_command_uses_durable_replay_path() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("lapx.db");
    cli(
        &db,
        "start",
        serde_json::json!({
            "race_id": "race",
            "at": 0,
            "config": config(Consequence::ResultTimePenaltyMs(250)),
        }),
    );
    cli(
        &db,
        "advance",
        serde_json::json!({"race_id": "race", "to": 100}),
    );
    let paused = cli(
        &db,
        "chaos",
        serde_json::json!({
            "race_id": "race",
            "source": {"lane": 2},
            "at": 200,
        }),
    );
    assert!(matches!(
        paused.status,
        RaceStatus::Active(ActiveRace {
            control: RaceControl::Paused { .. },
            ..
        })
    ));
    assert_eq!(paused.lane(2).unwrap().result_time_penalty_ms, 250);
    let stored = SqliteStore::open(&db).unwrap().events("race").unwrap();
    assert!(matches!(
        stored.last(),
        Some(Event::ConsequenceApplied { lane: 2, .. })
    ));
}

#[test]
fn overlapping_lane_cutoffs_expire_at_the_later_due_time() {
    let mut race_config = config(Consequence::LanePowerOffMs(100));
    race_config.false_start_consequence = Consequence::LanePowerOffMs(600);
    let mut events = RaceEngine::new()
        .handle(Command::StartRace {
            config: race_config,
            at: 0,
        })
        .unwrap();
    issue(
        &mut events,
        Command::SensorTriggered {
            lane: 1,
            at: 50,
            edge: SignalEdge::Rising,
        },
    );
    issue(&mut events, Command::AdvanceRace { to: 100 });
    issue(
        &mut events,
        Command::TriggerChaos {
            source: ChaosSource::Lane(1),
            at: 200,
        },
    );
    assert_eq!(
        RaceEngine::replay(&events)
            .unwrap()
            .state()
            .lane(1)
            .unwrap()
            .power_off_until,
        Some(650)
    );
}
