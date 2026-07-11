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
    assert!(matches!(
        &first.state().phase,
        RacePhase::Starting {
            start_due_at: 1_100,
            ..
        }
    ));
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

fn running_events(config: RaceConfig) -> Vec<Event> {
    let mut events = RaceEngine::new()
        .handle(Command::StartRace { config, at: 0 })
        .unwrap();
    let starting = RaceEngine::replay(&events).unwrap();
    events.extend(starting.handle(Command::AdvanceRace { to: 1_000 }).unwrap());
    events
}

#[test]
fn replay_rejects_skipped_due_events_before_later_timestamps() {
    let mut skipped_start = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(),
            at: 0,
        })
        .unwrap();
    skipped_start.push(Event::MeasurementCaptured {
        lane: 1,
        at: 4_000,
        edge: SignalEdge::Rising,
    });
    assert_eq!(
        RaceEngine::replay(&skipped_start).unwrap_err(),
        DomainError::InvalidEventOrder
    );

    let mut timed = config();
    timed.finish_condition = FinishCondition::TimeMs(100);
    let mut skipped_finish = running_events(timed);
    skipped_finish.push(Event::MeasurementCaptured {
        lane: 1,
        at: 1_101,
        edge: SignalEdge::Rising,
    });
    assert_eq!(
        RaceEngine::replay(&skipped_finish).unwrap_err(),
        DomainError::InvalidEventOrder
    );
}

#[test]
fn replay_rejects_incomplete_measurement_and_lap_target_chains() {
    let mut captured_only = running_events(config());
    captured_only.push(Event::MeasurementCaptured {
        lane: 1,
        at: 4_000,
        edge: SignalEdge::Rising,
    });
    assert_eq!(
        RaceEngine::replay(&captured_only).unwrap_err(),
        DomainError::InvalidEventOrder
    );

    let mut lap_config = config();
    lap_config.finish_condition = FinishCondition::Laps(1);
    lap_config.finish_mode = FinishMode::AllCurrentLap;
    let mut missing_finish_condition = running_events(lap_config);
    missing_finish_condition.extend([
        Event::MeasurementCaptured {
            lane: 1,
            at: 4_000,
            edge: SignalEdge::Rising,
        },
        Event::ValidLap {
            lane: 1,
            at: 4_000,
            lap_time_ms: 3_000,
        },
    ]);
    assert_eq!(
        RaceEngine::replay(&missing_finish_condition).unwrap_err(),
        DomainError::InvalidEventOrder
    );
}

#[test]
fn replay_rejects_mode_invalid_lane_finish_and_incomplete_finish_chain() {
    let mut timed = config();
    timed.finish_condition = FinishCondition::TimeMs(100);
    timed.finish_mode = FinishMode::AllCurrentLap;
    let mut invalid_lane_finish = running_events(timed);
    invalid_lane_finish.push(Event::FinishConditionReached {
        at: 1_100,
        leader_lane: 1,
    });
    invalid_lane_finish.push(Event::LaneFinished { lane: 1, at: 1_100 });
    assert_eq!(
        RaceEngine::replay(&invalid_lane_finish).unwrap_err(),
        DomainError::InvalidEventOrder
    );

    let mut immediate = config();
    immediate.finish_condition = FinishCondition::Laps(1);
    let mut incomplete = running_events(immediate);
    let running = RaceEngine::replay(&incomplete).unwrap();
    let mut emitted = running
        .handle(Command::SensorTriggered {
            lane: 1,
            at: 4_000,
            edge: SignalEdge::Rising,
        })
        .unwrap();
    assert!(matches!(emitted.pop(), Some(Event::RaceFinished { .. })));
    incomplete.extend(emitted);
    assert_eq!(
        RaceEngine::replay(&incomplete).unwrap_err(),
        DomainError::InvalidEventOrder
    );
}

#[test]
fn lifecycle_data_is_carried_by_typed_phase_variants() {
    let events = RaceEngine::new()
        .handle(Command::StartRace {
            config: config(),
            at: 100,
        })
        .unwrap();
    let state = RaceEngine::replay(&events).unwrap();
    let RacePhase::Starting {
        config,
        start_due_at,
    } = &state.state().phase
    else {
        panic!("expected starting phase");
    };
    assert_eq!(config.lanes, 2);
    assert_eq!(*start_due_at, 1_100);

    let json = serde_json::to_value(state.state()).unwrap();
    assert_eq!(json["phase"], "starting");
    assert_eq!(json["start_due_at"], 1_100);
    assert!(json.get("official_start_at").is_none());
    assert!(json.get("finished_at").is_none());
}
