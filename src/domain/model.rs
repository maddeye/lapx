use serde::{Deserialize, Deserializer, Serialize};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consequence {
    Abort,
    ResultTimePenaltyMs(ProtocolMillis),
    LanePowerOffMs(ProtocolMillis),
}

impl Consequence {
    pub(crate) fn duration(self) -> Option<ProtocolMillis> {
        match self {
            Self::Abort => None,
            Self::ResultTimePenaltyMs(duration) | Self::LanePowerOffMs(duration) => Some(duration),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RaceConfig {
    pub lanes: u8,
    pub driver_ids: Vec<Option<i64>>,
    pub start_sequence_ms: ProtocolMillis,
    pub restart_sequence_ms: ProtocolMillis,
    pub minimum_lap_time_ms: ProtocolMillis,
    pub finish_condition: FinishCondition,
    pub finish_mode: FinishMode,
    pub false_start_consequence: Consequence,
    pub chaos_consequence: Consequence,
}

impl<'de> Deserialize<'de> for RaceConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct CompatibleConfig {
            lanes: u8,
            #[serde(default)]
            driver_ids: Option<Vec<Option<i64>>>,
            start_sequence_ms: ProtocolMillis,
            restart_sequence_ms: Option<ProtocolMillis>,
            minimum_lap_time_ms: ProtocolMillis,
            finish_condition: FinishCondition,
            finish_mode: FinishMode,
            false_start_consequence: Option<Consequence>,
            chaos_consequence: Option<Consequence>,
        }

        let value = CompatibleConfig::deserialize(deserializer)?;
        Ok(Self {
            lanes: value.lanes,
            driver_ids: value
                .driver_ids
                .unwrap_or_else(|| vec![None; value.lanes as usize]),
            start_sequence_ms: value.start_sequence_ms,
            restart_sequence_ms: value.restart_sequence_ms.unwrap_or(value.start_sequence_ms),
            minimum_lap_time_ms: value.minimum_lap_time_ms,
            finish_condition: value.finish_condition,
            finish_mode: value.finish_mode,
            false_start_consequence: value.false_start_consequence.unwrap_or(Consequence::Abort),
            chaos_consequence: value.chaos_consequence.unwrap_or(Consequence::Abort),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalEdge {
    Rising,
    Falling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChaosSource {
    RaceControl,
    Lane(u8),
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
    PauseRace {
        at: ProtocolMillis,
    },
    ResumeRace {
        at: ProtocolMillis,
    },
    TriggerChaos {
        source: ChaosSource,
        at: ProtocolMillis,
    },
}

impl Command {
    pub fn timestamp(&self) -> ProtocolMillis {
        match self {
            Self::StartRace { at, .. }
            | Self::SensorTriggered { at, .. }
            | Self::CorrectLaps { at, .. }
            | Self::PauseRace { at }
            | Self::ResumeRace { at }
            | Self::TriggerChaos { at, .. } => *at,
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
    FalseStartDetected {
        lane: u8,
        at: ProtocolMillis,
    },
    ConsequenceApplied {
        lane: u8,
        consequence: Consequence,
        at: ProtocolMillis,
    },
    RacePaused {
        at: ProtocolMillis,
    },
    RestartSequencePlanned {
        due_at: ProtocolMillis,
        at: ProtocolMillis,
    },
    RaceResumed {
        at: ProtocolMillis,
    },
    ChaosTriggered {
        source: ChaosSource,
        at: ProtocolMillis,
    },
    LanePowerOffExpired {
        lane: u8,
        at: ProtocolMillis,
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
            | Self::FalseStartDetected { at, .. }
            | Self::ConsequenceApplied { at, .. }
            | Self::RacePaused { at }
            | Self::RestartSequencePlanned { at, .. }
            | Self::RaceResumed { at }
            | Self::ChaosTriggered { at, .. }
            | Self::LanePowerOffExpired { at, .. }
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
            Self::FalseStartDetected { .. } => "false_start_detected",
            Self::ConsequenceApplied { .. } => "consequence_applied",
            Self::RacePaused { .. } => "race_paused",
            Self::RestartSequencePlanned { .. } => "restart_sequence_planned",
            Self::RaceResumed { .. } => "race_resumed",
            Self::ChaosTriggered { .. } => "chaos_triggered",
            Self::LanePowerOffExpired { .. } => "lane_power_off_expired",
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
    InvalidDriverAssignments,
}

pub(crate) fn validate_config(config: &RaceConfig) -> Result<(), DomainError> {
    if !(1..=4).contains(&config.lanes) {
        return Err(DomainError::InvalidLaneCount);
    }
    if config.driver_ids.len() != config.lanes as usize
        || config.driver_ids.iter().flatten().any(|id| *id <= 0)
        || config
            .driver_ids
            .iter()
            .flatten()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != config.driver_ids.iter().flatten().count()
    {
        return Err(DomainError::InvalidDriverAssignments);
    }
    if config.start_sequence_ms == 0
        || config.restart_sequence_ms == 0
        || config.minimum_lap_time_ms == 0
        || config
            .false_start_consequence
            .duration()
            .is_some_and(|duration| duration == 0)
        || config
            .chaos_consequence
            .duration()
            .is_some_and(|duration| duration == 0)
    {
        return Err(DomainError::InvalidDuration);
    }
    match config.finish_condition {
        FinishCondition::Laps(0) | FinishCondition::TimeMs(0) => Err(DomainError::InvalidTarget),
        _ => Ok(()),
    }
}
