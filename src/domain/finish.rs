use super::{
    DomainError, Event, FinishCondition, FinishMode, FinishedRace, Lifecycle, ProtocolMillis,
    RaceState, RaceStatus,
};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinishAction {
    Wait,
    FinishLane(u8),
    FinishAll,
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

fn condition_reached(
    condition: &FinishCondition,
    mode: FinishMode,
    leader_lane: u8,
) -> FinishAction {
    match (condition, mode) {
        (FinishCondition::Laps(_), FinishMode::Immediate)
        | (FinishCondition::Laps(_), FinishMode::LeaderLap)
        | (FinishCondition::TimeMs(_), FinishMode::Immediate) => FinishAction::FinishAll,
        (FinishCondition::Laps(_), FinishMode::AllCurrentLap) => {
            FinishAction::FinishLane(leader_lane)
        }
        (FinishCondition::TimeMs(_), FinishMode::LeaderLap)
        | (FinishCondition::TimeMs(_), FinishMode::AllCurrentLap) => FinishAction::Wait,
    }
}

fn valid_lap(
    condition: &FinishCondition,
    mode: FinishMode,
    condition_leader: u8,
    lane: u8,
) -> FinishAction {
    match (condition, mode) {
        (FinishCondition::TimeMs(_), FinishMode::LeaderLap) if lane == condition_leader => {
            FinishAction::FinishAll
        }
        (FinishCondition::Laps(_), FinishMode::AllCurrentLap)
        | (FinishCondition::TimeMs(_), FinishMode::AllCurrentLap) => FinishAction::FinishLane(lane),
        (FinishCondition::Laps(_), FinishMode::Immediate)
        | (FinishCondition::Laps(_), FinishMode::LeaderLap)
        | (FinishCondition::TimeMs(_), FinishMode::Immediate)
        | (FinishCondition::TimeMs(_), FinishMode::LeaderLap) => FinishAction::Wait,
    }
}

pub(crate) fn start(
    state: &mut RaceState,
    at: u64,
    leader_lane: u8,
    followups: &mut VecDeque<Event>,
) -> Result<(), DomainError> {
    if leader_lane != condition_leader(state) {
        return Err(DomainError::InvalidEventOrder);
    }
    let active = state.active_mut().ok_or(DomainError::InvalidEventOrder)?;
    let Lifecycle::Running { official_start_at } = active.lifecycle else {
        return Err(DomainError::InvalidEventOrder);
    };
    let action = condition_reached(
        &active.config.finish_condition,
        active.config.finish_mode,
        leader_lane,
    );
    active.lifecycle = Lifecycle::Finishing {
        official_start_at,
        finish_condition_at: at,
        condition_leader: leader_lane,
    };
    queue(state, action, at, followups);
    Ok(())
}

pub(crate) fn after_valid_lap(
    state: &RaceState,
    lane: u8,
    at: u64,
    followups: &mut VecDeque<Event>,
) -> Result<(), DomainError> {
    let active = state.active().ok_or(DomainError::InvalidEventOrder)?;
    let Lifecycle::Finishing {
        condition_leader, ..
    } = active.lifecycle
    else {
        return Err(DomainError::InvalidEventOrder);
    };
    queue(
        state,
        valid_lap(
            &active.config.finish_condition,
            active.config.finish_mode,
            condition_leader,
            lane,
        ),
        at,
        followups,
    );
    Ok(())
}

fn queue(state: &RaceState, action: FinishAction, at: u64, followups: &mut VecDeque<Event>) {
    match action {
        FinishAction::Wait => {}
        FinishAction::FinishLane(lane) => {
            followups.push_back(Event::LaneFinished { lane, at });
        }
        FinishAction::FinishAll => followups.extend(
            state
                .lanes
                .iter()
                .filter(|lane| lane.finished_at.is_none())
                .map(|lane| Event::LaneFinished {
                    lane: lane.lane,
                    at,
                }),
        ),
    }
}

pub(crate) fn lane(
    state: &mut RaceState,
    lane: u8,
    at: u64,
    followups: &mut VecDeque<Event>,
) -> Result<(), DomainError> {
    if !matches!(
        state.active().map(|race| &race.lifecycle),
        Some(Lifecycle::Finishing { .. })
    ) {
        return Err(DomainError::InvalidEventOrder);
    }
    let race_time_ms = state
        .race_elapsed_ms(at)
        .ok_or(DomainError::InvalidEventOrder)?;
    let lane_state = state.lane_mut(lane).ok_or(DomainError::InvalidEventOrder)?;
    if lane_state.finished_at.replace(at).is_some() {
        return Err(DomainError::InvalidEventOrder);
    }
    race_time_ms
        .checked_add(lane_state.result_time_penalty_ms)
        .ok_or(DomainError::InvalidDuration)?;
    lane_state.race_time_ms = Some(race_time_ms);
    if state.lanes.iter().all(|lane| lane.finished_at.is_some()) {
        followups.push_back(Event::RaceFinished { at });
    }
    Ok(())
}

pub(crate) fn race(state: &mut RaceState, at: u64) -> Result<(), DomainError> {
    if !state.lanes.iter().all(|lane| lane.finished_at.is_some()) {
        return Err(DomainError::InvalidEventOrder);
    }
    let elapsed_ms = state
        .race_elapsed_ms(at)
        .ok_or(DomainError::InvalidEventOrder)?;
    let active = state.active().ok_or(DomainError::InvalidEventOrder)?;
    let Lifecycle::Finishing {
        official_start_at,
        finish_condition_at,
        condition_leader,
    } = active.lifecycle
    else {
        return Err(DomainError::InvalidEventOrder);
    };
    state.status = RaceStatus::Finished(FinishedRace {
        config: active.config.clone(),
        official_start_at,
        finish_condition_at,
        condition_leader,
        finished_at: at,
        elapsed_ms,
    });
    Ok(())
}
