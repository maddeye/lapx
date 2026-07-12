use crate::{
    domain::{ChaosSource, Command, RaceConfig, SignalEdge},
    hardware::HardwareSnapshot,
    runtime::{RaceRuntime, RuntimeError, StateSnapshot},
    store::{CompletedRace, Driver, DriverStats, HeatAssignment, StoreError, Tournament},
};
use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{Html, IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, sync::Arc};

mod assets;

/// Read-only surface safe for the public bind: state, SSE, and Rennscreen assets.
/// Control-only assets are denied here (see `assets::public_app_asset`).
pub fn public_router(runtime: Arc<RaceRuntime>) -> Router {
    read_router(runtime).route("/_app/{*path}", get(assets::public_app_asset))
}

fn read_router(runtime: Arc<RaceRuntime>) -> Router {
    Router::new()
        .route("/", get(assets::rennscreen))
        .route("/api/state", get(state))
        .route("/api/state/stream", get(state_stream))
        .with_state(runtime)
}

/// Full surface for the loopback bind: public reads plus all mutating and debug routes.
pub fn local_router(runtime: Arc<RaceRuntime>) -> Router {
    Router::new()
        .route("/control", get(assets::control))
        .route("/admin", get(assets::admin))
        .route("/_app/{*path}", get(assets::app_asset))
        .route("/api/drivers", get(drivers).post(create_driver))
        .route("/api/race-history", get(race_history))
        .route("/api/driver-stats", get(driver_stats))
        .route("/api/tournaments", get(tournaments).post(create_tournament))
        .route("/api/tournaments/{id}", get(tournament))
        .route("/api/tournaments/{id}/heats", post(append_heat))
        .route(
            "/api/tournaments/{id}/heats/{heat_id}/link",
            post(link_heat),
        )
        .route("/api/drivers/{id}/rename", post(rename_driver))
        .route("/api/drivers/{id}/archive", post(archive_driver))
        .route("/api/hardware", get(hardware_state))
        .route("/api/start", post(start))
        .route("/api/sensor", post(sensor))
        .route("/api/pause", post(pause))
        .route("/api/resume", post(resume))
        .route("/api/chaos", post(chaos))
        .route("/api/correct-laps", post(correct_laps))
        .route("/debug", get(debug))
        .route("/hardware", get(hardware))
        .with_state(runtime.clone())
        .merge(read_router(runtime))
}

/// Snapshot plus derived display timing; serializes as a superset of `StateSnapshot`.
/// One schema for GET /api/state, the SSE stream, and every command response.
#[derive(Serialize)]
struct HttpState {
    #[serde(flatten)]
    snapshot: StateSnapshot,
    race_elapsed_ms: Option<u64>,
    race_clock_running: bool,
    protocol_now: u64,
}

fn http_state(runtime: &RaceRuntime, snapshot: StateSnapshot) -> Result<HttpState, HttpError> {
    let protocol_now = runtime.protocol_now()?;
    Ok(HttpState {
        race_elapsed_ms: snapshot.state.race_elapsed_ms(protocol_now),
        race_clock_running: snapshot.state.race_clock_running(),
        protocol_now,
        snapshot,
    })
}

#[derive(Deserialize)]
struct StartInput {
    config: RaceConfig,
}

#[derive(Deserialize)]
struct SensorInput {
    lane: u8,
    edge: SignalEdge,
}

#[derive(Deserialize)]
struct ChaosInput {
    source: ChaosSource,
}

#[derive(Deserialize)]
struct CorrectionInput {
    lane: u8,
    delta_thousandths: i64,
}

#[derive(Deserialize)]
struct DriverNameInput {
    display_name: String,
}

#[derive(Deserialize)]
struct TournamentNameInput {
    name: String,
}

#[derive(Deserialize)]
struct HeatInput {
    assignments: Vec<HeatAssignment>,
}

#[derive(Deserialize)]
struct LinkHeatInput {
    race_id: String,
}

async fn drivers(State(runtime): State<Arc<RaceRuntime>>) -> Result<Json<Vec<Driver>>, HttpError> {
    let store = runtime.store();
    Ok(Json(store_task(move || store.drivers()).await?))
}

async fn race_history(
    State(runtime): State<Arc<RaceRuntime>>,
) -> Result<Json<Vec<CompletedRace>>, HttpError> {
    let store = runtime.store();
    Ok(Json(store_task(move || store.completed_races()).await?))
}

async fn driver_stats(
    State(runtime): State<Arc<RaceRuntime>>,
) -> Result<Json<Vec<DriverStats>>, HttpError> {
    let store = runtime.store();
    Ok(Json(store_task(move || store.driver_stats()).await?))
}

async fn tournaments(
    State(runtime): State<Arc<RaceRuntime>>,
) -> Result<Json<Vec<Tournament>>, HttpError> {
    let store = runtime.store();
    Ok(Json(store_task(move || store.tournaments()).await?))
}

async fn tournament(
    State(runtime): State<Arc<RaceRuntime>>,
    Path(id): Path<i64>,
) -> Result<Json<Tournament>, HttpError> {
    let store = runtime.store();
    Ok(Json(store_task(move || store.tournament(id)).await?))
}

async fn create_tournament(
    State(runtime): State<Arc<RaceRuntime>>,
    input: Result<Json<TournamentNameInput>, JsonRejection>,
) -> Result<Json<Tournament>, HttpError> {
    let input = parse(input)?;
    let store = runtime.store();
    Ok(Json(
        store_task(move || store.create_tournament(&input.name)).await?,
    ))
}

async fn append_heat(
    State(runtime): State<Arc<RaceRuntime>>,
    Path(id): Path<i64>,
    input: Result<Json<HeatInput>, JsonRejection>,
) -> Result<Json<Tournament>, HttpError> {
    let input = parse(input)?;
    let store = runtime.store();
    Ok(Json(
        store_task(move || store.append_heat(id, &input.assignments)).await?,
    ))
}

async fn link_heat(
    State(runtime): State<Arc<RaceRuntime>>,
    Path((id, heat_id)): Path<(i64, i64)>,
    input: Result<Json<LinkHeatInput>, JsonRejection>,
) -> Result<Json<Tournament>, HttpError> {
    let input = parse(input)?;
    let store = runtime.store();
    Ok(Json(
        store_task(move || store.link_heat(id, heat_id, &input.race_id)).await?,
    ))
}

async fn create_driver(
    State(runtime): State<Arc<RaceRuntime>>,
    input: Result<Json<DriverNameInput>, JsonRejection>,
) -> Result<Json<Driver>, HttpError> {
    let input = parse(input)?;
    let store = runtime.store();
    Ok(Json(
        store_task(move || store.create_driver(&input.display_name)).await?,
    ))
}

async fn rename_driver(
    State(runtime): State<Arc<RaceRuntime>>,
    Path(id): Path<i64>,
    input: Result<Json<DriverNameInput>, JsonRejection>,
) -> Result<Json<Driver>, HttpError> {
    let input = parse(input)?;
    let store = runtime.store();
    Ok(Json(
        store_task(move || store.rename_driver(id, &input.display_name)).await?,
    ))
}

async fn archive_driver(
    State(runtime): State<Arc<RaceRuntime>>,
    Path(id): Path<i64>,
    input: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<Driver>, HttpError> {
    parse(input)?;
    let store = runtime.store();
    Ok(Json(store_task(move || store.archive_driver(id)).await?))
}

async fn store_task<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
) -> Result<T, HttpError> {
    tokio::task::spawn_blocking(task)
        .await
        .map_err(RuntimeError::from)?
        .map_err(HttpError::Store)
}

async fn state(State(runtime): State<Arc<RaceRuntime>>) -> Result<Json<HttpState>, HttpError> {
    let snapshot = runtime.snapshot().await?;
    Ok(Json(http_state(&runtime, snapshot)?))
}

async fn hardware_state(
    State(runtime): State<Arc<RaceRuntime>>,
) -> Result<Json<HardwareSnapshot>, HttpError> {
    if runtime.hardware_snapshot().is_none() {
        return Err(HttpError::HardwareUnavailable);
    }
    runtime.snapshot().await?;
    runtime
        .hardware_snapshot()
        .map(Json)
        .ok_or(HttpError::HardwareUnavailable)
}

async fn start(
    State(runtime): State<Arc<RaceRuntime>>,
    input: Result<Json<StartInput>, JsonRejection>,
) -> Result<Json<HttpState>, HttpError> {
    let input = parse(input)?;
    let snapshot = runtime
        .apply_now(|at| Command::StartRace {
            config: input.config,
            at,
        })
        .await?;
    Ok(Json(http_state(&runtime, snapshot)?))
}

async fn sensor(
    State(runtime): State<Arc<RaceRuntime>>,
    input: Result<Json<SensorInput>, JsonRejection>,
) -> Result<Json<HttpState>, HttpError> {
    let input = parse(input)?;
    let snapshot = runtime
        .apply_now(move |at| Command::SensorTriggered {
            lane: input.lane,
            at,
            edge: input.edge,
        })
        .await?;
    Ok(Json(http_state(&runtime, snapshot)?))
}

async fn pause(State(runtime): State<Arc<RaceRuntime>>) -> Result<Json<HttpState>, HttpError> {
    let snapshot = runtime.apply_now(|at| Command::PauseRace { at }).await?;
    Ok(Json(http_state(&runtime, snapshot)?))
}

async fn resume(State(runtime): State<Arc<RaceRuntime>>) -> Result<Json<HttpState>, HttpError> {
    let snapshot = runtime.apply_now(|at| Command::ResumeRace { at }).await?;
    Ok(Json(http_state(&runtime, snapshot)?))
}

async fn chaos(
    State(runtime): State<Arc<RaceRuntime>>,
    input: Result<Json<ChaosInput>, JsonRejection>,
) -> Result<Json<HttpState>, HttpError> {
    let input = parse(input)?;
    let snapshot = runtime
        .apply_now(move |at| Command::TriggerChaos {
            source: input.source,
            at,
        })
        .await?;
    Ok(Json(http_state(&runtime, snapshot)?))
}

async fn correct_laps(
    State(runtime): State<Arc<RaceRuntime>>,
    input: Result<Json<CorrectionInput>, JsonRejection>,
) -> Result<Json<HttpState>, HttpError> {
    let input = parse(input)?;
    let snapshot = runtime
        .apply_now(move |at| Command::CorrectLaps {
            lane: input.lane,
            delta_thousandths: input.delta_thousandths,
            at,
        })
        .await?;
    Ok(Json(http_state(&runtime, snapshot)?))
}

async fn state_stream(
    State(runtime): State<Arc<RaceRuntime>>,
) -> Result<impl IntoResponse, HttpError> {
    let mut receiver = runtime.subscribe();
    let initial = runtime.snapshot().await?;
    let initial = state_event(&runtime, initial)?;
    let states = stream! {
        let mut sequence = initial.1;
        yield Ok::<_, Infallible>(initial.0);
        loop {
            let snapshot = match receiver.recv().await {
                Ok(snapshot) => snapshot,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    match runtime.snapshot().await {
                        Ok(snapshot) => snapshot,
                        Err(_) => break,
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if snapshot.sequence > sequence {
                sequence = snapshot.sequence;
                // Clock failure ends the stream; the client reconnects.
                match state_event(&runtime, snapshot) {
                    Ok((event, _)) => yield Ok(event),
                    Err(_) => break,
                }
            }
        }
    };
    Ok(Sse::new(states))
}

fn parse<T>(input: Result<Json<T>, JsonRejection>) -> Result<T, HttpError> {
    input
        .map(|input| input.0)
        .map_err(|error| HttpError::Malformed(error.to_string()))
}

fn state_event(runtime: &RaceRuntime, snapshot: StateSnapshot) -> Result<(Event, u64), HttpError> {
    let sequence = snapshot.sequence;
    let event = Event::default()
        .event("state")
        .id(sequence.to_string())
        .data(
            serde_json::to_string(&http_state(runtime, snapshot)?)
                .expect("state snapshot is serializable"),
        );
    Ok((event, sequence))
}

async fn debug() -> Html<&'static str> {
    Html(include_str!("../debug.html"))
}

async fn hardware() -> Html<&'static str> {
    Html(include_str!("../hardware.html"))
}

#[derive(Debug)]
enum HttpError {
    Malformed(String),
    HardwareUnavailable,
    Store(StoreError),
    Runtime(RuntimeError),
}

impl From<RuntimeError> for HttpError {
    fn from(value: RuntimeError) -> Self {
        Self::Runtime(value)
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        if let Self::Runtime(RuntimeError::PowerAfterCommit { snapshot, source }) = &self {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": source.to_string(),
                    "committed": snapshot,
                })),
            )
                .into_response();
        }
        let status = match &self {
            Self::Malformed(_)
            | Self::Store(
                StoreError::InvalidDriverName
                | StoreError::DriverNotActive(_)
                | StoreError::InvalidTournamentName
                | StoreError::InvalidHeatAssignments
                | StoreError::RaceAssignmentsMismatch,
            )
            | Self::Runtime(RuntimeError::Store(
                StoreError::Domain(_) | StoreError::DriverNotActive(_),
            ))
            | Self::Runtime(RuntimeError::HardwareLaneMismatch { .. }) => StatusCode::BAD_REQUEST,
            Self::HardwareUnavailable
            | Self::Store(
                StoreError::DriverNotFound(_)
                | StoreError::TournamentNotFound(_)
                | StoreError::HeatNotFound(_)
                | StoreError::RaceNotFound(_),
            ) => StatusCode::NOT_FOUND,
            Self::Store(
                StoreError::TournamentFrozen(_)
                | StoreError::HeatAlreadyLinked(_)
                | StoreError::RaceAlreadyLinked(_),
            ) => StatusCode::CONFLICT,
            Self::Store(error) | Self::Runtime(RuntimeError::Store(error)) if error.is_busy() => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = match &self {
            Self::Malformed(message) => message.clone(),
            Self::Store(StoreError::DriverNotActive(id))
            | Self::Runtime(RuntimeError::Store(StoreError::DriverNotActive(id))) => {
                format!("driver {id} is missing or archived")
            }
            Self::Runtime(error) => error.to_string(),
            Self::HardwareUnavailable => "hardware is not configured".into(),
            Self::Store(StoreError::InvalidDriverName) => "display_name must not be blank".into(),
            Self::Store(StoreError::DriverNotFound(id)) => format!("driver {id} not found"),
            Self::Store(StoreError::InvalidTournamentName) => "name must not be blank".into(),
            Self::Store(StoreError::TournamentNotFound(id)) => {
                format!("tournament {id} not found")
            }
            Self::Store(StoreError::HeatNotFound(id)) => format!("heat {id} not found"),
            Self::Store(StoreError::InvalidHeatAssignments) => {
                "assign lanes 1 through 4 once, with a different active driver per lane".into()
            }
            Self::Store(StoreError::TournamentFrozen(id)) => {
                format!("tournament {id} is frozen because a linked heat has started")
            }
            Self::Store(StoreError::HeatAlreadyLinked(id)) => {
                format!("heat {id} is already linked")
            }
            Self::Store(StoreError::RaceNotFound(id)) => format!("race {id} not found"),
            Self::Store(StoreError::RaceAlreadyLinked(id)) => {
                format!("race {id} is already linked")
            }
            Self::Store(StoreError::RaceAssignmentsMismatch) => {
                "race Fahrer/lane assignments do not match the heat".into()
            }
            Self::Store(error) => error.to_string(),
        };
        (status, message).into_response()
    }
}
