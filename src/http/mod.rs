use crate::{
    domain::{ChaosSource, Command, RaceConfig, SignalEdge},
    hardware::HardwareSnapshot,
    runtime::{RaceRuntime, RuntimeError, StateSnapshot},
    store::StoreError,
};
use async_stream::stream;
use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{Html, IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, sync::Arc};

mod assets;

/// Read-only surface safe for the public bind: state, SSE, and Rennscreen assets.
pub fn public_router(runtime: Arc<RaceRuntime>) -> Router {
    Router::new()
        .route("/", get(assets::rennscreen))
        .route("/_app/{*path}", get(assets::app_asset))
        .route("/api/state", get(state))
        .route("/api/state/stream", get(state_stream))
        .with_state(runtime)
}

/// Full surface for the loopback bind: public reads plus all mutating and debug routes.
pub fn local_router(runtime: Arc<RaceRuntime>) -> Router {
    Router::new()
        .route("/control", get(assets::control))
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
        .merge(public_router(runtime))
}

/// Snapshot plus derived display timing; serializes as a superset of `StateSnapshot`.
#[derive(Serialize)]
struct HttpState {
    #[serde(flatten)]
    snapshot: StateSnapshot,
    race_elapsed_ms: Option<u64>,
}

fn http_state(runtime: &RaceRuntime, snapshot: StateSnapshot) -> HttpState {
    let race_elapsed_ms = runtime
        .protocol_now()
        .ok()
        .and_then(|now| snapshot.state.race_elapsed_ms(now));
    HttpState {
        snapshot,
        race_elapsed_ms,
    }
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

async fn state(State(runtime): State<Arc<RaceRuntime>>) -> Result<Json<HttpState>, HttpError> {
    let snapshot = runtime.snapshot().await?;
    Ok(Json(http_state(&runtime, snapshot)))
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
) -> Result<Json<StateSnapshot>, HttpError> {
    let input = parse(input)?;
    Ok(Json(
        runtime
            .apply_now(|at| Command::StartRace {
                config: input.config,
                at,
            })
            .await?,
    ))
}

async fn sensor(
    State(runtime): State<Arc<RaceRuntime>>,
    input: Result<Json<SensorInput>, JsonRejection>,
) -> Result<Json<StateSnapshot>, HttpError> {
    let input = parse(input)?;
    Ok(Json(
        runtime
            .apply_now(move |at| Command::SensorTriggered {
                lane: input.lane,
                at,
                edge: input.edge,
            })
            .await?,
    ))
}

async fn pause(State(runtime): State<Arc<RaceRuntime>>) -> Result<Json<StateSnapshot>, HttpError> {
    Ok(Json(
        runtime.apply_now(|at| Command::PauseRace { at }).await?,
    ))
}

async fn resume(State(runtime): State<Arc<RaceRuntime>>) -> Result<Json<StateSnapshot>, HttpError> {
    Ok(Json(
        runtime.apply_now(|at| Command::ResumeRace { at }).await?,
    ))
}

async fn chaos(
    State(runtime): State<Arc<RaceRuntime>>,
    input: Result<Json<ChaosInput>, JsonRejection>,
) -> Result<Json<StateSnapshot>, HttpError> {
    let input = parse(input)?;
    Ok(Json(
        runtime
            .apply_now(move |at| Command::TriggerChaos {
                source: input.source,
                at,
            })
            .await?,
    ))
}

async fn correct_laps(
    State(runtime): State<Arc<RaceRuntime>>,
    input: Result<Json<CorrectionInput>, JsonRejection>,
) -> Result<Json<StateSnapshot>, HttpError> {
    let input = parse(input)?;
    Ok(Json(
        runtime
            .apply_now(move |at| Command::CorrectLaps {
                lane: input.lane,
                delta_thousandths: input.delta_thousandths,
                at,
            })
            .await?,
    ))
}

async fn state_stream(
    State(runtime): State<Arc<RaceRuntime>>,
) -> Result<impl IntoResponse, HttpError> {
    let mut receiver = runtime.subscribe();
    let initial = runtime.snapshot().await?;
    let states = stream! {
        let mut sequence = initial.sequence;
        yield Ok::<_, Infallible>(state_event(&runtime, initial));
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
                yield Ok(state_event(&runtime, snapshot));
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

fn state_event(runtime: &RaceRuntime, snapshot: StateSnapshot) -> Event {
    let sequence = snapshot.sequence;
    Event::default()
        .event("state")
        .id(sequence.to_string())
        .data(
            serde_json::to_string(&http_state(runtime, snapshot))
                .expect("state snapshot is serializable"),
        )
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
            | Self::Runtime(RuntimeError::Store(StoreError::Domain(_)))
            | Self::Runtime(RuntimeError::HardwareLaneMismatch { .. }) => StatusCode::BAD_REQUEST,
            Self::HardwareUnavailable => StatusCode::NOT_FOUND,
            Self::Runtime(RuntimeError::Store(error)) if error.is_busy() => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = match &self {
            Self::Malformed(message) => message.clone(),
            Self::Runtime(error) => error.to_string(),
            Self::HardwareUnavailable => "hardware is not configured".into(),
        };
        (status, message).into_response()
    }
}
