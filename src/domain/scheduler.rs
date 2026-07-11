use super::{
    DomainError, Event, FinishCondition, Lifecycle, ProtocolMillis, RaceControl, RaceState,
    RaceStatus, finish,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Due {
    LanePowerExpiry { lane: u8, at: ProtocolMillis },
    OfficialStart { at: ProtocolMillis },
    RaceResume { at: ProtocolMillis },
    TimeFinish { at: ProtocolMillis },
}

impl Due {
    fn key(self) -> (ProtocolMillis, u8, Option<u8>) {
        match self {
            Self::LanePowerExpiry { lane, at } => (at, 0, Some(lane)),
            Self::OfficialStart { at } => (at, 1, None),
            Self::RaceResume { at } => (at, 2, None),
            Self::TimeFinish { at } => (at, 3, None),
        }
    }

    pub fn event(self, state: &RaceState) -> Event {
        match self {
            Self::LanePowerExpiry { lane, at } => Event::LanePowerOffExpired { lane, at },
            Self::OfficialStart { at } => Event::OfficialStart { at },
            Self::RaceResume { at } => Event::RaceResumed { at },
            Self::TimeFinish { at } => Event::FinishConditionReached {
                at,
                leader_lane: finish::condition_leader(state),
            },
        }
    }
}

pub(crate) fn next_due_at(state: &RaceState) -> Result<Option<ProtocolMillis>, DomainError> {
    Ok(earliest(state, ProtocolMillis::MAX)?.map(|due| due.key().0))
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
            due.push(Due::LanePowerExpiry {
                lane: lane.lane,
                at,
            });
        }
    }

    match (&active.control, &active.lifecycle) {
        (RaceControl::Live, Lifecycle::Starting { start_due_at }) => {
            due.push(Due::OfficialStart { at: *start_due_at });
        }
        (RaceControl::Live, Lifecycle::Running { official_start_at }) => {
            if let FinishCondition::TimeMs(duration) = active.config.finish_condition {
                let at = official_start_at
                    .checked_add(duration)
                    .and_then(|at| at.checked_add(active.accumulated_pause_ms))
                    .ok_or(DomainError::InvalidDuration)?;
                due.push(Due::TimeFinish { at });
            }
        }
        (RaceControl::Restarting { restart_due_at, .. }, _) => {
            due.push(Due::RaceResume {
                at: *restart_due_at,
            });
        }
        _ => {}
    }

    Ok(due
        .into_iter()
        .filter(|due| due.key().0 <= through)
        .min_by_key(|due| due.key()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_timestamp_priority_is_explicit() {
        let mut due = [
            Due::TimeFinish { at: 10 },
            Due::RaceResume { at: 10 },
            Due::OfficialStart { at: 10 },
            Due::LanePowerExpiry { lane: 1, at: 10 },
        ];
        due.sort_by_key(|due| due.key());
        assert_eq!(
            due,
            [
                Due::LanePowerExpiry { lane: 1, at: 10 },
                Due::OfficialStart { at: 10 },
                Due::RaceResume { at: 10 },
                Due::TimeFinish { at: 10 },
            ]
        );
    }
}
