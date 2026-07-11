use lapx::domain::*;

fn config() -> RaceConfig {
    RaceConfig {
        lanes: 2,
        start_sequence_ms: 1_000,
        restart_sequence_ms: 500,
        minimum_lap_time_ms: 3_000,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}
fn started() -> (RaceEngine, Vec<Event>) {
    let events = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(),
            at: 100,
        })
        .unwrap();
    (RaceEngine::replay(&events).unwrap(), events)
}

#[test]
fn start_sequence() {
    let (engine, _) = started();
    assert!(
        engine
            .handle(Command::AdvanceRace { to: 1_099 })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        engine.handle(Command::AdvanceRace { to: 1_100 }).unwrap(),
        vec![Event::OfficialStart { at: 1_100 }]
    );
}

#[test]
fn replay_at_uses_only_committed_events_through_timestamp() {
    let (starting, mut events) = started();
    events.extend(starting.handle(Command::AdvanceRace { to: 1_100 }).unwrap());
    let running = RaceEngine::replay(&events).unwrap();
    events.extend(
        running
            .handle(Command::SensorTriggered {
                lane: 1,
                at: 4_100,
                edge: SignalEdge::Rising,
            })
            .unwrap(),
    );

    assert!(matches!(
        RaceEngine::replay_at(&events, 1_099).unwrap().state().phase,
        RacePhase::Starting {
            start_due_at: 1_100,
            ..
        }
    ));
    assert!(matches!(
        RaceEngine::replay_at(&events, 1_100).unwrap().state().phase,
        RacePhase::Running {
            official_start_at: 1_100,
            ..
        }
    ));
    assert_eq!(
        RaceEngine::replay_at(&events, 4_099)
            .unwrap()
            .state()
            .lane(1)
            .unwrap()
            .laps,
        0
    );
    assert_eq!(
        RaceEngine::replay_at(&events, 4_100)
            .unwrap()
            .state()
            .lane(1)
            .unwrap()
            .laps,
        1
    );

    let (_, committed_before_due) = started();
    assert!(matches!(
        RaceEngine::replay_at(&committed_before_due, 9_999)
            .unwrap()
            .state()
            .phase,
        RacePhase::Starting { .. }
    ));
}

#[test]
fn commands_before_last() {
    let (engine, mut events) = started();
    events.extend(engine.handle(Command::AdvanceRace { to: 1_100 }).unwrap());
    let running = RaceEngine::replay(&events).unwrap();
    assert_eq!(
        running.handle(Command::AdvanceRace { to: 1_099 }),
        Err(DomainError::TimestampBeforeLast {
            last: 1_100,
            command: 1_099
        })
    );
}

#[test]
fn due_events_precede_later_commands() {
    let (engine, _) = started();
    let events = engine.handle(Command::AdvanceRace { to: 4_100 }).unwrap();
    assert_eq!(events.first(), Some(&Event::OfficialStart { at: 1_100 }));
}
