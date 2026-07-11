use lapx::domain::*;

fn config(condition: FinishCondition, mode: FinishMode) -> RaceConfig {
    RaceConfig {
        lanes: 2,
        start_sequence_ms: 10,
        restart_sequence_ms: 5,
        minimum_lap_time_ms: 100,
        finish_condition: condition,
        finish_mode: mode,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}
fn start(condition: FinishCondition, mode: FinishMode) -> Vec<Event> {
    let mut events = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(condition, mode),
            at: 0,
        })
        .unwrap();
    let engine = RaceEngine::replay(&events).unwrap();
    events.extend(engine.handle(Command::AdvanceRace { to: 10 }).unwrap());
    events
}
fn issue(events: &mut Vec<Event>, command: Command) -> Vec<Event> {
    let new = RaceEngine::replay(events).unwrap().handle(command).unwrap();
    events.extend(new.clone());
    new
}
fn sensor(events: &mut Vec<Event>, lane: u8, at: u64) -> Vec<Event> {
    issue(
        events,
        Command::SensorTriggered {
            lane,
            at,
            edge: SignalEdge::Rising,
        },
    )
}

#[test]
fn finish() {
    for mode in [FinishMode::Immediate, FinishMode::LeaderLap] {
        let mut events = start(FinishCondition::Laps(1), mode);
        let emitted = sensor(&mut events, 1, 110);
        assert!(matches!(
            emitted.as_slice(),
            [
                Event::MeasurementCaptured { .. },
                Event::ValidLap { .. },
                Event::FinishConditionReached { leader_lane: 1, .. },
                Event::LaneFinished { lane: 1, .. },
                Event::LaneFinished { lane: 2, .. },
                Event::RaceFinished { .. }
            ]
        ));
        assert!(matches!(
            RaceEngine::replay(&events).unwrap().state().phase,
            RacePhase::Finished { .. }
        ));
    }
}

#[test]
fn finish_leader_lap_waits_for_condition_time_leader() {
    let mut events = start(FinishCondition::TimeMs(100), FinishMode::LeaderLap);
    issue(&mut events, Command::AdvanceRace { to: 110 });
    assert!(matches!(
        RaceEngine::replay(&events).unwrap().state().phase,
        RacePhase::Finishing { .. }
    ));
    sensor(&mut events, 2, 120);
    assert!(matches!(
        RaceEngine::replay(&events).unwrap().state().phase,
        RacePhase::Finishing { .. }
    ));
    sensor(&mut events, 1, 121);
    assert!(matches!(
        RaceEngine::replay(&events).unwrap().state().phase,
        RacePhase::Finished { .. }
    ));
}

#[test]
fn finish_all_current_lap_gives_each_lane_one_crossing() {
    let mut events = start(FinishCondition::TimeMs(100), FinishMode::AllCurrentLap);
    issue(&mut events, Command::AdvanceRace { to: 110 });
    sensor(&mut events, 1, 120);
    let rejected = sensor(&mut events, 1, 220);
    assert!(matches!(
        rejected.last(),
        Some(Event::MeasurementRejected {
            reason: RejectionReason::LaneAlreadyFinished,
            ..
        })
    ));
    sensor(&mut events, 2, 221);
    assert!(matches!(
        RaceEngine::replay(&events).unwrap().state().phase,
        RacePhase::Finished { .. }
    ));

    let mut lap_limited = start(FinishCondition::Laps(1), FinishMode::AllCurrentLap);
    let trigger = sensor(&mut lap_limited, 1, 110);
    assert!(matches!(
        trigger.as_slice(),
        [
            Event::MeasurementCaptured { .. },
            Event::ValidLap { .. },
            Event::FinishConditionReached { .. },
            Event::LaneFinished { lane: 1, at: 110 }
        ]
    ));
    let rejected = sensor(&mut lap_limited, 1, 210);
    assert!(matches!(
        rejected.last(),
        Some(Event::MeasurementRejected {
            reason: RejectionReason::LaneAlreadyFinished,
            ..
        })
    ));
    sensor(&mut lap_limited, 2, 211);
    let state = RaceEngine::replay(&lap_limited).unwrap();
    assert!(matches!(state.state().phase, RacePhase::Finished { .. }));
    assert_eq!(state.state().lane(1).unwrap().laps, 1);
    assert_eq!(state.state().lane(2).unwrap().laps, 1);
}

#[test]
fn live_correction() {
    let mut events = start(FinishCondition::Laps(1), FinishMode::Immediate);
    assert_eq!(
        RaceEngine::replay(&events)
            .unwrap()
            .handle(Command::CorrectLaps {
                lane: 1,
                delta_thousandths: 500,
                at: 50
            }),
        Err(DomainError::InvalidPhase)
    );
    sensor(&mut events, 1, 110);
    issue(
        &mut events,
        Command::CorrectLaps {
            lane: 2,
            delta_thousandths: 1_500,
            at: 111,
        },
    );
    let engine = RaceEngine::replay(&events).unwrap();
    assert_eq!(
        engine.state().lane(2).unwrap().corrected_laps_thousandths,
        1_500
    );
    assert_eq!(engine.state().standings()[0].lane, 2);
    assert_eq!(
        engine.handle(Command::CorrectLaps {
            lane: 1,
            delta_thousandths: -1_001,
            at: 112
        }),
        Err(DomainError::NegativeCorrectedLaps)
    );
}

#[test]
fn sensor_after_due_time() {
    let mut events = start(FinishCondition::TimeMs(100), FinishMode::Immediate);
    let emitted = sensor(&mut events, 1, 150);
    assert!(matches!(
        emitted.as_slice(),
        [
            Event::FinishConditionReached { at: 110, .. },
            Event::LaneFinished { .. },
            Event::LaneFinished { .. },
            Event::RaceFinished { at: 110 }
        ]
    ));
    assert_eq!(
        RaceEngine::replay(&events).unwrap().state().finished_at(),
        Some(110)
    );
}
