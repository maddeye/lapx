use lapx::domain::*;

fn running() -> (RaceEngine, Vec<Event>) {
    let config = RaceConfig {
        lanes: 2,
        driver_ids: vec![None; 2],
        start_sequence_ms: 1_000,
        restart_sequence_ms: 500,
        minimum_lap_time_ms: 3_000,
        finish_condition: FinishCondition::Laps(10),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    };
    let mut events = RaceEngine::new()
        .handle(Command::StartRace { config, at: 0 })
        .unwrap();
    let engine = RaceEngine::replay(&events).unwrap();
    events.extend(engine.handle(Command::AdvanceRace { to: 1_000 }).unwrap());
    (RaceEngine::replay(&events).unwrap(), events)
}

fn issue(events: &mut Vec<Event>, command: Command) -> Vec<Event> {
    let engine = RaceEngine::replay(events).unwrap();
    let new = engine.handle(command).unwrap();
    events.extend(new.clone());
    new
}

#[test]
fn mindestrundenzeit() {
    let (_, mut events) = running();
    let rejected = issue(
        &mut events,
        Command::SensorTriggered {
            lane: 1,
            at: 3_999,
            edge: SignalEdge::Falling,
        },
    );
    assert!(matches!(
        rejected.as_slice(),
        [
            Event::MeasurementCaptured {
                edge: SignalEdge::Falling,
                ..
            },
            Event::MeasurementRejected {
                reason: RejectionReason::Mindestrundenzeit,
                ..
            }
        ]
    ));

    issue(
        &mut events,
        Command::SensorTriggered {
            lane: 1,
            at: 4_000,
            edge: SignalEdge::Rising,
        },
    );
    issue(
        &mut events,
        Command::SensorTriggered {
            lane: 1,
            at: 6_999,
            edge: SignalEdge::Rising,
        },
    );
    issue(
        &mut events,
        Command::SensorTriggered {
            lane: 1,
            at: 7_000,
            edge: SignalEdge::Rising,
        },
    );
    issue(
        &mut events,
        Command::SensorTriggered {
            lane: 1,
            at: 10_500,
            edge: SignalEdge::Rising,
        },
    );

    let state = RaceEngine::replay(&events).unwrap();
    let lane = state.state().lane(1).unwrap();
    assert_eq!(lane.laps, 3);
    assert_eq!(lane.last_lap_ms, Some(3_500));
    assert_eq!(lane.best_lap_ms, Some(3_000));
}

#[test]
fn invalid_lane_is_rejected() {
    let (engine, _) = running();
    assert_eq!(
        engine.handle(Command::SensorTriggered {
            lane: 3,
            at: 4_000,
            edge: SignalEdge::Rising
        }),
        Err(DomainError::InvalidLane)
    );
}
