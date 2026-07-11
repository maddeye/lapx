use super::{
    Command, DomainError, Event, FinishCondition, FinishMode, LaneState, ProtocolMillis,
    RaceConfig, RacePhase, RaceState, RejectionReason, validate_config,
};

#[derive(Clone, Debug)]
enum Pending {
    Event(Event),
    Start { event: Event, config: RaceConfig },
}

impl Pending {
    fn event(&self) -> &Event {
        match self {
            Self::Event(event) | Self::Start { event, .. } => event,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RaceEngine {
    state: RaceState,
    pending: Option<Pending>,
}

impl RaceEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &RaceState {
        &self.state
    }

    pub fn replay(events: &[Event]) -> Result<Self, DomainError> {
        let mut engine = Self::new();
        for event in events {
            engine.accept(event)?;
        }
        engine.complete_replay()
    }

    pub fn replay_at(events: &[Event], at: ProtocolMillis) -> Result<Self, DomainError> {
        let mut engine = Self::new();
        for event in events.iter().take_while(|event| event.timestamp() <= at) {
            engine.accept(event)?;
        }
        engine.complete_replay()
    }

    pub fn handle(&self, command: Command) -> Result<Vec<Event>, DomainError> {
        if let Command::StartRace { config, at } = command {
            if !matches!(self.state.phase, RacePhase::Ready) {
                return Err(DomainError::InvalidPhase);
            }
            let mut engine = self.clone();
            let mut events = vec![];
            engine.emit(Event::RaceConfigured { config, at }, &mut events)?;
            return Ok(events);
        }

        let command_at = command.timestamp();
        if let Some(last) = self.state.last_event_at.filter(|last| command_at < *last) {
            return Err(DomainError::TimestampBeforeLast {
                last,
                command: command_at,
            });
        }
        if matches!(self.state.phase, RacePhase::Ready) {
            return Err(DomainError::InvalidPhase);
        }

        let mut engine = self.clone();
        let mut events = vec![];
        engine.materialize_due(command_at, &mut events)?;

        if matches!(engine.state.phase, RacePhase::Finished { .. }) {
            return match command {
                Command::CorrectLaps {
                    lane,
                    delta_thousandths,
                    at,
                } => {
                    let current = engine
                        .state
                        .lane(lane)
                        .ok_or(DomainError::InvalidLane)?
                        .corrected_laps_thousandths;
                    if current
                        .checked_add(delta_thousandths)
                        .is_none_or(|laps| laps < 0)
                    {
                        return Err(DomainError::NegativeCorrectedLaps);
                    }
                    engine.emit(
                        Event::LapCorrectionApplied {
                            lane,
                            delta_thousandths,
                            at,
                        },
                        &mut events,
                    )?;
                    Ok(events)
                }
                _ if !events.is_empty() => Ok(events),
                _ => Err(DomainError::InvalidPhase),
            };
        }

        match command {
            Command::AdvanceRace { .. } => Ok(events),
            Command::SensorTriggered { lane, at, edge }
                if matches!(
                    engine.state.phase,
                    RacePhase::Running { .. } | RacePhase::Finishing { .. }
                ) =>
            {
                if engine.state.lane(lane).is_none() {
                    return Err(DomainError::InvalidLane);
                }
                engine.emit(Event::MeasurementCaptured { lane, at, edge }, &mut events)?;
                Ok(events)
            }
            _ => Err(DomainError::InvalidPhase),
        }
    }

    fn complete_replay(self) -> Result<Self, DomainError> {
        if self.pending.is_some() {
            Err(DomainError::InvalidEventOrder)
        } else {
            Ok(self)
        }
    }

    fn emit(&mut self, event: Event, events: &mut Vec<Event>) -> Result<(), DomainError> {
        self.accept(&event)?;
        events.push(event);
        while let Some(event) = self.pending.as_ref().map(|pending| pending.event().clone()) {
            self.accept(&event)?;
            events.push(event);
        }
        Ok(())
    }

    fn materialize_due(
        &mut self,
        to: ProtocolMillis,
        events: &mut Vec<Event>,
    ) -> Result<(), DomainError> {
        while let Some(event) = self.due_event(to)? {
            self.emit(event, events)?;
        }
        Ok(())
    }

    fn accept(&mut self, event: &Event) -> Result<(), DomainError> {
        if self
            .state
            .last_event_at
            .is_some_and(|last| event.timestamp() < last)
        {
            return Err(DomainError::InvalidEventOrder);
        }

        if let Some(pending) = self.pending.take() {
            if pending.event() != event {
                return Err(DomainError::InvalidEventOrder);
            }
            self.accept_expected(event, pending)?;
        } else {
            if self
                .due_event(event.timestamp())?
                .is_some_and(|due| due != *event)
            {
                return Err(DomainError::InvalidEventOrder);
            }
            self.accept_root(event)?;
        }
        self.state.last_event_at = Some(event.timestamp());
        Ok(())
    }

    fn accept_root(&mut self, event: &Event) -> Result<(), DomainError> {
        match event {
            Event::RaceConfigured { config, at }
                if matches!(self.state.phase, RacePhase::Ready)
                    && self.state.last_event_at.is_none() =>
            {
                validate_config(config)?;
                let due_at = at
                    .checked_add(config.start_sequence_ms)
                    .ok_or(DomainError::InvalidDuration)?;
                if let FinishCondition::TimeMs(duration) = config.finish_condition {
                    due_at
                        .checked_add(duration)
                        .ok_or(DomainError::InvalidDuration)?;
                }
                self.pending = Some(Pending::Start {
                    event: Event::StartSequenceStarted { due_at, at: *at },
                    config: config.clone(),
                });
            }
            Event::OfficialStart { .. }
                if self.due_event(event.timestamp())? == Some(event.clone()) =>
            {
                self.start_official(event)?;
            }
            Event::FinishConditionReached { .. }
                if self.due_event(event.timestamp())? == Some(event.clone()) =>
            {
                self.start_finishing(event)?;
            }
            Event::MeasurementCaptured { lane, at, .. }
                if matches!(
                    self.state.phase,
                    RacePhase::Running { .. } | RacePhase::Finishing { .. }
                ) =>
            {
                self.expect_measurement_result(*lane, *at)?;
            }
            Event::LapCorrectionApplied {
                lane,
                delta_thousandths,
                ..
            } if matches!(self.state.phase, RacePhase::Finished { .. }) => {
                let lane = self
                    .state
                    .lane_mut(*lane)
                    .ok_or(DomainError::InvalidEventOrder)?;
                lane.corrected_laps_thousandths = lane
                    .corrected_laps_thousandths
                    .checked_add(*delta_thousandths)
                    .filter(|laps| *laps >= 0)
                    .ok_or(DomainError::NegativeCorrectedLaps)?;
            }
            _ => return Err(DomainError::InvalidEventOrder),
        }
        Ok(())
    }

    fn accept_expected(&mut self, event: &Event, pending: Pending) -> Result<(), DomainError> {
        match event {
            Event::StartSequenceStarted { due_at, .. } => {
                let Pending::Start { config, .. } = pending else {
                    return Err(DomainError::InvalidEventOrder);
                };
                self.state.lanes = (1..=config.lanes)
                    .map(|lane| LaneState {
                        lane,
                        laps: 0,
                        corrected_laps_thousandths: 0,
                        last_lap_ms: None,
                        best_lap_ms: None,
                        last_valid_at: None,
                        finished_at: None,
                    })
                    .collect();
                self.state.phase = RacePhase::Starting {
                    config,
                    start_due_at: *due_at,
                };
            }
            Event::MeasurementRejected { .. } => {}
            Event::ValidLap {
                lane,
                at,
                lap_time_ms,
            } => self.apply_valid_lap(*lane, *at, *lap_time_ms)?,
            Event::FinishConditionReached { .. } => self.start_finishing(event)?,
            Event::LaneFinished { lane, at } => self.finish_lane(*lane, *at)?,
            Event::RaceFinished { at } => self.finish_race(*at)?,
            _ => return Err(DomainError::InvalidEventOrder),
        }
        Ok(())
    }

    fn due_event(&self, to: ProtocolMillis) -> Result<Option<Event>, DomainError> {
        match &self.state.phase {
            RacePhase::Starting { start_due_at, .. } if *start_due_at <= to => {
                Ok(Some(Event::OfficialStart { at: *start_due_at }))
            }
            RacePhase::Running {
                config,
                official_start_at,
            } => {
                let FinishCondition::TimeMs(duration) = config.finish_condition else {
                    return Ok(None);
                };
                let at = official_start_at
                    .checked_add(duration)
                    .ok_or(DomainError::InvalidDuration)?;
                Ok((at <= to).then(|| Event::FinishConditionReached {
                    at,
                    leader_lane: condition_leader(&self.state),
                }))
            }
            _ => Ok(None),
        }
    }

    fn start_official(&mut self, event: &Event) -> Result<(), DomainError> {
        let Event::OfficialStart { at } = event else {
            return Err(DomainError::InvalidEventOrder);
        };
        let RacePhase::Starting {
            config,
            start_due_at,
        } = &self.state.phase
        else {
            return Err(DomainError::InvalidEventOrder);
        };
        if at != start_due_at {
            return Err(DomainError::InvalidEventOrder);
        }
        self.state.phase = RacePhase::Running {
            config: config.clone(),
            official_start_at: *at,
        };
        Ok(())
    }

    fn expect_measurement_result(
        &mut self,
        lane: u8,
        at: ProtocolMillis,
    ) -> Result<(), DomainError> {
        let lane_state = self
            .state
            .lane(lane)
            .ok_or(DomainError::InvalidEventOrder)?;
        let event = if lane_state.finished_at.is_some() {
            Event::MeasurementRejected {
                lane,
                at,
                reason: RejectionReason::LaneAlreadyFinished,
            }
        } else {
            let reference = lane_state
                .last_valid_at
                .or(self.state.official_start_at())
                .ok_or(DomainError::InvalidEventOrder)?;
            let elapsed = at
                .checked_sub(reference)
                .ok_or(DomainError::InvalidEventOrder)?;
            if elapsed < self.state.config().unwrap().minimum_lap_time_ms {
                Event::MeasurementRejected {
                    lane,
                    at,
                    reason: RejectionReason::Mindestrundenzeit,
                }
            } else {
                Event::ValidLap {
                    lane,
                    at,
                    lap_time_ms: elapsed,
                }
            }
        };
        self.pending = Some(Pending::Event(event));
        Ok(())
    }

    fn apply_valid_lap(
        &mut self,
        lane: u8,
        at: ProtocolMillis,
        lap_time_ms: ProtocolMillis,
    ) -> Result<(), DomainError> {
        let lane_state = self
            .state
            .lane_mut(lane)
            .ok_or(DomainError::InvalidEventOrder)?;
        lane_state.laps = lane_state
            .laps
            .checked_add(1)
            .ok_or(DomainError::InvalidEventOrder)?;
        lane_state.corrected_laps_thousandths = lane_state
            .corrected_laps_thousandths
            .checked_add(1_000)
            .ok_or(DomainError::InvalidEventOrder)?;
        lane_state.last_lap_ms = Some(lap_time_ms);
        lane_state.best_lap_ms = Some(
            lane_state
                .best_lap_ms
                .map_or(lap_time_ms, |best| best.min(lap_time_ms)),
        );
        lane_state.last_valid_at = Some(at);

        match &self.state.phase {
            RacePhase::Running { config, .. } if matches!(config.finish_condition, FinishCondition::Laps(target) if self.state.lane(lane).unwrap().laps >= target) =>
            {
                self.pending = Some(Pending::Event(Event::FinishConditionReached {
                    at,
                    leader_lane: condition_leader(&self.state),
                }));
            }
            RacePhase::Finishing {
                config,
                condition_leader,
                ..
            } if config.finish_mode == FinishMode::LeaderLap && *condition_leader == lane => {
                self.expect_first_unfinished(at)?;
            }
            RacePhase::Finishing { config, .. }
                if config.finish_mode == FinishMode::AllCurrentLap =>
            {
                self.pending = Some(Pending::Event(Event::LaneFinished { lane, at }));
            }
            _ => {}
        }
        Ok(())
    }

    fn start_finishing(&mut self, event: &Event) -> Result<(), DomainError> {
        let Event::FinishConditionReached { at, leader_lane } = event else {
            return Err(DomainError::InvalidEventOrder);
        };
        let RacePhase::Running {
            config,
            official_start_at,
        } = &self.state.phase
        else {
            return Err(DomainError::InvalidEventOrder);
        };
        let config = config.clone();
        let official_start_at = *official_start_at;
        if *leader_lane != condition_leader(&self.state) {
            return Err(DomainError::InvalidEventOrder);
        }
        self.state.phase = RacePhase::Finishing {
            config: config.clone(),
            official_start_at,
            finish_condition_at: *at,
            condition_leader: *leader_lane,
        };

        match config.finish_mode {
            FinishMode::Immediate => self.expect_first_unfinished(*at)?,
            FinishMode::LeaderLap
                if matches!(config.finish_condition, FinishCondition::Laps(_)) =>
            {
                self.expect_first_unfinished(*at)?;
            }
            FinishMode::AllCurrentLap
                if matches!(config.finish_condition, FinishCondition::Laps(_)) =>
            {
                self.pending = Some(Pending::Event(Event::LaneFinished {
                    lane: *leader_lane,
                    at: *at,
                }));
            }
            _ => {}
        }
        Ok(())
    }

    fn expect_first_unfinished(&mut self, at: ProtocolMillis) -> Result<(), DomainError> {
        let lane = self
            .state
            .lanes
            .iter()
            .find(|lane| lane.finished_at.is_none())
            .ok_or(DomainError::InvalidEventOrder)?
            .lane;
        self.pending = Some(Pending::Event(Event::LaneFinished { lane, at }));
        Ok(())
    }

    fn finish_lane(&mut self, lane: u8, at: ProtocolMillis) -> Result<(), DomainError> {
        let lane_state = self
            .state
            .lane_mut(lane)
            .ok_or(DomainError::InvalidEventOrder)?;
        if lane_state.finished_at.replace(at).is_some() {
            return Err(DomainError::InvalidEventOrder);
        }

        if self
            .state
            .lanes
            .iter()
            .all(|lane| lane.finished_at.is_some())
        {
            self.pending = Some(Pending::Event(Event::RaceFinished { at }));
        } else if matches!(
            self.state.finish_mode(),
            Some(FinishMode::Immediate | FinishMode::LeaderLap)
        ) {
            self.expect_first_unfinished(at)?;
        }
        Ok(())
    }

    fn finish_race(&mut self, at: ProtocolMillis) -> Result<(), DomainError> {
        if !self
            .state
            .lanes
            .iter()
            .all(|lane| lane.finished_at.is_some())
        {
            return Err(DomainError::InvalidEventOrder);
        }
        let RacePhase::Finishing {
            config,
            official_start_at,
            finish_condition_at,
            condition_leader,
        } = &self.state.phase
        else {
            return Err(DomainError::InvalidEventOrder);
        };
        self.state.phase = RacePhase::Finished {
            config: config.clone(),
            official_start_at: *official_start_at,
            finish_condition_at: *finish_condition_at,
            condition_leader: *condition_leader,
            finished_at: at,
        };
        Ok(())
    }
}

fn condition_leader(state: &RaceState) -> u8 {
    state
        .lanes
        .iter()
        .min_by_key(|lane| {
            (
                std::cmp::Reverse(lane.laps),
                lane.last_valid_at.unwrap_or(ProtocolMillis::MAX),
                lane.lane,
            )
        })
        .unwrap()
        .lane
}
