use super::{
    ChaosSource, Command, Consequence, DomainError, Event, FinishCondition, FinishMode, LaneState,
    ProtocolMillis, RaceConfig, RacePhase, RaceState, RejectionReason, SuspendedPhase,
    validate_config,
};

#[derive(Clone, Debug)]
enum Pending {
    Event(Event),
    Start {
        event: Event,
        config: RaceConfig,
    },
    FalseStart {
        event: Event,
        consequence: Event,
    },
    Pause {
        event: Event,
        consequence: Option<Event>,
    },
}

impl Pending {
    fn event(&self) -> &Event {
        match self {
            Self::Event(event)
            | Self::Start { event, .. }
            | Self::FalseStart { event, .. }
            | Self::Pause { event, .. } => event,
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
        if matches!(engine.state.phase, RacePhase::Aborted { .. }) {
            return if events.is_empty() {
                Err(DomainError::InvalidPhase)
            } else {
                Ok(events)
            };
        }

        match command {
            Command::AdvanceRace { .. } => Ok(events),
            Command::SensorTriggered { lane, at, edge } if engine.accepts_measurement() => {
                if engine.state.lane(lane).is_none() {
                    return Err(DomainError::InvalidLane);
                }
                engine.emit(Event::MeasurementCaptured { lane, at, edge }, &mut events)?;
                Ok(events)
            }
            Command::PauseRace { at } if engine.state.phase.active().is_some() => {
                engine.emit(Event::RacePaused { at }, &mut events)?;
                Ok(events)
            }
            Command::ResumeRace { at }
                if matches!(engine.state.phase, RacePhase::Paused { .. }) =>
            {
                let due_at = at
                    .checked_add(engine.state.config().unwrap().restart_sequence_ms)
                    .ok_or(DomainError::InvalidDuration)?;
                engine.emit(Event::RestartSequencePlanned { due_at, at }, &mut events)?;
                Ok(events)
            }
            Command::TriggerChaos { source, at } if engine.state.phase.active().is_some() => {
                if let ChaosSource::Lane(lane) = source
                    && engine.state.lane(lane).is_none()
                {
                    return Err(DomainError::InvalidLane);
                }
                engine.emit(Event::ChaosTriggered { source, at }, &mut events)?;
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
            Event::RaceResumed { .. }
                if self.due_event(event.timestamp())? == Some(event.clone()) =>
            {
                self.resume(event.timestamp())?;
            }
            Event::LanePowerOffExpired { lane, .. }
                if self.due_event(event.timestamp())? == Some(event.clone()) =>
            {
                let lane = self
                    .state
                    .lane_mut(*lane)
                    .ok_or(DomainError::InvalidEventOrder)?;
                if lane.power_off_until.take() != Some(event.timestamp()) {
                    return Err(DomainError::InvalidEventOrder);
                }
            }
            Event::FinishConditionReached { .. }
                if self.due_event(event.timestamp())? == Some(event.clone()) =>
            {
                self.start_finishing(event)?;
            }
            Event::MeasurementCaptured { lane, at, .. } if self.accepts_measurement() => {
                if self.measurement_is_false_start() {
                    self.expect_false_start(*lane, *at)?;
                } else {
                    self.expect_measurement_result(*lane, *at)?;
                }
            }
            Event::RacePaused { at } if self.state.phase.active().is_some() => {
                self.pause(*at)?;
            }
            Event::RestartSequencePlanned { due_at, at }
                if matches!(self.state.phase, RacePhase::Paused { .. })
                    && at.checked_add(self.state.config().unwrap().restart_sequence_ms)
                        == Some(*due_at) =>
            {
                let RacePhase::Paused {
                    suspended,
                    paused_at,
                } = self.state.phase.clone()
                else {
                    unreachable!()
                };
                self.state.phase = RacePhase::Restarting {
                    suspended,
                    paused_at,
                    restart_due_at: *due_at,
                };
            }
            Event::ChaosTriggered { source, at } if self.state.phase.active().is_some() => {
                let consequence = match source {
                    ChaosSource::RaceControl => None,
                    ChaosSource::Lane(lane) if self.state.lane(*lane).is_some() => {
                        Some(self.consequence_event(
                            *lane,
                            self.state.config().unwrap().chaos_consequence,
                            *at,
                        )?)
                    }
                    ChaosSource::Lane(_) => return Err(DomainError::InvalidEventOrder),
                };
                self.pending = Some(Pending::Pause {
                    event: Event::RacePaused { at: *at },
                    consequence,
                });
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
        match (event, pending) {
            (Event::StartSequenceStarted { due_at, .. }, Pending::Start { config, .. }) => {
                self.state.lanes = (1..=config.lanes)
                    .map(|lane| LaneState {
                        lane,
                        laps: 0,
                        corrected_laps_thousandths: 0,
                        last_lap_ms: None,
                        best_lap_ms: None,
                        last_valid_at: None,
                        finished_at: None,
                        race_time_ms: None,
                        result_time_penalty_ms: 0,
                        power_off_until: None,
                    })
                    .collect();
                self.state.phase = RacePhase::Starting {
                    config,
                    start_due_at: *due_at,
                };
            }
            (Event::FalseStartDetected { .. }, Pending::FalseStart { consequence, .. }) => {
                self.pending = Some(Pending::Event(consequence))
            }
            (Event::RacePaused { at }, Pending::Pause { consequence, .. }) => {
                self.pause(*at)?;
                self.pending = consequence.map(Pending::Event);
            }
            (
                Event::ConsequenceApplied {
                    lane,
                    consequence,
                    at,
                },
                Pending::Event(_),
            ) => {
                self.apply_consequence(*lane, consequence, *at)?;
            }
            (Event::MeasurementRejected { .. }, Pending::Event(_)) => {}
            (
                Event::ValidLap {
                    lane,
                    at,
                    lap_time_ms,
                },
                Pending::Event(_),
            ) => self.apply_valid_lap(*lane, *at, *lap_time_ms)?,
            (Event::FinishConditionReached { .. }, Pending::Event(_)) => {
                self.start_finishing(event)?
            }
            (Event::LaneFinished { lane, at }, Pending::Event(_)) => {
                self.finish_lane(*lane, *at)?
            }
            (Event::RaceFinished { at }, Pending::Event(_)) => self.finish_race(*at)?,
            _ => return Err(DomainError::InvalidEventOrder),
        }
        Ok(())
    }

    fn due_event(&self, to: ProtocolMillis) -> Result<Option<Event>, DomainError> {
        if matches!(
            self.state.phase,
            RacePhase::Finished { .. } | RacePhase::Aborted { .. }
        ) {
            return Ok(None);
        }
        let mut due = vec![];
        match &self.state.phase {
            RacePhase::Starting { start_due_at, .. } => {
                due.push(Event::OfficialStart { at: *start_due_at });
            }
            RacePhase::Running {
                config,
                official_start_at,
                paused_ms,
            } => {
                if let FinishCondition::TimeMs(duration) = config.finish_condition {
                    let at = official_start_at
                        .checked_add(duration)
                        .and_then(|at| at.checked_add(*paused_ms))
                        .ok_or(DomainError::InvalidDuration)?;
                    due.push(Event::FinishConditionReached {
                        at,
                        leader_lane: condition_leader(&self.state),
                    });
                }
            }
            RacePhase::Restarting { restart_due_at, .. } => {
                due.push(Event::RaceResumed {
                    at: *restart_due_at,
                });
            }
            _ => {}
        }
        due.extend(self.state.lanes.iter().filter_map(|lane| {
            lane.power_off_until.map(|at| Event::LanePowerOffExpired {
                lane: lane.lane,
                at,
            })
        }));
        due.retain(|event| event.timestamp() <= to);
        due.sort_by_key(|event| (event.timestamp(), event.event_type(), event.lane()));
        Ok(due.into_iter().next())
    }

    fn accepts_measurement(&self) -> bool {
        matches!(
            self.state.phase,
            RacePhase::Starting { .. }
                | RacePhase::Running { .. }
                | RacePhase::Finishing { .. }
                | RacePhase::Paused { .. }
        )
    }

    fn measurement_is_false_start(&self) -> bool {
        matches!(self.active_phase(), Some(SuspendedPhase::Starting { .. }))
    }

    fn active_phase(&self) -> Option<SuspendedPhase> {
        match &self.state.phase {
            RacePhase::Paused { suspended, .. } | RacePhase::Restarting { suspended, .. } => {
                Some(suspended.clone())
            }
            phase => phase.active(),
        }
    }

    fn set_active_phase(&mut self, active: SuspendedPhase) -> Result<(), DomainError> {
        match &mut self.state.phase {
            RacePhase::Paused { suspended, .. } | RacePhase::Restarting { suspended, .. } => {
                *suspended = active;
            }
            phase if phase.active().is_some() => *phase = RacePhase::from_active(active),
            _ => return Err(DomainError::InvalidEventOrder),
        }
        Ok(())
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
            paused_ms: 0,
        };
        Ok(())
    }

    fn expect_false_start(&mut self, lane: u8, at: ProtocolMillis) -> Result<(), DomainError> {
        if self.state.lane(lane).is_none() {
            return Err(DomainError::InvalidEventOrder);
        }
        let consequence = self.consequence_event(
            lane,
            self.state.config().unwrap().false_start_consequence,
            at,
        )?;
        self.pending = Some(Pending::FalseStart {
            event: Event::FalseStartDetected { lane, at },
            consequence,
        });
        Ok(())
    }

    fn consequence_event(
        &self,
        lane: u8,
        consequence: Consequence,
        at: ProtocolMillis,
    ) -> Result<Event, DomainError> {
        if let Consequence::LanePowerOffMs(duration) = consequence {
            at.checked_add(duration)
                .ok_or(DomainError::InvalidDuration)?;
            Ok(Event::ConsequenceApplied {
                lane,
                consequence: Consequence::LanePowerOffMs(duration),
                at,
            })
        } else {
            Ok(Event::ConsequenceApplied {
                lane,
                consequence,
                at,
            })
        }
    }

    fn apply_consequence(
        &mut self,
        lane: u8,
        consequence: &Consequence,
        at: ProtocolMillis,
    ) -> Result<(), DomainError> {
        if self.state.lane(lane).is_none() {
            return Err(DomainError::InvalidEventOrder);
        }
        match consequence {
            Consequence::Abort => {
                let config = self.state.config().unwrap().clone();
                self.state.phase = RacePhase::Aborted {
                    config,
                    aborted_at: at,
                };
            }
            Consequence::ResultTimePenaltyMs(penalty) => {
                let lane = self.state.lane_mut(lane).unwrap();
                let total = lane
                    .result_time_penalty_ms
                    .checked_add(*penalty)
                    .ok_or(DomainError::InvalidDuration)?;
                if lane
                    .race_time_ms
                    .is_some_and(|time| time.checked_add(total).is_none())
                {
                    return Err(DomainError::InvalidDuration);
                }
                lane.result_time_penalty_ms = total;
            }
            Consequence::LanePowerOffMs(duration) => {
                let until = at
                    .checked_add(*duration)
                    .ok_or(DomainError::InvalidDuration)?;
                let lane = self.state.lane_mut(lane).unwrap();
                lane.power_off_until =
                    Some(lane.power_off_until.map_or(until, |old| old.max(until)));
            }
        }
        Ok(())
    }

    fn pause(&mut self, at: ProtocolMillis) -> Result<(), DomainError> {
        let suspended = self
            .state
            .phase
            .active()
            .ok_or(DomainError::InvalidEventOrder)?;
        self.state.phase = RacePhase::Paused {
            suspended,
            paused_at: at,
        };
        Ok(())
    }

    fn resume(&mut self, at: ProtocolMillis) -> Result<(), DomainError> {
        let RacePhase::Restarting {
            suspended,
            paused_at,
            restart_due_at,
        } = self.state.phase.clone()
        else {
            return Err(DomainError::InvalidEventOrder);
        };
        if at != restart_due_at {
            return Err(DomainError::InvalidEventOrder);
        }
        let paused_duration = at
            .checked_sub(paused_at)
            .ok_or(DomainError::InvalidEventOrder)?;
        let active = suspended
            .shifted(paused_duration)
            .map_err(|()| DomainError::InvalidDuration)?;
        if let SuspendedPhase::Running {
            config,
            official_start_at,
            paused_ms,
        } = &active
            && let FinishCondition::TimeMs(duration) = config.finish_condition
        {
            official_start_at
                .checked_add(duration)
                .and_then(|due| due.checked_add(*paused_ms))
                .ok_or(DomainError::InvalidDuration)?;
        }
        self.state.phase = RacePhase::from_active(active);
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

        match self.active_phase() {
            Some(SuspendedPhase::Running { config, .. }) if matches!(config.finish_condition, FinishCondition::Laps(target) if self.state.lane(lane).unwrap().laps >= target) =>
            {
                self.pending = Some(Pending::Event(Event::FinishConditionReached {
                    at,
                    leader_lane: condition_leader(&self.state),
                }));
            }
            Some(SuspendedPhase::Finishing {
                config,
                condition_leader,
                ..
            }) if config.finish_mode == FinishMode::LeaderLap && condition_leader == lane => {
                self.expect_first_unfinished(at)?;
            }
            Some(SuspendedPhase::Finishing { config, .. })
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
        let Some(SuspendedPhase::Running {
            config,
            official_start_at,
            paused_ms,
        }) = self.active_phase()
        else {
            return Err(DomainError::InvalidEventOrder);
        };
        if *leader_lane != condition_leader(&self.state) {
            return Err(DomainError::InvalidEventOrder);
        }
        self.set_active_phase(SuspendedPhase::Finishing {
            config: config.clone(),
            official_start_at,
            paused_ms,
            finish_condition_at: *at,
            condition_leader: *leader_lane,
        })?;

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
        let race_time_ms = self
            .state
            .race_elapsed_ms(at)
            .ok_or(DomainError::InvalidEventOrder)?;
        let lane_state = self
            .state
            .lane_mut(lane)
            .ok_or(DomainError::InvalidEventOrder)?;
        if lane_state.finished_at.replace(at).is_some() {
            return Err(DomainError::InvalidEventOrder);
        }
        race_time_ms
            .checked_add(lane_state.result_time_penalty_ms)
            .ok_or(DomainError::InvalidDuration)?;
        lane_state.race_time_ms = Some(race_time_ms);

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
        let Some(SuspendedPhase::Finishing {
            config,
            official_start_at,
            mut paused_ms,
            finish_condition_at,
            condition_leader,
        }) = self.active_phase()
        else {
            return Err(DomainError::InvalidEventOrder);
        };
        if let RacePhase::Paused { paused_at, .. } = self.state.phase {
            paused_ms = paused_ms
                .checked_add(
                    at.checked_sub(paused_at)
                        .ok_or(DomainError::InvalidEventOrder)?,
                )
                .ok_or(DomainError::InvalidDuration)?;
        }
        self.state.phase = RacePhase::Finished {
            config,
            official_start_at,
            paused_ms,
            finish_condition_at,
            condition_leader,
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
