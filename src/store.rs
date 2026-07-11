use crate::domain::{Command, DomainError, Event, RaceEngine, RaceState};
use rusqlite::{Connection, ErrorCode, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    ops::Deref,
    path::{Path, PathBuf},
    time::Duration,
};

const EVENT_SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub sequence: u64,
    pub state: RaceState,
}

impl Deref for StateSnapshot {
    type Target = RaceState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

#[derive(Clone, Debug)]
pub struct SqliteStore {
    path: PathBuf,
}

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Domain(DomainError),
    CorruptProtocol(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for StoreError {}
impl StoreError {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(error, _))
                if matches!(error.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
        )
    }
}
impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
impl From<DomainError> for StoreError {
    fn from(value: DomainError) -> Self {
        Self::Domain(value)
    }
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let store = Self {
            path: path.as_ref().to_owned(),
        };
        let connection = store.connect()?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS race_events (
                race_id TEXT NOT NULL,
                sequence INTEGER NOT NULL CHECK (sequence > 0),
                event_type TEXT NOT NULL,
                schema_version INTEGER NOT NULL CHECK (schema_version = 1),
                payload TEXT NOT NULL,
                PRIMARY KEY (race_id, sequence)
            );
            CREATE TRIGGER IF NOT EXISTS race_events_contiguous
            BEFORE INSERT ON race_events
            WHEN NEW.sequence != COALESCE((SELECT MAX(sequence) + 1 FROM race_events WHERE race_id = NEW.race_id), 1)
            BEGIN SELECT RAISE(ABORT, 'race event sequence must be contiguous'); END;
            CREATE TRIGGER IF NOT EXISTS race_events_no_update
            BEFORE UPDATE ON race_events BEGIN SELECT RAISE(ABORT, 'race events are append-only'); END;
            CREATE TRIGGER IF NOT EXISTS race_events_no_delete
            BEFORE DELETE ON race_events BEGIN SELECT RAISE(ABORT, 'race events are append-only'); END;"
        )?;
        Ok(store)
    }

    pub fn execute(&self, race_id: &str, command: Command) -> Result<StateSnapshot, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut events = load_events(&transaction, race_id)?;
        let engine = replay(&events)?;
        let emitted = engine.handle(command)?;
        let first_sequence = events.len() as i64 + 1;
        for (offset, event) in emitted.iter().enumerate() {
            transaction.execute(
                "INSERT INTO race_events (race_id, sequence, event_type, schema_version, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![race_id, first_sequence + offset as i64, event.event_type(), EVENT_SCHEMA_VERSION, serde_json::to_string(event)?],
            )?;
        }
        events.extend(emitted);
        let snapshot = StateSnapshot {
            sequence: events.len() as u64,
            state: replay(&events)?.state().clone(),
        };
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn load(&self, race_id: &str) -> Result<StateSnapshot, StoreError> {
        let connection = self.connect()?;
        let events = load_events(&connection, race_id)?;
        Ok(StateSnapshot {
            sequence: events.len() as u64,
            state: replay(&events)?.state().clone(),
        })
    }

    pub fn events(&self, race_id: &str) -> Result<Vec<Event>, StoreError> {
        let connection = self.connect()?;
        load_events(&connection, race_id)
    }

    fn connect(&self) -> Result<Connection, StoreError> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_millis(50))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(connection)
    }
}

fn replay(events: &[Event]) -> Result<RaceEngine, StoreError> {
    RaceEngine::replay(events)
        .map_err(|error| StoreError::CorruptProtocol(format!("invalid event history: {error:?}")))
}

fn load_events(connection: &Connection, race_id: &str) -> Result<Vec<Event>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT sequence, event_type, schema_version, payload FROM race_events WHERE race_id = ?1 ORDER BY sequence"
    )?;
    let rows = statement.query_map([race_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut events = vec![];
    for row in rows {
        let (sequence, event_type, version, payload) = row?;
        let expected = events.len() as i64 + 1;
        if sequence != expected || version != EVENT_SCHEMA_VERSION {
            return Err(StoreError::CorruptProtocol(format!(
                "expected sequence {expected} at schema {EVENT_SCHEMA_VERSION}, got sequence {sequence} at schema {version}"
            )));
        }
        let event: Event = serde_json::from_str(&payload)?;
        if event.event_type() != event_type {
            return Err(StoreError::CorruptProtocol(format!(
                "event type {event_type} does not match payload"
            )));
        }
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn race_protocol_connections_use_wal_and_full_synchronous() {
        let dir = tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
        let connection = store.connect().unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();

        assert_eq!(journal_mode, "wal");
        assert_eq!(synchronous, 2); // SQLite FULL
    }
}
