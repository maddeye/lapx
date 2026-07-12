use crate::domain::{Command, DomainError, Event, RaceEngine, RaceState, RaceStatus};
use rusqlite::{Connection, ErrorCode, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    ops::Deref,
    path::{Path, PathBuf},
    time::Duration,
};

mod tournament;

pub use tournament::{
    HeatAssignment, Tournament, TournamentGeneration, TournamentGenerationMode, TournamentHeat,
};

const EVENT_SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Driver {
    pub id: i64,
    pub display_name: String,
    pub archived_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaceResult {
    pub position: u8,
    pub lane: u8,
    pub driver_id: Option<i64>,
    pub corrected_laps_thousandths: i64,
    pub result_time_ms: Option<u64>,
    pub best_lap_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedRace {
    pub race_id: String,
    pub official_start_at: u64,
    pub finished_at: u64,
    pub results: Vec<RaceResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverStats {
    pub driver_id: i64,
    pub starts: u64,
    pub wins: u64,
    pub best_lap_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EloRating {
    pub driver_id: i64,
    pub rating: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EloDelta {
    pub driver_id: i64,
    pub delta: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EloRaceDelta {
    pub race_id: String,
    pub deltas: Vec<EloDelta>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EloSummary {
    pub ratings: Vec<EloRating>,
    pub races: Vec<EloRaceDelta>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub race_id: String,
    pub sequence: u64,
    pub state: RaceState,
}

impl StateSnapshot {
    pub fn follows(&self, previous: &Self) -> bool {
        if self.race_id == previous.race_id {
            return self.sequence > previous.sequence;
        }
        matches!(
            previous.state.status,
            RaceStatus::Finished(_) | RaceStatus::Aborted
        ) && self.sequence == 0
            && matches!(self.state.status, RaceStatus::Ready)
    }
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
pub enum ImmediateError<E> {
    Store(StoreError),
    Callback {
        snapshot: Box<StateSnapshot>,
        source: E,
    },
}

impl<E> From<StoreError> for ImmediateError<E> {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

impl<E> From<rusqlite::Error> for ImmediateError<E> {
    fn from(value: rusqlite::Error) -> Self {
        Self::Store(value.into())
    }
}

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Domain(DomainError),
    InvalidDriverName,
    DriverNotFound(i64),
    DriverNotActive(i64),
    InvalidTournamentName,
    InvalidTournamentGeneration,
    TournamentNotFound(i64),
    HeatNotFound(i64),
    InvalidHeatAssignments,
    TournamentFrozen(i64),
    HeatAlreadyLinked(i64),
    RaceNotFound(String),
    RaceAlreadyLinked(String),
    RaceAssignmentsMismatch,
    InvalidRaceId,
    CurrentRaceConflict { expected: String, actual: String },
    RaceNotTerminal(String),
    RaceAlreadyExists(String),
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
            "CREATE TABLE IF NOT EXISTS current_race (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                race_id TEXT NOT NULL CHECK (length(trim(race_id)) > 0)
            );
            CREATE TABLE IF NOT EXISTS race_events (
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
            BEFORE DELETE ON race_events BEGIN SELECT RAISE(ABORT, 'race events are append-only'); END;
            CREATE TABLE IF NOT EXISTS drivers (
                id INTEGER PRIMARY KEY,
                display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
                archived_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS tournaments (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL CHECK (length(trim(name)) > 0)
            );
            CREATE TABLE IF NOT EXISTS tournament_generation (
                tournament_id INTEGER PRIMARY KEY REFERENCES tournaments(id),
                mode TEXT NOT NULL CHECK (mode IN ('random', 'elo_balanced')),
                seed TEXT NOT NULL,
                lane_count INTEGER NOT NULL CHECK (lane_count BETWEEN 1 AND 4)
            );
            CREATE TABLE IF NOT EXISTS tournament_heats (
                id INTEGER PRIMARY KEY,
                tournament_id INTEGER NOT NULL REFERENCES tournaments(id),
                position INTEGER NOT NULL CHECK (position > 0),
                race_id TEXT UNIQUE,
                UNIQUE (tournament_id, position)
            );
            CREATE TABLE IF NOT EXISTS tournament_heat_assignments (
                heat_id INTEGER NOT NULL REFERENCES tournament_heats(id),
                lane INTEGER NOT NULL CHECK (lane BETWEEN 1 AND 4),
                driver_id INTEGER NOT NULL REFERENCES drivers(id),
                PRIMARY KEY (heat_id, lane),
                UNIQUE (heat_id, driver_id)
            );
            CREATE TRIGGER IF NOT EXISTS tournament_generation_no_update
            BEFORE UPDATE ON tournament_generation
            BEGIN SELECT RAISE(ABORT, 'tournament generation is immutable'); END;
            CREATE TRIGGER IF NOT EXISTS tournament_generation_no_delete
            BEFORE DELETE ON tournament_generation
            BEGIN SELECT RAISE(ABORT, 'tournament generation is immutable'); END;
            CREATE TRIGGER IF NOT EXISTS tournament_heat_order_immutable
            BEFORE UPDATE OF tournament_id, position ON tournament_heats
            BEGIN SELECT RAISE(ABORT, 'tournament heat order is immutable'); END;
            CREATE TRIGGER IF NOT EXISTS tournament_heats_no_delete
            BEFORE DELETE ON tournament_heats
            BEGIN SELECT RAISE(ABORT, 'tournament heats are immutable'); END;
            CREATE TRIGGER IF NOT EXISTS tournament_assignments_no_update
            BEFORE UPDATE ON tournament_heat_assignments
            BEGIN SELECT RAISE(ABORT, 'tournament heat assignments are immutable'); END;
            CREATE TRIGGER IF NOT EXISTS tournament_assignments_no_delete
            BEFORE DELETE ON tournament_heat_assignments
            BEGIN SELECT RAISE(ABORT, 'tournament heat assignments are immutable'); END;"
        )?;
        Ok(store)
    }

    pub fn execute(&self, race_id: &str, command: Command) -> Result<StateSnapshot, StoreError> {
        self.execute_with(race_id, |_| command)
    }

    pub fn execute_now<F>(
        &self,
        race_id: &str,
        proposed_at: u64,
        command: F,
    ) -> Result<StateSnapshot, StoreError>
    where
        F: FnOnce(u64) -> Command,
    {
        self.execute_with(race_id, |last| command(proposed_at.max(last.unwrap_or(0))))
    }

    fn execute_with<F>(&self, race_id: &str, command: F) -> Result<StateSnapshot, StoreError>
    where
        F: FnOnce(Option<u64>) -> Command,
    {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut events = load_events(&transaction, race_id)?;
        let engine = replay(&events)?;
        let emitted = engine.handle(command(engine.state().last_event_at))?;
        if let Some(Event::RaceConfigured { config, .. }) = emitted.first() {
            validate_active_drivers(&transaction, &config.driver_ids)?;
        }
        let first_sequence = events.len() as i64 + 1;
        for (offset, event) in emitted.iter().enumerate() {
            transaction.execute(
                "INSERT INTO race_events (race_id, sequence, event_type, schema_version, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![race_id, first_sequence + offset as i64, event.event_type(), EVENT_SCHEMA_VERSION, serde_json::to_string(event)?],
            )?;
        }
        events.extend(emitted);
        let snapshot = StateSnapshot {
            race_id: race_id.to_owned(),
            sequence: events.len() as u64,
            state: replay(&events)?.state().clone(),
        };
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn load(&self, race_id: &str) -> Result<StateSnapshot, StoreError> {
        let connection = self.connect()?;
        snapshot(&connection, race_id)
    }

    pub fn initialize_current_race(
        &self,
        fallback_race_id: &str,
    ) -> Result<StateSnapshot, StoreError> {
        let fallback_race_id = valid_race_id(fallback_race_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO current_race (singleton, race_id) VALUES (1, ?1)",
            [fallback_race_id],
        )?;
        let race_id: String = transaction.query_row(
            "SELECT race_id FROM current_race WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        let current = snapshot(&transaction, &race_id)?;
        transaction.commit()?;
        Ok(current)
    }

    pub fn switch_current_race(
        &self,
        expected_race_id: &str,
        next_race_id: &str,
    ) -> Result<StateSnapshot, StoreError> {
        match self.switch_current_race_with(expected_race_id, next_race_id, || {
            Ok::<_, std::convert::Infallible>(())
        }) {
            Ok(snapshot) => Ok(snapshot),
            Err(ImmediateError::Store(error)) => Err(error),
            Err(ImmediateError::Callback { source, .. }) => match source {},
        }
    }

    pub fn switch_current_race_with<E>(
        &self,
        expected_race_id: &str,
        next_race_id: &str,
        before_commit: impl FnOnce() -> Result<(), E>,
    ) -> Result<StateSnapshot, ImmediateError<E>> {
        let next_race_id = valid_race_id(next_race_id)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual: String = transaction.query_row(
            "SELECT race_id FROM current_race WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if actual != expected_race_id {
            return Err(StoreError::CurrentRaceConflict {
                expected: expected_race_id.to_owned(),
                actual,
            }
            .into());
        }
        let current = snapshot(&transaction, &actual)?;
        if !matches!(
            current.state.status,
            RaceStatus::Finished(_) | RaceStatus::Aborted
        ) {
            return Err(StoreError::RaceNotTerminal(actual).into());
        }
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM race_events WHERE race_id = ?1)",
            [next_race_id],
            |row| row.get(0),
        )?;
        if exists || next_race_id == actual {
            return Err(StoreError::RaceAlreadyExists(next_race_id.to_owned()).into());
        }
        before_commit().map_err(|source| ImmediateError::Callback {
            snapshot: Box::new(current),
            source,
        })?;
        transaction.execute(
            "UPDATE current_race SET race_id = ?1 WHERE singleton = 1",
            [next_race_id],
        )?;
        let next = snapshot(&transaction, next_race_id)?;
        transaction.commit()?;
        Ok(next)
    }

    pub fn with_immediate_head<T, E>(
        &self,
        race_id: &str,
        callback: impl FnOnce(&StateSnapshot) -> Result<T, E>,
    ) -> Result<(StateSnapshot, T), ImmediateError<E>> {
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let snapshot = snapshot(&transaction, race_id)?;
        let result = callback(&snapshot);
        transaction.commit().map_err(StoreError::from)?;
        result
            .map(|value| (snapshot.clone(), value))
            .map_err(|source| ImmediateError::Callback {
                snapshot: Box::new(snapshot),
                source,
            })
    }

    pub fn events(&self, race_id: &str) -> Result<Vec<Event>, StoreError> {
        let connection = self.connect()?;
        load_events(&connection, race_id)
    }

    pub fn drivers(&self) -> Result<Vec<Driver>, StoreError> {
        let connection = self.connect()?;
        load_drivers(&connection)
    }

    pub fn completed_races(&self) -> Result<Vec<CompletedRace>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let races = load_completed_races(&transaction)?;
        transaction.commit()?;
        Ok(races)
    }

    pub fn driver_stats(&self) -> Result<Vec<DriverStats>, StoreError> {
        let mut stats = BTreeMap::<i64, DriverStats>::new();
        for race in self.completed_races()? {
            for result in race.results {
                let Some(driver_id) = result.driver_id else {
                    continue;
                };
                let stat = stats.entry(driver_id).or_insert(DriverStats {
                    driver_id,
                    starts: 0,
                    wins: 0,
                    best_lap_ms: None,
                });
                stat.starts += 1;
                stat.wins += u64::from(result.position == 1);
                if let Some(lap) = result.best_lap_ms {
                    stat.best_lap_ms = Some(stat.best_lap_ms.map_or(lap, |best| best.min(lap)));
                }
            }
        }
        Ok(stats.into_values().collect())
    }

    pub fn elo(&self) -> Result<EloSummary, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let summary = derive_elo(&transaction)?;
        transaction.commit()?;
        Ok(summary)
    }

    pub fn create_driver(&self, display_name: &str) -> Result<Driver, StoreError> {
        let display_name = driver_name(display_name)?;
        let connection = self.connect()?;
        connection.execute(
            "INSERT INTO drivers (display_name) VALUES (?1)",
            [display_name],
        )?;
        load_driver(&connection, connection.last_insert_rowid())
    }

    pub fn rename_driver(&self, id: i64, display_name: &str) -> Result<Driver, StoreError> {
        let display_name = driver_name(display_name)?;
        let connection = self.connect()?;
        if connection.execute(
            "UPDATE drivers SET display_name = ?1 WHERE id = ?2",
            params![display_name, id],
        )? == 0
        {
            return Err(StoreError::DriverNotFound(id));
        }
        load_driver(&connection, id)
    }

    pub fn archive_driver(&self, id: i64) -> Result<Driver, StoreError> {
        let connection = self.connect()?;
        if connection.execute(
            "UPDATE drivers SET archived_at = COALESCE(archived_at, unixepoch()) WHERE id = ?1",
            [id],
        )? == 0
        {
            return Err(StoreError::DriverNotFound(id));
        }
        load_driver(&connection, id)
    }

    fn connect(&self) -> Result<Connection, StoreError> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_millis(50))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(connection)
    }
}

fn load_drivers(connection: &Connection) -> Result<Vec<Driver>, StoreError> {
    let mut statement =
        connection.prepare("SELECT id, display_name, archived_at FROM drivers ORDER BY id")?;
    statement
        .query_map([], driver_from_row)?
        .collect::<Result<_, _>>()
        .map_err(StoreError::from)
}

fn load_completed_races(connection: &Connection) -> Result<Vec<CompletedRace>, StoreError> {
    let race_ids = {
        let mut statement = connection.prepare(
            "SELECT race_id
             FROM race_events
             GROUP BY race_id
             HAVING SUM(event_type = 'race_finished') > 0
             ORDER BY MAX(CASE WHEN event_type = 'race_finished' THEN rowid END) DESC",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut races = Vec::new();
    for race_id in race_ids {
        let events = load_events(connection, &race_id)?;
        let state = replay(&events)?.state().clone();
        if let RaceStatus::Finished(finished) = &state.status {
            let results = state
                .standings()
                .into_iter()
                .enumerate()
                .map(|(index, standing)| RaceResult {
                    position: index as u8 + 1,
                    lane: standing.lane,
                    driver_id: finished.config.driver_ids[standing.lane as usize - 1],
                    corrected_laps_thousandths: standing.corrected_laps_thousandths,
                    result_time_ms: standing.result_time_ms,
                    best_lap_ms: state.lane(standing.lane).and_then(|lane| lane.best_lap_ms),
                })
                .collect();
            races.push(CompletedRace {
                race_id,
                official_start_at: finished.official_start_at,
                finished_at: finished.finished_at,
                results,
            });
        }
    }
    Ok(races)
}

fn derive_elo(connection: &Connection) -> Result<EloSummary, StoreError> {
    let mut ratings: BTreeMap<_, _> = load_drivers(connection)?
        .into_iter()
        .map(|driver| (driver.id, 1500))
        .collect();
    let mut completed = load_completed_races(connection)?;
    // History is newest-first; Elo folds the durable completion order oldest-first.
    completed.reverse();
    let mut races = Vec::with_capacity(completed.len());

    for race in completed {
        let participants: Vec<_> = race
            .results
            .iter()
            .filter(|result| result.driver_id.is_some())
            .collect();
        let mut deltas = if participants.len() < 2 {
            Vec::new()
        } else {
            participants
                .iter()
                .map(|result| {
                    let driver_id = result.driver_id.expect("assigned Fahrer has an id");
                    let rating = *ratings.get(&driver_id).unwrap_or(&1500);
                    let opponents = (participants.len() - 1) as f64;
                    let (actual, expected) = participants
                        .iter()
                        .filter(|other| other.driver_id != result.driver_id)
                        .fold((0.0, 0.0), |(actual, expected), other| {
                            let other_rating = *ratings
                                .get(&other.driver_id.expect("assigned Fahrer has an id"))
                                .unwrap_or(&1500);
                            (
                                actual + actual_score(result, other),
                                expected
                                    + 1.0
                                        / (1.0
                                            + 10f64.powf((other_rating - rating) as f64 / 400.0)),
                            )
                        });
                    EloDelta {
                        driver_id,
                        // Average first, then deterministically round the race delta once.
                        delta: (32.0 * (actual / opponents - expected / opponents)).round() as i64,
                    }
                })
                .collect::<Vec<_>>()
        };
        deltas.sort_by_key(|delta| delta.driver_id);
        for delta in &deltas {
            *ratings.entry(delta.driver_id).or_insert(1500) += delta.delta;
        }
        races.push(EloRaceDelta {
            race_id: race.race_id,
            deltas,
        });
    }

    Ok(EloSummary {
        ratings: ratings
            .into_iter()
            .map(|(driver_id, rating)| EloRating { driver_id, rating })
            .collect(),
        races,
    })
}

fn actual_score(result: &RaceResult, other: &RaceResult) -> f64 {
    let metric = |result: &RaceResult| {
        (
            std::cmp::Reverse(result.corrected_laps_thousandths),
            result.result_time_ms.unwrap_or(u64::MAX),
        )
    };
    match metric(result).cmp(&metric(other)) {
        std::cmp::Ordering::Less => 1.0,
        std::cmp::Ordering::Equal => 0.5,
        std::cmp::Ordering::Greater => 0.0,
    }
}

fn validate_active_drivers(
    connection: &Connection,
    driver_ids: &[Option<i64>],
) -> Result<(), StoreError> {
    for id in driver_ids.iter().flatten() {
        let active: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM drivers WHERE id = ?1 AND archived_at IS NULL)",
            [id],
            |row| row.get(0),
        )?;
        if !active {
            return Err(StoreError::DriverNotActive(*id));
        }
    }
    Ok(())
}

fn valid_race_id(race_id: &str) -> Result<&str, StoreError> {
    if race_id.trim().is_empty() {
        Err(StoreError::InvalidRaceId)
    } else {
        Ok(race_id)
    }
}

fn driver_name(display_name: &str) -> Result<&str, StoreError> {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        Err(StoreError::InvalidDriverName)
    } else {
        Ok(display_name)
    }
}

fn driver_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Driver> {
    Ok(Driver {
        id: row.get(0)?,
        display_name: row.get(1)?,
        archived_at: row.get(2)?,
    })
}

fn load_driver(connection: &Connection, id: i64) -> Result<Driver, StoreError> {
    connection
        .query_row(
            "SELECT id, display_name, archived_at FROM drivers WHERE id = ?1",
            [id],
            driver_from_row,
        )
        .map_err(StoreError::from)
}

fn replay(events: &[Event]) -> Result<RaceEngine, StoreError> {
    RaceEngine::replay(events)
        .map_err(|error| StoreError::CorruptProtocol(format!("invalid event history: {error:?}")))
}

fn snapshot(connection: &Connection, race_id: &str) -> Result<StateSnapshot, StoreError> {
    let events = load_events(connection, race_id)?;
    Ok(StateSnapshot {
        race_id: race_id.to_owned(),
        sequence: events.len() as u64,
        state: replay(&events)?.state().clone(),
    })
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
    fn sqlite_connections_use_wal_full_synchronous_and_foreign_keys() {
        let dir = tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
        let connection = store.connect().unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        let foreign_keys: bool = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();

        assert_eq!(journal_mode, "wal");
        assert_eq!(synchronous, 2); // SQLite FULL
        assert!(foreign_keys);
    }

    #[test]
    fn elo_read_helpers_share_one_snapshot() {
        let dir = tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
        store.create_driver("Ada").unwrap();

        let mut connection = store.connect().unwrap();
        let transaction = connection.transaction().unwrap();
        assert_eq!(load_drivers(&transaction).unwrap().len(), 1);
        store.create_driver("Grace").unwrap();
        assert_eq!(load_drivers(&transaction).unwrap().len(), 1);
        transaction.commit().unwrap();
        assert_eq!(store.drivers().unwrap().len(), 2);
    }
}
