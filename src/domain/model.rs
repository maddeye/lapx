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

pub(crate) fn validate_config(config: &RaceConfig) -> Result<(), DomainError> {
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
