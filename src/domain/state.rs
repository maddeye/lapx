use super::{DomainError, ProtocolMillis, RaceConfig, scheduler};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "lifecycle", rename_all = "snake_case")]
pub enum Lifecycle {
    Starting {
        start_due_at: ProtocolMillis,
    },
    Running {
        official_start_at: ProtocolMillis,
    },
    Finishing {
        official_start_at: ProtocolMillis,
        finish_condition_at: ProtocolMillis,
        condition_leader: u8,
    },
}

impl Lifecycle {
    pub(crate) fn official_start_at(&self) -> Option<ProtocolMillis> {
        match self {
            Self::Running { official_start_at }
            | Self::Finishing {
                official_start_at, ..
            } => Some(*official_start_at),
            Self::Starting { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "control", rename_all = "snake_case")]
pub enum RaceControl {
    #[default]
    Live,
    Paused {
        paused_at: ProtocolMillis,
    },
    Restarting {
        paused_at: ProtocolMillis,
        restart_due_at: ProtocolMillis,
    },
}

impl RaceControl {
    pub(crate) fn paused_at(&self) -> Option<ProtocolMillis> {
        match self {
            Self::Paused { paused_at } | Self::Restarting { paused_at, .. } => Some(*paused_at),
            Self::Live => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRace {
    pub config: RaceConfig,
    #[serde(flatten)]
    pub lifecycle: Lifecycle,
    #[serde(flatten)]
    pub control: RaceControl,
    pub accumulated_pause_ms: ProtocolMillis,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedRace {
    pub config: RaceConfig,
    pub official_start_at: ProtocolMillis,
    pub finish_condition_at: ProtocolMillis,
    pub condition_leader: u8,
    pub finished_at: ProtocolMillis,
    pub elapsed_ms: ProtocolMillis,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RaceStatus {
    #[default]
    Ready,
    Active(ActiveRace),
    Finished(FinishedRace),
    Aborted,
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
    pub race_time_ms: Option<ProtocolMillis>,
    pub result_time_penalty_ms: ProtocolMillis,
    pub power_off_until: Option<ProtocolMillis>,
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
    pub status: RaceStatus,
    pub last_event_at: Option<ProtocolMillis>,
    pub lanes: Vec<LaneState>,
}

impl RaceState {
    pub fn lane(&self, lane: u8) -> Option<&LaneState> {
        self.lanes.iter().find(|item| item.lane == lane)
    }

    pub fn next_due_at(&self) -> Result<Option<ProtocolMillis>, DomainError> {
        scheduler::next_due_at(self)
    }

    pub fn config(&self) -> Option<&RaceConfig> {
        match &self.status {
            RaceStatus::Active(active) => Some(&active.config),
            RaceStatus::Finished(finished) => Some(&finished.config),
            RaceStatus::Ready | RaceStatus::Aborted => None,
        }
    }

    pub fn official_start_at(&self) -> Option<ProtocolMillis> {
        match &self.status {
            RaceStatus::Active(active) => active.lifecycle.official_start_at(),
            RaceStatus::Finished(finished) => Some(finished.official_start_at),
            RaceStatus::Ready | RaceStatus::Aborted => None,
        }
    }

    pub fn finished_at(&self) -> Option<ProtocolMillis> {
        match &self.status {
            RaceStatus::Finished(finished) => Some(finished.finished_at),
            _ => None,
        }
    }

    pub fn intended_lane_power(&self, lane: u8) -> Option<bool> {
        let lane = self.lane(lane)?;
        Some(matches!(
            &self.status,
            RaceStatus::Active(ActiveRace {
                lifecycle: Lifecycle::Running { .. } | Lifecycle::Finishing { .. },
                control: RaceControl::Live,
                ..
            }) if lane.power_off_until.is_none()
        ))
    }

    /// True while the race clock advances: an active race that is running or
    /// finishing under live control. Canonical for every display clock.
    pub fn race_clock_running(&self) -> bool {
        matches!(
            &self.status,
            RaceStatus::Active(ActiveRace {
                lifecycle: Lifecycle::Running { .. } | Lifecycle::Finishing { .. },
                control: RaceControl::Live,
                ..
            })
        )
    }

    pub fn race_elapsed_ms(&self, at: ProtocolMillis) -> Option<ProtocolMillis> {
        match &self.status {
            RaceStatus::Active(active) => {
                let start = active.lifecycle.official_start_at()?;
                let effective_at = active.control.paused_at().unwrap_or(at);
                effective_at
                    .checked_sub(start)?
                    .checked_sub(active.accumulated_pause_ms)
            }
            RaceStatus::Finished(finished) => Some(finished.elapsed_ms),
            RaceStatus::Ready | RaceStatus::Aborted => None,
        }
    }

    pub fn standings(&self) -> Vec<Standing> {
        let mut standings: Vec<_> = self
            .lanes
            .iter()
            .map(|lane| Standing {
                lane: lane.lane,
                corrected_laps_thousandths: lane.corrected_laps_thousandths,
                result_time_ms: lane
                    .race_time_ms
                    .map(|time| time.saturating_add(lane.result_time_penalty_ms)),
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

    pub(crate) fn active(&self) -> Option<&ActiveRace> {
        match &self.status {
            RaceStatus::Active(active) => Some(active),
            _ => None,
        }
    }

    pub(crate) fn active_mut(&mut self) -> Option<&mut ActiveRace> {
        match &mut self.status {
            RaceStatus::Active(active) => Some(active),
            _ => None,
        }
    }

    pub(crate) fn lane_mut(&mut self, lane: u8) -> Option<&mut LaneState> {
        self.lanes.iter_mut().find(|item| item.lane == lane)
    }
}
