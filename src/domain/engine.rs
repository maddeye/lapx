use super::{Command, DomainError, Event, RaceState, RaceStatus, rules, scheduler};
use std::collections::VecDeque;

#[derive(Clone, Debug, Default)]
pub struct RaceEngine {
    state: RaceState,
    expected: VecDeque<Event>,
}

impl RaceEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &RaceState {
        &self.state
    }

    pub fn replay(events: &[Event]) -> Result<Self, DomainError> {
        let mut engine = Self::new();
        for event in events {
            engine.accept(event)?;
        }
        engine.complete_replay()
    }

    pub fn replay_at(events: &[Event], at: u64) -> Result<Self, DomainError> {
        let mut engine = Self::new();
        for event in events.iter().take_while(|event| event.timestamp() <= at) {
            engine.accept(event)?;
        }
        engine.complete_replay()
    }

    pub fn handle(&self, command: Command) -> Result<Vec<Event>, DomainError> {
        let command_at = command.timestamp();
        let starting = matches!(&command, Command::StartRace { .. });
        if let Some(last) = self.state.last_event_at.filter(|last| command_at < *last) {
            return Err(DomainError::TimestampBeforeLast {
                last,
                command: command_at,
            });
        }

        let mut engine = self.clone();
        let mut events = Vec::new();
        engine.materialize_due(command_at, &mut events)?;
        let root = match rules::command_root(&engine.state, command) {
            Ok(root) => root,
            Err(DomainError::InvalidPhase)
                if !starting
                    && !events.is_empty()
                    && matches!(
                        engine.state.status,
                        RaceStatus::Finished(_) | RaceStatus::Aborted
                    ) =>
            {
                return Ok(events);
            }
            Err(error) => return Err(error),
        };
        if let Some(event) = root {
            engine.emit(event, &mut events)?;
        }
        Ok(events)
    }

    fn complete_replay(self) -> Result<Self, DomainError> {
        if self.expected.is_empty() {
            Ok(self)
        } else {
            Err(DomainError::InvalidEventOrder)
        }
    }

    fn emit(&mut self, event: Event, emitted: &mut Vec<Event>) -> Result<(), DomainError> {
        self.accept(&event)?;
        emitted.push(event);
        while let Some(event) = self.expected.front().cloned() {
            self.accept(&event)?;
            emitted.push(event);
        }
        Ok(())
    }

    fn materialize_due(
        &mut self,
        through: u64,
        emitted: &mut Vec<Event>,
    ) -> Result<(), DomainError> {
        while let Some(due) = scheduler::earliest(&self.state, through)? {
            self.emit(due.event(&self.state), emitted)?;
        }
        Ok(())
    }

    fn accept(&mut self, event: &Event) -> Result<(), DomainError> {
        if self
            .state
            .last_event_at
            .is_some_and(|last| event.timestamp() < last)
        {
            return Err(DomainError::InvalidEventOrder);
        }

        let expected = if let Some(next) = self.expected.pop_front() {
            if next != *event {
                return Err(DomainError::InvalidEventOrder);
            }
            true
        } else {
            false
        };
        let scheduled = if expected {
            false
        } else if let Some(due) = scheduler::earliest(&self.state, event.timestamp())? {
            if due.event(&self.state) != *event {
                return Err(DomainError::InvalidEventOrder);
            }
            true
        } else {
            false
        };

        rules::apply(
            &mut self.state,
            event,
            expected,
            scheduled,
            &mut self.expected,
        )?;
        self.state.last_event_at = Some(event.timestamp());
        Ok(())
    }
}
