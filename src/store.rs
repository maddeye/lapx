use crate::domain::{Command, DomainError, Event, RaceEngine, RaceState, RaceStatus};
use rusqlite::{Connection, ErrorCode, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    ops::Deref,
    path::{Path, PathBuf},
    time::Duration,
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
pub struct HeatAssignment {
    pub lane: u8,
    pub driver_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TournamentHeat {
    pub id: i64,
    pub position: u64,
    pub assignments: Vec<HeatAssignment>,
    pub race_id: Option<String>,
    pub results: Option<Vec<RaceResult>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tournament {
    pub id: i64,
    pub name: String,
    pub heats: Vec<TournamentHeat>,
}

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

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Domain(DomainError),
    InvalidDriverName,
    DriverNotFound(i64),
    DriverNotActive(i64),
    InvalidTournamentName,
    TournamentNotFound(i64),
    HeatNotFound(i64),
    InvalidHeatAssignments,
    TournamentFrozen(i64),
    HeatAlreadyLinked(i64),
    RaceNotFound(String),
    RaceAlreadyLinked(String),
    RaceAssignmentsMismatch,
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
        let mut statement =
            connection.prepare("SELECT id, display_name, archived_at FROM drivers ORDER BY id")?;
        let rows = statement.query_map([], driver_from_row)?;
        rows.collect::<Result<_, _>>().map_err(StoreError::from)
    }

    pub fn completed_races(&self) -> Result<Vec<CompletedRace>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let race_ids = {
            let mut statement = transaction.prepare(
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
            let events = load_events(&transaction, &race_id)?;
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

    pub fn tournaments(&self) -> Result<Vec<Tournament>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let ids = {
            let mut statement = transaction.prepare("SELECT id FROM tournaments ORDER BY id")?;
            statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let tournaments = ids
            .into_iter()
            .map(|id| load_tournament(&transaction, id))
            .collect::<Result<_, _>>()?;
        transaction.commit()?;
        Ok(tournaments)
    }

    pub fn tournament(&self, id: i64) -> Result<Tournament, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let tournament = load_tournament(&transaction, id)?;
        transaction.commit()?;
        Ok(tournament)
    }

    pub fn create_tournament(&self, name: &str) -> Result<Tournament, StoreError> {
        let name = tournament_name(name)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("INSERT INTO tournaments (name) VALUES (?1)", [name])?;
        let id = transaction.last_insert_rowid();
        let tournament = load_tournament(&transaction, id)?;
        transaction.commit()?;
        Ok(tournament)
    }

    pub fn append_heat(
        &self,
        tournament_id: i64,
        assignments: &[HeatAssignment],
    ) -> Result<Tournament, StoreError> {
        validate_heat_assignments(assignments)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_tournament(&transaction, tournament_id)?;
        if tournament_started(&transaction, tournament_id)? {
            return Err(StoreError::TournamentFrozen(tournament_id));
        }
        validate_active_drivers(
            &transaction,
            &assignments
                .iter()
                .map(|assignment| Some(assignment.driver_id))
                .collect::<Vec<_>>(),
        )?;
        let position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position), 0) + 1 FROM tournament_heats WHERE tournament_id = ?1",
            [tournament_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO tournament_heats (tournament_id, position) VALUES (?1, ?2)",
            params![tournament_id, position],
        )?;
        let heat_id = transaction.last_insert_rowid();
        for assignment in assignments {
            transaction.execute(
                "INSERT INTO tournament_heat_assignments (heat_id, lane, driver_id) VALUES (?1, ?2, ?3)",
                params![heat_id, assignment.lane, assignment.driver_id],
            )?;
        }
        let tournament = load_tournament(&transaction, tournament_id)?;
        transaction.commit()?;
        Ok(tournament)
    }

    pub fn link_heat(
        &self,
        tournament_id: i64,
        heat_id: i64,
        race_id: &str,
    ) -> Result<Tournament, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_tournament(&transaction, tournament_id)?;
        let linked: Option<String> = transaction
            .query_row(
                "SELECT race_id FROM tournament_heats WHERE id = ?1 AND tournament_id = ?2",
                params![heat_id, tournament_id],
                |row| row.get(0),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => StoreError::HeatNotFound(heat_id),
                error => StoreError::Sqlite(error),
            })?;
        if linked.is_some() {
            return Err(StoreError::HeatAlreadyLinked(heat_id));
        }
        let already_linked: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM tournament_heats WHERE race_id = ?1)",
            [race_id],
            |row| row.get(0),
        )?;
        if already_linked {
            return Err(StoreError::RaceAlreadyLinked(race_id.to_owned()));
        }
        let events = load_events(&transaction, race_id)?;
        let config = events
            .iter()
            .find_map(|event| match event {
                Event::RaceConfigured { config, .. } => Some(config),
                _ => None,
            })
            .ok_or_else(|| StoreError::RaceNotFound(race_id.to_owned()))?;
        let assignments = load_assignments(&transaction, heat_id)?;
        let expected: Vec<_> = assignments
            .iter()
            .map(|assignment| Some(assignment.driver_id))
            .collect();
        if config.lanes as usize != assignments.len() || config.driver_ids != expected {
            return Err(StoreError::RaceAssignmentsMismatch);
        }
        replay(&events)?;
        transaction.execute(
            "UPDATE tournament_heats SET race_id = ?1 WHERE id = ?2",
            params![race_id, heat_id],
        )?;
        let tournament = load_tournament(&transaction, tournament_id)?;
        transaction.commit()?;
        Ok(tournament)
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

fn tournament_name(name: &str) -> Result<&str, StoreError> {
    let name = name.trim();
    if name.is_empty() {
        Err(StoreError::InvalidTournamentName)
    } else {
        Ok(name)
    }
}

fn validate_heat_assignments(assignments: &[HeatAssignment]) -> Result<(), StoreError> {
    let lanes: BTreeSet<_> = assignments
        .iter()
        .map(|assignment| assignment.lane)
        .collect();
    if assignments.is_empty()
        || assignments.len() > 4
        || lanes != (1..=assignments.len() as u8).collect()
        || assignments
            .iter()
            .any(|assignment| assignment.driver_id <= 0)
        || assignments
            .iter()
            .map(|assignment| assignment.driver_id)
            .collect::<BTreeSet<_>>()
            .len()
            != assignments.len()
    {
        Err(StoreError::InvalidHeatAssignments)
    } else {
        Ok(())
    }
}

fn require_tournament(connection: &Connection, id: i64) -> Result<(), StoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tournaments WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::TournamentNotFound(id))
    }
}

fn tournament_started(connection: &Connection, tournament_id: i64) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM tournament_heats heat
                JOIN race_events event ON event.race_id = heat.race_id
                WHERE heat.tournament_id = ?1 AND event.event_type = 'official_start'
            )",
            [tournament_id],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn load_tournament(connection: &Connection, id: i64) -> Result<Tournament, StoreError> {
    let name = connection
        .query_row("SELECT name FROM tournaments WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => StoreError::TournamentNotFound(id),
            error => StoreError::Sqlite(error),
        })?;
    let rows = {
        let mut statement = connection.prepare(
            "SELECT id, position, race_id FROM tournament_heats WHERE tournament_id = ?1 ORDER BY position",
        )?;
        statement
            .query_map([id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut heats = Vec::with_capacity(rows.len());
    for (heat_id, position, race_id) in rows {
        let assignments = load_assignments(connection, heat_id)?;
        let results = race_id
            .as_deref()
            .map(|race_id| current_results(connection, race_id, &assignments))
            .transpose()?;
        heats.push(TournamentHeat {
            id: heat_id,
            position,
            assignments,
            race_id,
            results,
        });
    }
    Ok(Tournament { id, name, heats })
}

fn load_assignments(
    connection: &Connection,
    heat_id: i64,
) -> Result<Vec<HeatAssignment>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT lane, driver_id FROM tournament_heat_assignments WHERE heat_id = ?1 ORDER BY lane",
    )?;
    statement
        .query_map([heat_id], |row| {
            Ok(HeatAssignment {
                lane: row.get(0)?,
                driver_id: row.get(1)?,
            })
        })?
        .collect::<Result<_, _>>()
        .map_err(StoreError::from)
}

fn current_results(
    connection: &Connection,
    race_id: &str,
    assignments: &[HeatAssignment],
) -> Result<Vec<RaceResult>, StoreError> {
    let events = load_events(connection, race_id)?;
    let state = replay(&events)?.state().clone();
    Ok(state
        .standings()
        .into_iter()
        .enumerate()
        .map(|(index, standing)| RaceResult {
            position: index as u8 + 1,
            lane: standing.lane,
            driver_id: assignments
                .iter()
                .find(|assignment| assignment.lane == standing.lane)
                .map(|assignment| assignment.driver_id),
            corrected_laps_thousandths: standing.corrected_laps_thousandths,
            result_time_ms: standing.result_time_ms,
            best_lap_ms: state.lane(standing.lane).and_then(|lane| lane.best_lap_ms),
        })
        .collect())
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
}
