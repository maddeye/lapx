use super::{
    RaceResult, SqliteStore, StoreError, derive_elo, load_events, replay, validate_active_drivers,
};
use crate::domain::Event;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TournamentGenerationMode {
    Random,
    EloBalanced,
}

impl TournamentGenerationMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::EloBalanced => "elo_balanced",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "random" => Some(Self::Random),
            "elo_balanced" => Some(Self::EloBalanced),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TournamentGeneration {
    pub mode: TournamentGenerationMode,
    #[serde(with = "decimal_u64")]
    pub seed: u64,
    pub lane_count: u8,
}

mod decimal_u64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Number(u64),
            Decimal(String),
        }

        match Value::deserialize(deserializer)? {
            Value::Number(value) => Ok(value),
            Value::Decimal(value) => value.parse().map_err(D::Error::custom),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tournament {
    pub id: i64,
    pub name: String,
    pub generation: Option<TournamentGeneration>,
    pub heats: Vec<TournamentHeat>,
}

impl SqliteStore {
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

    pub fn create_generated_tournament(
        &self,
        name: &str,
        driver_ids: &[i64],
        lane_count: u8,
        mode: TournamentGenerationMode,
        seed: u64,
    ) -> Result<Tournament, StoreError> {
        let name = tournament_name(name)?;
        let mut driver_ids = driver_ids.to_vec();
        driver_ids.sort_unstable();
        if driver_ids.len() < 2
            || !(1..=4).contains(&lane_count)
            || driver_ids.iter().any(|id| *id <= 0)
            || driver_ids.windows(2).any(|ids| ids[0] == ids[1])
        {
            return Err(StoreError::InvalidTournamentGeneration);
        }

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_active_drivers(
            &transaction,
            &driver_ids.iter().copied().map(Some).collect::<Vec<_>>(),
        )?;
        let heats = generate_heats(&transaction, &driver_ids, lane_count, mode, seed)?;

        transaction.execute("INSERT INTO tournaments (name) VALUES (?1)", [name])?;
        let id = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO tournament_generation (tournament_id, mode, seed, lane_count) VALUES (?1, ?2, ?3, ?4)",
            params![id, mode.as_str(), seed.to_string(), lane_count],
        )?;
        for (index, drivers) in heats.iter().enumerate() {
            let assignments: Vec<_> = drivers
                .iter()
                .enumerate()
                .map(|(lane, driver_id)| HeatAssignment {
                    lane: lane as u8 + 1,
                    driver_id: *driver_id,
                })
                .collect();
            insert_heat(&transaction, id, index as u64 + 1, &assignments)?;
        }
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
        insert_heat(&transaction, tournament_id, position as u64, assignments)?;
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

fn insert_heat(
    connection: &Connection,
    tournament_id: i64,
    position: u64,
    assignments: &[HeatAssignment],
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO tournament_heats (tournament_id, position) VALUES (?1, ?2)",
        params![tournament_id, position],
    )?;
    let heat_id = connection.last_insert_rowid();
    for assignment in assignments {
        connection.execute(
            "INSERT INTO tournament_heat_assignments (heat_id, lane, driver_id) VALUES (?1, ?2, ?3)",
            params![heat_id, assignment.lane, assignment.driver_id],
        )?;
    }
    Ok(())
}

fn generate_heats(
    connection: &Connection,
    canonical_driver_ids: &[i64],
    lane_count: u8,
    mode: TournamentGenerationMode,
    seed: u64,
) -> Result<Vec<Vec<i64>>, StoreError> {
    let mut rng = seed;
    let ordered = match mode {
        TournamentGenerationMode::Random => {
            let mut drivers = canonical_driver_ids.to_vec();
            // Fixed generation contract: SplitMix64, then one reverse Fisher-Yates shuffle.
            for index in (1..drivers.len()).rev() {
                let swap = (splitmix64(&mut rng) % (index as u64 + 1)) as usize;
                drivers.swap(index, swap);
            }
            drivers
        }
        TournamentGenerationMode::EloBalanced => {
            let ratings: BTreeMap<_, _> = derive_elo(connection)?
                .ratings
                .into_iter()
                .map(|rating| (rating.driver_id, rating.rating))
                .collect();
            let mut drivers: Vec<_> = canonical_driver_ids
                .iter()
                .map(|id| (*id, *ratings.get(id).unwrap_or(&1500), splitmix64(&mut rng)))
                .collect();
            drivers.sort_by_key(|(id, rating, tie)| (std::cmp::Reverse(*rating), *tie, *id));
            drivers.into_iter().map(|(id, _, _)| id).collect()
        }
    };

    let lanes = lane_count as usize;
    if mode == TournamentGenerationMode::Random {
        return Ok(ordered.chunks(lanes).map(<[_]>::to_vec).collect());
    }
    let heat_count = ordered.len().div_ceil(lanes);
    let mut heats = vec![Vec::new(); heat_count];
    for (index, driver_id) in ordered.into_iter().enumerate() {
        let row = index / heat_count;
        let offset = index % heat_count;
        let heat = if row % 2 == 0 {
            offset
        } else {
            heat_count - 1 - offset
        };
        heats[heat].push(driver_id);
    }
    Ok(heats)
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
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
    let generation = connection
        .query_row(
            "SELECT mode, seed, lane_count FROM tournament_generation WHERE tournament_id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u8>(2)?,
                ))
            },
        )
        .optional()?
        .map(|(mode, seed, lane_count)| -> Result<_, StoreError> {
            Ok(TournamentGeneration {
                mode: TournamentGenerationMode::from_str(&mode).ok_or_else(|| {
                    StoreError::CorruptProtocol(format!("invalid tournament mode {mode}"))
                })?,
                seed: seed.parse().map_err(|_| {
                    StoreError::CorruptProtocol(format!("invalid tournament seed {seed}"))
                })?,
                lane_count,
            })
        })
        .transpose()?;
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
    Ok(Tournament {
        id,
        name,
        generation,
        heats,
    })
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
