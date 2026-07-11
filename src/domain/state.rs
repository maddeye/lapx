use super::{FinishMode, ProtocolMillis, RaceConfig};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum RacePhase {
    #[default]
    Ready,
    Starting {
        config: RaceConfig,
        start_due_at: ProtocolMillis,
    },
    Running {
        config: RaceConfig,
        official_start_at: ProtocolMillis,
    },
    Finishing {
        config: RaceConfig,
        official_start_at: ProtocolMillis,
        finish_condition_at: ProtocolMillis,
        condition_leader: u8,
    },
    Finished {
        config: RaceConfig,
        official_start_at: ProtocolMillis,
        finish_condition_at: ProtocolMillis,
        condition_leader: u8,
        finished_at: ProtocolMillis,
    },
}

impl RacePhase {
    pub fn config(&self) -> Option<&RaceConfig> {
        match self {
            Self::Ready => None,
            Self::Starting { config, .. }
            | Self::Running { config, .. }
            | Self::Finishing { config, .. }
            | Self::Finished { config, .. } => Some(config),
        }
    }

    pub fn official_start_at(&self) -> Option<ProtocolMillis> {
        match self {
            Self::Running {
                official_start_at, ..
            }
            | Self::Finishing {
                official_start_at, ..
            }
            | Self::Finished {
                official_start_at, ..
            } => Some(*official_start_at),
            Self::Ready | Self::Starting { .. } => None,
        }
    }

    pub fn finished_at(&self) -> Option<ProtocolMillis> {
        match self {
            Self::Finished { finished_at, .. } => Some(*finished_at),
            _ => None,
        }
    }
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaceState {
    #[serde(flatten)]
    pub phase: RacePhase,
    pub last_event_at: Option<ProtocolMillis>,
    pub lanes: Vec<LaneState>,
}

impl RaceState {
    pub fn lane(&self, lane: u8) -> Option<&LaneState> {
        self.lanes.iter().find(|item| item.lane == lane)
    }

    pub fn config(&self) -> Option<&RaceConfig> {
        self.phase.config()
    }

    pub fn official_start_at(&self) -> Option<ProtocolMillis> {
        self.phase.official_start_at()
    }

    pub fn finished_at(&self) -> Option<ProtocolMillis> {
        self.phase.finished_at()
    }

    pub fn standings(&self) -> Vec<Standing> {
        let start = self.official_start_at();
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

    pub(crate) fn lane_mut(&mut self, lane: u8) -> Option<&mut LaneState> {
        self.lanes.iter_mut().find(|item| item.lane == lane)
    }

    pub(crate) fn finish_mode(&self) -> Option<FinishMode> {
        self.config().map(|config| config.finish_mode)
    }
}
