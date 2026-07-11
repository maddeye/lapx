use crate::{
    domain::{ChaosSource, Command, RaceConfig, SignalEdge},
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
use serde::Deserialize;
use std::{convert::Infallible, sync::Arc};

pub fn router(runtime: Arc<RaceRuntime>) -> Router {
    Router::new()
        .route("/api/state", get(state))
        .route("/api/state/stream", get(state_stream))
        .route("/api/start", post(start))
        .route("/api/sensor", post(sensor))
        .route("/api/pause", post(pause))
        .route("/api/resume", post(resume))
        .route("/api/chaos", post(chaos))
        .route("/api/correct-laps", post(correct_laps))
        .route("/debug", get(debug))
        .with_state(runtime)
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

async fn state(State(runtime): State<Arc<RaceRuntime>>) -> Result<Json<StateSnapshot>, HttpError> {
    Ok(Json(runtime.snapshot().await?))
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
        yield Ok::<_, Infallible>(state_event(&initial));
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
                yield Ok(state_event(&snapshot));
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

fn state_event(snapshot: &StateSnapshot) -> Event {
    Event::default()
        .event("state")
        .id(snapshot.sequence.to_string())
        .data(serde_json::to_string(snapshot).expect("state snapshot is serializable"))
}

async fn debug() -> Html<&'static str> {
    Html(include_str!("debug.html"))
}

#[derive(Debug)]
enum HttpError {
    Malformed(String),
    Runtime(RuntimeError),
}

impl From<RuntimeError> for HttpError {
    fn from(value: RuntimeError) -> Self {
        Self::Runtime(value)
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Malformed(_) | Self::Runtime(RuntimeError::Store(StoreError::Domain(_))) => {
                StatusCode::BAD_REQUEST
            }
            Self::Runtime(RuntimeError::Store(error)) if error.is_busy() => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = match &self {
            Self::Malformed(message) => message.clone(),
            _ => format!("{self:?}"),
        };
        (status, message).into_response()
    }
}
