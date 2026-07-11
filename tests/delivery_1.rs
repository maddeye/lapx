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

#[test]
fn invalid_start_config() {
    for lanes in [0, 5] {
        let mut bad = config();
        bad.lanes = lanes;
        assert_eq!(
            RaceEngine::new().handle(Command::StartRace { config: bad, at: 0 }),
            Err(DomainError::InvalidLaneCount)
        );
    }
    let mut bad = config();
    bad.minimum_lap_time_ms = 0;
    assert_eq!(
        RaceEngine::new().handle(Command::StartRace { config: bad, at: 0 }),
        Err(DomainError::InvalidDuration)
    );
    let mut bad = config();
    bad.finish_condition = FinishCondition::Laps(0);
    assert_eq!(
        RaceEngine::new().handle(Command::StartRace { config: bad, at: 0 }),
        Err(DomainError::InvalidTarget)
    );
}

#[test]
fn invalid_control_commands() {
    let engine = RaceEngine::new();
    assert_eq!(
        engine.handle(Command::AdvanceRace { to: 1 }),
        Err(DomainError::InvalidPhase)
    );
    assert_eq!(
        engine.handle(Command::SensorTriggered {
            lane: 1,
            at: 1,
            edge: SignalEdge::Rising
        }),
        Err(DomainError::InvalidPhase)
    );
    assert_eq!(
        engine.handle(Command::CorrectLaps {
            lane: 1,
            delta_thousandths: 1_000,
            at: 1
        }),
        Err(DomainError::InvalidPhase)
    );
}
