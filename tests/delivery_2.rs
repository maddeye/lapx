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

#[test]
fn replay() {
    let events = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(),
            at: 100,
        })
        .unwrap();
    let first = RaceEngine::replay(&events).unwrap();
    let second = RaceEngine::replay(&events).unwrap();
    assert_eq!(first.state(), second.state());
    assert_eq!(
        serde_json::to_vec(first.state()).unwrap(),
        serde_json::to_vec(second.state()).unwrap()
    );
    assert_eq!(first.state().phase, RacePhase::Starting);
    assert_eq!(first.state().lanes.len(), 2);
}

#[test]
fn replay_rejects_impossible_event_order() {
    assert_eq!(
        RaceEngine::replay(&[Event::OfficialStart { at: 10 }]).unwrap_err(),
        DomainError::InvalidEventOrder
    );
    let events = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(),
            at: 0,
        })
        .unwrap();
    let mut running = events;
    running.push(Event::OfficialStart { at: 1_000 });
    running.push(Event::ValidLap {
        lane: 1,
        at: 4_000,
        lap_time_ms: 3_000,
    });
    assert_eq!(
        RaceEngine::replay(&running).unwrap_err(),
        DomainError::InvalidEventOrder
    );
}
