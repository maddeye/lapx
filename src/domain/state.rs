use super::{FinishMode, ProtocolMillis, RaceConfig};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "suspended_phase", rename_all = "snake_case")]
pub enum SuspendedPhase {
    Starting {
        config: RaceConfig,
        start_due_at: ProtocolMillis,
    },
    Running {
        config: RaceConfig,
        official_start_at: ProtocolMillis,
        paused_ms: ProtocolMillis,
    },
    Finishing {
        config: RaceConfig,
        official_start_at: ProtocolMillis,
        paused_ms: ProtocolMillis,
        finish_condition_at: ProtocolMillis,
        condition_leader: u8,
    },
}

impl SuspendedPhase {
    fn config(&self) -> &RaceConfig {
        match self {
            Self::Starting { config, .. }
            | Self::Running { config, .. }
            | Self::Finishing { config, .. } => config,
        }
    }

    fn official_start_at(&self) -> Option<ProtocolMillis> {
        match self {
            Self::Running {
                official_start_at, ..
            }
            | Self::Finishing {
                official_start_at, ..
            } => Some(*official_start_at),
            Self::Starting { .. } => None,
        }
    }

    pub(crate) fn shifted(self, duration: ProtocolMillis) -> Result<Self, ()> {
        Ok(match self {
            Self::Starting {
                config,
                start_due_at,
            } => Self::Starting {
                config,
                start_due_at: start_due_at.checked_add(duration).ok_or(())?,
            },
            Self::Running {
                config,
                official_start_at,
                paused_ms,
            } => Self::Running {
                config,
                official_start_at,
                paused_ms: paused_ms.checked_add(duration).ok_or(())?,
            },
            Self::Finishing {
                config,
                official_start_at,
                paused_ms,
                finish_condition_at,
                condition_leader,
            } => Self::Finishing {
                config,
                official_start_at,
                paused_ms: paused_ms.checked_add(duration).ok_or(())?,
                finish_condition_at,
                condition_leader,
            },
        })
    }
}

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
        paused_ms: ProtocolMillis,
    },
    Finishing {
        config: RaceConfig,
        official_start_at: ProtocolMillis,
        paused_ms: ProtocolMillis,
        finish_condition_at: ProtocolMillis,
        condition_leader: u8,
    },
    Paused {
        suspended: SuspendedPhase,
        paused_at: ProtocolMillis,
    },
    Restarting {
        suspended: SuspendedPhase,
        paused_at: ProtocolMillis,
        restart_due_at: ProtocolMillis,
    },
    Finished {
        config: RaceConfig,
        official_start_at: ProtocolMillis,
        paused_ms: ProtocolMillis,
        finish_condition_at: ProtocolMillis,
        condition_leader: u8,
        finished_at: ProtocolMillis,
    },
    Aborted {
        config: RaceConfig,
        aborted_at: ProtocolMillis,
    },
}

impl RacePhase {
    pub fn config(&self) -> Option<&RaceConfig> {
        match self {
            Self::Ready => None,
            Self::Starting { config, .. }
            | Self::Running { config, .. }
            | Self::Finishing { config, .. }
            | Self::Finished { config, .. }
            | Self::Aborted { config, .. } => Some(config),
            Self::Paused { suspended, .. } | Self::Restarting { suspended, .. } => {
                Some(suspended.config())
            }
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
            Self::Paused { suspended, .. } | Self::Restarting { suspended, .. } => {
                suspended.official_start_at()
            }
            Self::Ready | Self::Starting { .. } | Self::Aborted { .. } => None,
        }
    }

    pub fn finished_at(&self) -> Option<ProtocolMillis> {
        match self {
            Self::Finished { finished_at, .. } => Some(*finished_at),
            _ => None,
        }
    }

    pub(crate) fn active(&self) -> Option<SuspendedPhase> {
        match self {
            Self::Starting {
                config,
                start_due_at,
            } => Some(SuspendedPhase::Starting {
                config: config.clone(),
                start_due_at: *start_due_at,
            }),
            Self::Running {
                config,
                official_start_at,
                paused_ms,
            } => Some(SuspendedPhase::Running {
                config: config.clone(),
                official_start_at: *official_start_at,
                paused_ms: *paused_ms,
            }),
            Self::Finishing {
                config,
                official_start_at,
                paused_ms,
                finish_condition_at,
                condition_leader,
            } => Some(SuspendedPhase::Finishing {
                config: config.clone(),
                official_start_at: *official_start_at,
                paused_ms: *paused_ms,
                finish_condition_at: *finish_condition_at,
                condition_leader: *condition_leader,
            }),
            _ => None,
        }
    }

    pub(crate) fn from_active(active: SuspendedPhase) -> Self {
        match active {
            SuspendedPhase::Starting {
                config,
                start_due_at,
            } => Self::Starting {
                config,
                start_due_at,
            },
            SuspendedPhase::Running {
                config,
                official_start_at,
                paused_ms,
            } => Self::Running {
                config,
                official_start_at,
                paused_ms,
            },
            SuspendedPhase::Finishing {
                config,
                official_start_at,
                paused_ms,
                finish_condition_at,
                condition_leader,
            } => Self::Finishing {
                config,
                official_start_at,
                paused_ms,
                finish_condition_at,
                condition_leader,
            },
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

    pub fn intended_lane_power(&self, lane: u8) -> Option<bool> {
        let lane = self.lane(lane)?;
        Some(
            matches!(
                self.phase,
                RacePhase::Running { .. } | RacePhase::Finishing { .. }
            ) && lane.power_off_until.is_none(),
        )
    }

    pub fn race_elapsed_ms(&self, at: ProtocolMillis) -> Option<ProtocolMillis> {
        let (official_start_at, paused_ms, effective_at) = match &self.phase {
            RacePhase::Running {
                official_start_at,
                paused_ms,
                ..
            }
            | RacePhase::Finishing {
                official_start_at,
                paused_ms,
                ..
            }
            | RacePhase::Finished {
                official_start_at,
                paused_ms,
                ..
            } => (*official_start_at, *paused_ms, at),
            RacePhase::Paused {
                suspended:
                    SuspendedPhase::Running {
                        official_start_at,
                        paused_ms,
                        ..
                    }
                    | SuspendedPhase::Finishing {
                        official_start_at,
                        paused_ms,
                        ..
                    },
                paused_at,
            }
            | RacePhase::Restarting {
                suspended:
                    SuspendedPhase::Running {
                        official_start_at,
                        paused_ms,
                        ..
                    }
                    | SuspendedPhase::Finishing {
                        official_start_at,
                        paused_ms,
                        ..
                    },
                paused_at,
                ..
            } => (*official_start_at, *paused_ms, *paused_at),
            _ => return None,
        };
        effective_at
            .checked_sub(official_start_at)?
            .checked_sub(paused_ms)
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

    pub(crate) fn lane_mut(&mut self, lane: u8) -> Option<&mut LaneState> {
        self.lanes.iter_mut().find(|item| item.lane == lane)
    }

    pub(crate) fn finish_mode(&self) -> Option<FinishMode> {
        self.config().map(|config| config.finish_mode)
    }
}
