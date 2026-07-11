use serde::{Deserialize, Serialize};

pub type ProtocolMillis = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishCondition {
    Laps(u32),
    TimeMs(ProtocolMillis),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishMode {
    Immediate,
    LeaderLap,
    AllCurrentLap,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaceConfig {
    pub lanes: u8,
    pub start_sequence_ms: ProtocolMillis,
    pub minimum_lap_time_ms: ProtocolMillis,
    pub finish_condition: FinishCondition,
    pub finish_mode: FinishMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalEdge {
    Rising,
    Falling,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    StartRace {
        config: RaceConfig,
        at: ProtocolMillis,
    },
    AdvanceRace {
        to: ProtocolMillis,
    },
    SensorTriggered {
        lane: u8,
        at: ProtocolMillis,
        edge: SignalEdge,
    },
    CorrectLaps {
        lane: u8,
        delta_thousandths: i64,
        at: ProtocolMillis,
    },
}

impl Command {
    pub fn timestamp(&self) -> ProtocolMillis {
        match self {
            Self::StartRace { at, .. }
            | Self::SensorTriggered { at, .. }
            | Self::CorrectLaps { at, .. } => *at,
            Self::AdvanceRace { to } => *to,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    Mindestrundenzeit,
    LaneAlreadyFinished,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    RaceConfigured {
        config: RaceConfig,
        at: ProtocolMillis,
    },
    StartSequenceStarted {
        due_at: ProtocolMillis,
        at: ProtocolMillis,
    },
    OfficialStart {
        at: ProtocolMillis,
    },
    MeasurementCaptured {
        lane: u8,
        at: ProtocolMillis,
        edge: SignalEdge,
    },
    MeasurementRejected {
        lane: u8,
        at: ProtocolMillis,
        reason: RejectionReason,
    },
    ValidLap {
        lane: u8,
        at: ProtocolMillis,
        lap_time_ms: ProtocolMillis,
    },
    FinishConditionReached {
        at: ProtocolMillis,
        leader_lane: u8,
    },
    LaneFinished {
        lane: u8,
        at: ProtocolMillis,
    },
    RaceFinished {
        at: ProtocolMillis,
    },
    LapCorrectionApplied {
        lane: u8,
        delta_thousandths: i64,
        at: ProtocolMillis,
    },
}

impl Event {
    pub fn timestamp(&self) -> ProtocolMillis {
        match self {
            Self::RaceConfigured { at, .. }
            | Self::StartSequenceStarted { at, .. }
            | Self::OfficialStart { at }
            | Self::MeasurementCaptured { at, .. }
            | Self::MeasurementRejected { at, .. }
            | Self::ValidLap { at, .. }
            | Self::FinishConditionReached { at, .. }
            | Self::LaneFinished { at, .. }
            | Self::RaceFinished { at }
            | Self::LapCorrectionApplied { at, .. } => *at,
        }
    }

    pub fn event_type(&self) -> &'static str {
        match self {
            Self::RaceConfigured { .. } => "race_configured",
            Self::StartSequenceStarted { .. } => "start_sequence_started",
            Self::OfficialStart { .. } => "official_start",
            Self::MeasurementCaptured { .. } => "measurement_captured",
            Self::MeasurementRejected { .. } => "measurement_rejected",
            Self::ValidLap { .. } => "valid_lap",
            Self::FinishConditionReached { .. } => "finish_condition_reached",
            Self::LaneFinished { .. } => "lane_finished",
            Self::RaceFinished { .. } => "race_finished",
            Self::LapCorrectionApplied { .. } => "lap_correction_applied",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RacePhase {
    #[default]
    Ready,
    Starting,
    Running,
    Finishing,
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneState {
    pub lane: u8,
    pub laps: u32,
    pub corrected_laps_thousandths: i64,
    pub last_lap_ms: Option<ProtocolMillis>,
    pub best_lap_ms: Option<ProtocolMillis>,
    pub last_valid_at: Option<ProtocolMillis>,
    pub finished_at: Option<ProtocolMillis>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub lane: u8,
    pub corrected_laps_thousandths: i64,
    pub result_time_ms: Option<ProtocolMillis>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaceState {
    pub phase: RacePhase,
    pub config: Option<RaceConfig>,
    pub start_due_at: Option<ProtocolMillis>,
    pub official_start_at: Option<ProtocolMillis>,
    pub finish_condition_at: Option<ProtocolMillis>,
    pub condition_leader: Option<u8>,
    pub finished_at: Option<ProtocolMillis>,
    pub last_event_at: Option<ProtocolMillis>,
    pub lanes: Vec<LaneState>,
    #[serde(skip)]
    pending_measurement: Option<(u8, ProtocolMillis)>,
}

impl Default for RaceState {
    fn default() -> Self {
        Self {
            phase: RacePhase::Ready,
            config: None,
            start_due_at: None,
            official_start_at: None,
            finish_condition_at: None,
            condition_leader: None,
            finished_at: None,
            last_event_at: None,
            lanes: vec![],
            pending_measurement: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainError {
    InvalidLaneCount,
    InvalidDuration,
    InvalidTarget,
    InvalidLane,
    InvalidPhase,
    TimestampBeforeLast {
        last: ProtocolMillis,
        command: ProtocolMillis,
    },
    InvalidEventOrder,
    NegativeCorrectedLaps,
}

#[derive(Clone, Debug, Default)]
pub struct RaceEngine {
    state: RaceState,
}

impl RaceEngine {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn state(&self) -> &RaceState {
        &self.state
    }

    pub fn replay(events: &[Event]) -> Result<Self, DomainError> {
        let mut state = RaceState::default();
        for event in events {
            apply_event(&mut state, event)?;
        }
        if state.pending_measurement.is_some() {
            return Err(DomainError::InvalidEventOrder);
        }
        Ok(Self { state })
    }

    pub fn handle(&self, command: Command) -> Result<Vec<Event>, DomainError> {
        if let Command::StartRace { config, at } = command {
            if self.state.phase != RacePhase::Ready {
                return Err(DomainError::InvalidPhase);
            }
            validate_config(&config)?;
            let due_at = at
                .checked_add(config.start_sequence_ms)
                .ok_or(DomainError::InvalidDuration)?;
            if let FinishCondition::TimeMs(duration) = config.finish_condition {
                due_at
                    .checked_add(duration)
                    .ok_or(DomainError::InvalidDuration)?;
            }
            return Ok(vec![
                Event::RaceConfigured { config, at },
                Event::StartSequenceStarted { due_at, at },
            ]);
        }
        let command_at = command.timestamp();
        if let Some(last) = self.state.last_event_at.filter(|last| command_at < *last) {
            return Err(DomainError::TimestampBeforeLast {
                last,
                command: command_at,
            });
        }
        if self.state.phase == RacePhase::Ready {
            return Err(DomainError::InvalidPhase);
        }

        let mut state = self.state.clone();
        let mut events = vec![];
        materialize_due(&mut state, command_at, &mut events)?;
        if state.phase == RacePhase::Finished {
            return match command {
                Command::CorrectLaps {
                    lane,
                    delta_thousandths,
                    at,
                } => {
                    let current = state
                        .lane(lane)
                        .ok_or(DomainError::InvalidLane)?
                        .corrected_laps_thousandths;
                    if current
                        .checked_add(delta_thousandths)
                        .is_none_or(|laps| laps < 0)
                    {
                        return Err(DomainError::NegativeCorrectedLaps);
                    }
                    push_event(
                        &mut state,
                        &mut events,
                        Event::LapCorrectionApplied {
                            lane,
                            delta_thousandths,
                            at,
                        },
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
                if matches!(state.phase, RacePhase::Running | RacePhase::Finishing) =>
            {
                handle_sensor(&mut state, &mut events, lane, at, edge)?;
                Ok(events)
            }
            _ => Err(DomainError::InvalidPhase),
        }
    }
}

fn handle_sensor(
    state: &mut RaceState,
    events: &mut Vec<Event>,
    lane: u8,
    at: ProtocolMillis,
    edge: SignalEdge,
) -> Result<(), DomainError> {
    if state.lane(lane).is_none() {
        return Err(DomainError::InvalidLane);
    }
    push_event(state, events, Event::MeasurementCaptured { lane, at, edge })?;
    if state.lane(lane).unwrap().finished_at.is_some() {
        return push_event(
            state,
            events,
            Event::MeasurementRejected {
                lane,
                at,
                reason: RejectionReason::LaneAlreadyFinished,
            },
        );
    }
    let reference = state
        .lane(lane)
        .unwrap()
        .last_valid_at
        .or(state.official_start_at)
        .unwrap();
    let elapsed = at - reference;
    if elapsed < state.config.as_ref().unwrap().minimum_lap_time_ms {
        return push_event(
            state,
            events,
            Event::MeasurementRejected {
                lane,
                at,
                reason: RejectionReason::Mindestrundenzeit,
            },
        );
    }
    push_event(
        state,
        events,
        Event::ValidLap {
            lane,
            at,
            lap_time_ms: elapsed,
        },
    )?;

    if state.phase == RacePhase::Running {
        if matches!(state.config.as_ref().unwrap().finish_condition, FinishCondition::Laps(target) if state.lane(lane).unwrap().laps >= target)
        {
            reach_finish_condition(state, events, at, true)?;
        }
    } else {
        match state.config.as_ref().unwrap().finish_mode {
            FinishMode::LeaderLap if state.condition_leader == Some(lane) => {
                finish_all(state, events, at)?
            }
            FinishMode::AllCurrentLap => {
                push_event(state, events, Event::LaneFinished { lane, at })?;
                if state.lanes.iter().all(|item| item.finished_at.is_some()) {
                    push_event(state, events, Event::RaceFinished { at })?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn push_event(
    state: &mut RaceState,
    events: &mut Vec<Event>,
    event: Event,
) -> Result<(), DomainError> {
    apply_event(state, &event)?;
    events.push(event);
    Ok(())
}

fn materialize_due(
    state: &mut RaceState,
    to: ProtocolMillis,
    events: &mut Vec<Event>,
) -> Result<(), DomainError> {
    if state.phase == RacePhase::Starting && state.start_due_at.is_some_and(|due| due <= to) {
        push_event(
            state,
            events,
            Event::OfficialStart {
                at: state.start_due_at.unwrap(),
            },
        )?;
    }
    if state.phase == RacePhase::Running
        && let FinishCondition::TimeMs(duration) = state.config.as_ref().unwrap().finish_condition
    {
        let due = state
            .official_start_at
            .unwrap()
            .checked_add(duration)
            .ok_or(DomainError::InvalidDuration)?;
        if due <= to {
            reach_finish_condition(state, events, due, false)?;
        }
    }
    Ok(())
}

fn reach_finish_condition(
    state: &mut RaceState,
    events: &mut Vec<Event>,
    at: ProtocolMillis,
    lap_trigger: bool,
) -> Result<(), DomainError> {
    let leader_lane = condition_leader(state);
    push_event(
        state,
        events,
        Event::FinishConditionReached { at, leader_lane },
    )?;
    let mode = state.config.as_ref().unwrap().finish_mode;
    if mode == FinishMode::Immediate || (mode == FinishMode::LeaderLap && lap_trigger) {
        finish_all(state, events, at)?;
    }
    Ok(())
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

fn finish_all(
    state: &mut RaceState,
    events: &mut Vec<Event>,
    at: ProtocolMillis,
) -> Result<(), DomainError> {
    let lanes: Vec<_> = state
        .lanes
        .iter()
        .filter(|lane| lane.finished_at.is_none())
        .map(|lane| lane.lane)
        .collect();
    for lane in lanes {
        push_event(state, events, Event::LaneFinished { lane, at })?;
    }
    push_event(state, events, Event::RaceFinished { at })
}

fn apply_event(state: &mut RaceState, event: &Event) -> Result<(), DomainError> {
    if state
        .last_event_at
        .is_some_and(|last| event.timestamp() < last)
    {
        return Err(DomainError::InvalidEventOrder);
    }
    match event {
        Event::RaceConfigured { config, .. }
            if state.phase == RacePhase::Ready && state.config.is_none() =>
        {
            validate_config(config)?;
            state.lanes = (1..=config.lanes)
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
            state.config = Some(config.clone());
        }
        Event::StartSequenceStarted { due_at, at }
            if state.phase == RacePhase::Ready
                && state.last_event_at == Some(*at)
                && state.config.as_ref().is_some_and(|config| {
                    at.checked_add(config.start_sequence_ms) == Some(*due_at)
                }) =>
        {
            state.phase = RacePhase::Starting;
            state.start_due_at = Some(*due_at);
        }
        Event::OfficialStart { at }
            if state.phase == RacePhase::Starting && state.start_due_at == Some(*at) =>
        {
            state.phase = RacePhase::Running;
            state.official_start_at = Some(*at);
        }
        Event::MeasurementCaptured { lane, at, .. }
            if matches!(state.phase, RacePhase::Running | RacePhase::Finishing)
                && state.lane(*lane).is_some()
                && state.pending_measurement.is_none() =>
        {
            state.pending_measurement = Some((*lane, *at));
        }
        Event::MeasurementRejected { lane, at, reason }
            if state.pending_measurement == Some((*lane, *at)) =>
        {
            let lane_state = state.lane(*lane).ok_or(DomainError::InvalidEventOrder)?;
            let reference = lane_state
                .last_valid_at
                .or(state.official_start_at)
                .ok_or(DomainError::InvalidEventOrder)?;
            let correct_reason = match reason {
                RejectionReason::Mindestrundenzeit => {
                    lane_state.finished_at.is_none()
                        && at.checked_sub(reference).is_some_and(|elapsed| {
                            elapsed < state.config.as_ref().unwrap().minimum_lap_time_ms
                        })
                }
                RejectionReason::LaneAlreadyFinished => lane_state.finished_at.is_some(),
            };
            if !correct_reason {
                return Err(DomainError::InvalidEventOrder);
            }
            state.pending_measurement = None;
        }
        Event::ValidLap {
            lane,
            at,
            lap_time_ms,
        } if matches!(state.phase, RacePhase::Running | RacePhase::Finishing)
            && state.pending_measurement == Some((*lane, *at)) =>
        {
            let lane_state = state.lane(*lane).ok_or(DomainError::InvalidEventOrder)?;
            let reference = lane_state
                .last_valid_at
                .or(state.official_start_at)
                .ok_or(DomainError::InvalidEventOrder)?;
            let expected = at
                .checked_sub(reference)
                .ok_or(DomainError::InvalidEventOrder)?;
            if lane_state.finished_at.is_some()
                || expected != *lap_time_ms
                || expected < state.config.as_ref().unwrap().minimum_lap_time_ms
            {
                return Err(DomainError::InvalidEventOrder);
            }
            let lane = state.lane_mut(*lane).unwrap();
            lane.laps = lane
                .laps
                .checked_add(1)
                .ok_or(DomainError::InvalidEventOrder)?;
            lane.corrected_laps_thousandths = lane
                .corrected_laps_thousandths
                .checked_add(1_000)
                .ok_or(DomainError::InvalidEventOrder)?;
            lane.last_lap_ms = Some(*lap_time_ms);
            lane.best_lap_ms = Some(
                lane.best_lap_ms
                    .map_or(*lap_time_ms, |best| best.min(*lap_time_ms)),
            );
            lane.last_valid_at = Some(*at);
            state.pending_measurement = None;
        }
        Event::FinishConditionReached { at, leader_lane }
            if state.phase == RacePhase::Running
                && state.pending_measurement.is_none()
                && *leader_lane == condition_leader(state)
                && finish_condition_reached(state, *at) =>
        {
            state.phase = RacePhase::Finishing;
            state.finish_condition_at = Some(*at);
            state.condition_leader = Some(*leader_lane);
        }
        Event::LaneFinished { lane, at } if state.phase == RacePhase::Finishing => {
            let lane = state
                .lane_mut(*lane)
                .ok_or(DomainError::InvalidEventOrder)?;
            if lane.finished_at.is_some() {
                return Err(DomainError::InvalidEventOrder);
            }
            lane.finished_at = Some(*at);
        }
        Event::RaceFinished { at }
            if state.phase == RacePhase::Finishing
                && state.lanes.iter().all(|lane| lane.finished_at.is_some()) =>
        {
            state.phase = RacePhase::Finished;
            state.finished_at = Some(*at);
        }
        Event::LapCorrectionApplied {
            lane,
            delta_thousandths,
            ..
        } if state.phase == RacePhase::Finished => {
            let lane = state
                .lane_mut(*lane)
                .ok_or(DomainError::InvalidEventOrder)?;
            lane.corrected_laps_thousandths = lane
                .corrected_laps_thousandths
                .checked_add(*delta_thousandths)
                .filter(|laps| *laps >= 0)
                .ok_or(DomainError::InvalidEventOrder)?;
        }
        _ => return Err(DomainError::InvalidEventOrder),
    }
    state.last_event_at = Some(event.timestamp());
    Ok(())
}

fn finish_condition_reached(state: &RaceState, at: ProtocolMillis) -> bool {
    match state.config.as_ref().unwrap().finish_condition {
        FinishCondition::Laps(target) => {
            state.last_event_at == Some(at) && state.lanes.iter().any(|lane| lane.laps >= target)
        }
        FinishCondition::TimeMs(duration) => {
            state
                .official_start_at
                .and_then(|start| start.checked_add(duration))
                == Some(at)
        }
    }
}

impl RaceState {
    pub fn lane(&self, lane: u8) -> Option<&LaneState> {
        self.lanes.iter().find(|item| item.lane == lane)
    }
    fn lane_mut(&mut self, lane: u8) -> Option<&mut LaneState> {
        self.lanes.iter_mut().find(|item| item.lane == lane)
    }

    pub fn standings(&self) -> Vec<Standing> {
        let start = self.official_start_at;
        let mut standings: Vec<_> = self
            .lanes
            .iter()
            .map(|lane| Standing {
                lane: lane.lane,
                corrected_laps_thousandths: lane.corrected_laps_thousandths,
                result_time_ms: lane
                    .finished_at
                    .zip(start)
                    .map(|(finish, start)| finish - start),
            })
            .collect();
        standings.sort_by_key(|standing| {
            (
                std::cmp::Reverse(standing.corrected_laps_thousandths),
                standing.result_time_ms.unwrap_or(ProtocolMillis::MAX),
                standing.lane,
            )
        });
        standings
    }
}

fn validate_config(config: &RaceConfig) -> Result<(), DomainError> {
    if !(1..=4).contains(&config.lanes) {
        return Err(DomainError::InvalidLaneCount);
    }
    if config.start_sequence_ms == 0 || config.minimum_lap_time_ms == 0 {
        return Err(DomainError::InvalidDuration);
    }
    match config.finish_condition {
        FinishCondition::Laps(0) | FinishCondition::TimeMs(0) => Err(DomainError::InvalidTarget),
        _ => Ok(()),
    }
}
