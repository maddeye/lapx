use super::{
    DomainError, Event, FinishCondition, Lifecycle, ProtocolMillis, RaceControl, RaceState,
    RaceStatus,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DueKind {
    LanePowerExpiry,
    OfficialStart,
    RaceResume,
    TimeFinish,
}

impl DueKind {
    fn priority(self) -> u8 {
        match self {
            Self::LanePowerExpiry => 0,
            Self::OfficialStart => 1,
            Self::RaceResume => 2,
            Self::TimeFinish => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Due {
    pub at: ProtocolMillis,
    pub kind: DueKind,
    pub lane: Option<u8>,
}

impl Due {
    pub fn event(self, state: &RaceState) -> Event {
        match self.kind {
            DueKind::LanePowerExpiry => Event::LanePowerOffExpired {
                lane: self.lane.expect("lane expiry always has a lane"),
                at: self.at,
            },
            DueKind::OfficialStart => Event::OfficialStart { at: self.at },
            DueKind::RaceResume => Event::RaceResumed { at: self.at },
            DueKind::TimeFinish => Event::FinishConditionReached {
                at: self.at,
                leader_lane: condition_leader(state),
            },
        }
    }
}

pub(crate) fn earliest(
    state: &RaceState,
    through: ProtocolMillis,
) -> Result<Option<Due>, DomainError> {
    let RaceStatus::Active(active) = &state.status else {
        return Ok(None);
    };
    let mut due = Vec::new();

    for lane in &state.lanes {
        if let Some(at) = lane.power_off_until {
            due.push(Due {
                at,
                kind: DueKind::LanePowerExpiry,
                lane: Some(lane.lane),
            });
        }
    }

    match (&active.control, &active.lifecycle) {
        (RaceControl::Live, Lifecycle::Starting { start_due_at }) => due.push(Due {
            at: *start_due_at,
            kind: DueKind::OfficialStart,
            lane: None,
        }),
        (RaceControl::Live, Lifecycle::Running { official_start_at }) => {
            if let FinishCondition::TimeMs(duration) = active.config.finish_condition {
                let at = official_start_at
                    .checked_add(duration)
                    .and_then(|at| at.checked_add(active.accumulated_pause_ms))
                    .ok_or(DomainError::InvalidDuration)?;
                due.push(Due {
                    at,
                    kind: DueKind::TimeFinish,
                    lane: None,
                });
            }
        }
        (RaceControl::Restarting { restart_due_at, .. }, _) => due.push(Due {
            at: *restart_due_at,
            kind: DueKind::RaceResume,
            lane: None,
        }),
        _ => {}
    }

    Ok(due
        .into_iter()
        .filter(|due| due.at <= through)
        .min_by_key(|due| (due.at, due.kind.priority(), due.lane)))
}

pub(crate) fn condition_leader(state: &RaceState) -> u8 {
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
        .expect("an active race has lanes")
        .lane
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_timestamp_priority_is_explicit() {
        let mut due = [
            Due {
                at: 10,
                kind: DueKind::TimeFinish,
                lane: None,
            },
            Due {
                at: 10,
                kind: DueKind::RaceResume,
                lane: None,
            },
            Due {
                at: 10,
                kind: DueKind::OfficialStart,
                lane: None,
            },
            Due {
                at: 10,
                kind: DueKind::LanePowerExpiry,
                lane: Some(1),
            },
        ];
        due.sort_by_key(|due| (due.at, due.kind.priority(), due.lane));
        assert_eq!(
            due.map(|due| due.kind),
            [
                DueKind::LanePowerExpiry,
                DueKind::OfficialStart,
                DueKind::RaceResume,
                DueKind::TimeFinish,
            ]
        );
    }
}
