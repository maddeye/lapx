use lapx::domain::*;

fn config() -> RaceConfig {
    RaceConfig {
        lanes: 2,
        start_sequence_ms: 1_000,
        minimum_lap_time_ms: 3_000,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
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
fn replay_at() {
    let (engine, mut events) = started();
    assert_eq!(engine.state().phase, RacePhase::Starting);
    events.extend(engine.handle(Command::AdvanceRace { to: 2_000 }).unwrap());
    let replayed = RaceEngine::replay(&events).unwrap();
    assert_eq!(replayed.state().phase, RacePhase::Running);
    assert_eq!(replayed.state().official_start_at, Some(1_100));
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
