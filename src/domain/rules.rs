use super::{
    ActiveRace, ChaosSource, Command, Consequence, DomainError, Event, FinishCondition, LaneState,
    Lifecycle, RaceConfig, RaceControl, RaceState, RaceStatus, RejectionReason, condition_leader,
    finish, validate_config,
};
use std::collections::VecDeque;

pub(crate) fn command_root(
    state: &RaceState,
    command: Command,
) -> Result<Option<Event>, DomainError> {
    let event = match command {
        Command::StartRace { config, at } if matches!(state.status, RaceStatus::Ready) => {
            Event::RaceConfigured { config, at }
        }
        Command::AdvanceRace { .. } if state.active().is_some() => return Ok(None),
        Command::SensorTriggered { lane, at, edge } if state.active().is_some() => {
            if state.lane(lane).is_none() {
                return Err(DomainError::InvalidLane);
            }
            Event::MeasurementCaptured { lane, at, edge }
        }
        Command::CorrectLaps {
            lane,
            delta_thousandths,
            at,
        } if matches!(state.status, RaceStatus::Finished(_)) => {
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
            Event::LapCorrectionApplied {
                lane,
                delta_thousandths,
                at,
            }
        }
        Command::PauseRace { at }
            if matches!(
                state.active().map(|active| &active.control),
                Some(RaceControl::Live)
            ) =>
        {
            Event::RacePaused { at }
        }
        Command::ResumeRace { at }
            if matches!(
                state.active().map(|active| &active.control),
                Some(RaceControl::Paused { .. })
            ) =>
        {
            let due_at = at
                .checked_add(state.config().expect("active race").restart_sequence_ms)
                .ok_or(DomainError::InvalidDuration)?;
            Event::RestartSequencePlanned { due_at, at }
        }
        Command::TriggerChaos { source, at } if state.active().is_some() => {
            if let ChaosSource::Lane(lane) = source
                && state.lane(lane).is_none()
            {
                return Err(DomainError::InvalidLane);
            }
            Event::ChaosTriggered { source, at }
        }
        _ => return Err(DomainError::InvalidPhase),
    };
    Ok(Some(event))
}

pub(crate) fn apply(
    state: &mut RaceState,
    event: &Event,
    expected: bool,
    scheduled: bool,
    followups: &mut VecDeque<Event>,
) -> Result<(), DomainError> {
    match event {
        Event::RaceConfigured { config, at }
            if !expected
                && matches!(state.status, RaceStatus::Ready)
                && state.last_event_at.is_none() =>
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
            initialize(state, config, due_at);
            followups.push_back(Event::StartSequenceStarted { due_at, at: *at });
        }
        Event::StartSequenceStarted { due_at, .. }
            if expected
                && matches!(
                    state.active().map(|race| &race.lifecycle),
                    Some(Lifecycle::Starting { start_due_at }) if start_due_at == due_at
                ) => {}
        Event::OfficialStart { at } if scheduled => start_official(state, *at)?,
        Event::MeasurementCaptured { lane, at, .. } if !expected => {
            capture_measurement(state, *lane, *at, followups)?
        }
        Event::MeasurementRejected { .. } if expected => {}
        Event::ValidLap {
            lane,
            at,
            lap_time_ms,
        } if expected => apply_valid_lap(state, *lane, *at, *lap_time_ms, followups)?,
        Event::FalseStartDetected { lane, at } if expected => {
            let consequence = state
                .config()
                .ok_or(DomainError::InvalidEventOrder)?
                .false_start_consequence;
            followups.push_back(consequence_event(*lane, consequence, *at)?);
        }
        Event::ConsequenceApplied {
            lane,
            consequence,
            at,
        } if expected => apply_consequence(state, *lane, *consequence, *at)?,
        Event::RacePaused { at }
            if (expected
                || matches!(
                    state.active().map(|race| &race.control),
                    Some(RaceControl::Live)
                )) =>
        {
            pause(state, *at)?
        }
        Event::RestartSequencePlanned { due_at, at } if !expected => {
            plan_restart(state, *at, *due_at)?
        }
        Event::RaceResumed { at } if scheduled => resume(state, *at)?,
        Event::ChaosTriggered { source, at } if !expected => {
            trigger_chaos(state, *source, *at, followups)?
        }
        Event::LanePowerOffExpired { lane, at } if scheduled => {
            let lane = state
                .lane_mut(*lane)
                .ok_or(DomainError::InvalidEventOrder)?;
            if lane.power_off_until.take() != Some(*at) {
                return Err(DomainError::InvalidEventOrder);
            }
        }
        Event::FinishConditionReached { at, leader_lane } if expected || scheduled => {
            finish::start(state, *at, *leader_lane, followups)?
        }
        Event::LaneFinished { lane, at } if expected => finish::lane(state, *lane, *at, followups)?,
        Event::RaceFinished { at } if expected => finish::race(state, *at)?,
        Event::LapCorrectionApplied {
            lane,
            delta_thousandths,
            ..
        } if !expected && matches!(state.status, RaceStatus::Finished(_)) => {
            let lane = state
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

fn initialize(state: &mut RaceState, config: &RaceConfig, start_due_at: u64) {
    state.lanes = (1..=config.lanes)
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
    state.status = RaceStatus::Active(ActiveRace {
        config: config.clone(),
        lifecycle: Lifecycle::Starting { start_due_at },
        control: RaceControl::Live,
        accumulated_pause_ms: 0,
    });
}

fn start_official(state: &mut RaceState, at: u64) -> Result<(), DomainError> {
    let active = state.active_mut().ok_or(DomainError::InvalidEventOrder)?;
    if !matches!(active.control, RaceControl::Live)
        || !matches!(active.lifecycle, Lifecycle::Starting { start_due_at } if start_due_at == at)
    {
        return Err(DomainError::InvalidEventOrder);
    }
    active.lifecycle = Lifecycle::Running {
        official_start_at: at,
    };
    Ok(())
}

fn capture_measurement(
    state: &RaceState,
    lane: u8,
    at: u64,
    followups: &mut VecDeque<Event>,
) -> Result<(), DomainError> {
    let active = state.active().ok_or(DomainError::InvalidEventOrder)?;
    let lane_state = state.lane(lane).ok_or(DomainError::InvalidEventOrder)?;
    if matches!(active.lifecycle, Lifecycle::Starting { .. }) {
        followups.push_back(Event::FalseStartDetected { lane, at });
        return Ok(());
    }
    if lane_state.finished_at.is_some() {
        followups.push_back(Event::MeasurementRejected {
            lane,
            at,
            reason: RejectionReason::LaneAlreadyFinished,
        });
        return Ok(());
    }
    let reference = lane_state
        .last_valid_at
        .or(active.lifecycle.official_start_at())
        .ok_or(DomainError::InvalidEventOrder)?;
    let elapsed = at
        .checked_sub(reference)
        .ok_or(DomainError::InvalidEventOrder)?;
    followups.push_back(if elapsed < active.config.minimum_lap_time_ms {
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
    });
    Ok(())
}

fn consequence_event(lane: u8, consequence: Consequence, at: u64) -> Result<Event, DomainError> {
    if let Consequence::LanePowerOffMs(duration) = consequence {
        at.checked_add(duration)
            .ok_or(DomainError::InvalidDuration)?;
    }
    Ok(Event::ConsequenceApplied {
        lane,
        consequence,
        at,
    })
}

fn apply_consequence(
    state: &mut RaceState,
    lane: u8,
    consequence: Consequence,
    at: u64,
) -> Result<(), DomainError> {
    if state.lane(lane).is_none() {
        return Err(DomainError::InvalidEventOrder);
    }
    match consequence {
        Consequence::Abort => state.status = RaceStatus::Aborted,
        Consequence::ResultTimePenaltyMs(penalty) => {
            let lane = state.lane_mut(lane).expect("lane checked above");
            let total = lane
                .result_time_penalty_ms
                .checked_add(penalty)
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
                .checked_add(duration)
                .ok_or(DomainError::InvalidDuration)?;
            let lane = state.lane_mut(lane).expect("lane checked above");
            lane.power_off_until = Some(lane.power_off_until.map_or(until, |old| old.max(until)));
        }
    }
    Ok(())
}

fn pause(state: &mut RaceState, at: u64) -> Result<(), DomainError> {
    let active = state.active_mut().ok_or(DomainError::InvalidEventOrder)?;
    let paused_at = match active.control {
        RaceControl::Live => at,
        RaceControl::Paused { paused_at } | RaceControl::Restarting { paused_at, .. } => paused_at,
    };
    active.control = RaceControl::Paused { paused_at };
    Ok(())
}

fn plan_restart(state: &mut RaceState, at: u64, due_at: u64) -> Result<(), DomainError> {
    let active = state.active_mut().ok_or(DomainError::InvalidEventOrder)?;
    let RaceControl::Paused { paused_at } = active.control else {
        return Err(DomainError::InvalidEventOrder);
    };
    if at.checked_add(active.config.restart_sequence_ms) != Some(due_at) {
        return Err(DomainError::InvalidEventOrder);
    }
    active.control = RaceControl::Restarting {
        paused_at,
        restart_due_at: due_at,
    };
    Ok(())
}

fn resume(state: &mut RaceState, at: u64) -> Result<(), DomainError> {
    let active = state.active_mut().ok_or(DomainError::InvalidEventOrder)?;
    let RaceControl::Restarting {
        paused_at,
        restart_due_at,
    } = active.control
    else {
        return Err(DomainError::InvalidEventOrder);
    };
    if restart_due_at != at {
        return Err(DomainError::InvalidEventOrder);
    }
    let duration = at
        .checked_sub(paused_at)
        .ok_or(DomainError::InvalidEventOrder)?;
    if let Lifecycle::Starting { start_due_at } = &mut active.lifecycle {
        *start_due_at = start_due_at
            .checked_add(duration)
            .ok_or(DomainError::InvalidDuration)?;
    } else {
        active.accumulated_pause_ms = active
            .accumulated_pause_ms
            .checked_add(duration)
            .ok_or(DomainError::InvalidDuration)?;
    }
    if let (Lifecycle::Running { official_start_at }, FinishCondition::TimeMs(limit)) =
        (&active.lifecycle, &active.config.finish_condition)
    {
        official_start_at
            .checked_add(*limit)
            .and_then(|due| due.checked_add(active.accumulated_pause_ms))
            .ok_or(DomainError::InvalidDuration)?;
    }
    active.control = RaceControl::Live;
    Ok(())
}

fn trigger_chaos(
    state: &RaceState,
    source: ChaosSource,
    at: u64,
    followups: &mut VecDeque<Event>,
) -> Result<(), DomainError> {
    let active = state.active().ok_or(DomainError::InvalidEventOrder)?;
    let consequence = match source {
        ChaosSource::RaceControl => None,
        ChaosSource::Lane(lane) if state.lane(lane).is_some() => Some(consequence_event(
            lane,
            active.config.chaos_consequence,
            at,
        )?),
        ChaosSource::Lane(_) => return Err(DomainError::InvalidEventOrder),
    };
    followups.push_back(Event::RacePaused { at });
    if let Some(event) = consequence {
        followups.push_back(event);
    }
    Ok(())
}

fn apply_valid_lap(
    state: &mut RaceState,
    lane: u8,
    at: u64,
    lap_time_ms: u64,
    followups: &mut VecDeque<Event>,
) -> Result<(), DomainError> {
    let lane_state = state.lane_mut(lane).ok_or(DomainError::InvalidEventOrder)?;
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

    let active = state.active().ok_or(DomainError::InvalidEventOrder)?;
    match &active.lifecycle {
        Lifecycle::Running { .. } if matches!(active.config.finish_condition, FinishCondition::Laps(target) if state.lane(lane).expect("lane exists").laps >= target) =>
        {
            followups.push_back(Event::FinishConditionReached {
                at,
                leader_lane: condition_leader(state),
            });
        }
        Lifecycle::Finishing { .. } => finish::after_valid_lap(state, lane, at, followups)?,
        _ => {}
    }
    Ok(())
}
