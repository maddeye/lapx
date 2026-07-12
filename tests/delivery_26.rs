use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::{Command, Consequence, FinishCondition, FinishMode, RaceConfig, SignalEdge},
    http::{local_router, public_router},
    runtime::RaceRuntime,
    store::{SqliteStore, Tournament, TournamentGenerationMode},
};
use std::collections::BTreeSet;
use tempfile::tempdir;
use tower::ServiceExt;

fn post(path: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn response(app: &Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, body)
}

async fn generated(
    app: &Router,
    name: &str,
    driver_ids: Vec<i64>,
    lane_count: u8,
    mode: &str,
    seed: u64,
) -> Tournament {
    let (status, body) = response(
        app,
        post(
            "/api/tournaments/generate",
            serde_json::json!({
                "name": name,
                "driver_ids": driver_ids,
                "lane_count": lane_count,
                "mode": mode,
                "seed": seed,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

fn assignment_ids(tournament: &Tournament) -> Vec<Vec<i64>> {
    tournament
        .heats
        .iter()
        .map(|heat| {
            heat.assignments
                .iter()
                .map(|assignment| assignment.driver_id)
                .collect()
        })
        .collect()
}

fn assert_valid_heats(tournament: &Tournament, selected: &[i64], lane_count: u8) {
    let mut actual = Vec::new();
    for heat in &tournament.heats {
        assert!(!heat.assignments.is_empty());
        assert!(heat.assignments.len() <= lane_count as usize);
        for (index, assignment) in heat.assignments.iter().enumerate() {
            assert_eq!(assignment.lane, index as u8 + 1);
            actual.push(assignment.driver_id);
        }
    }
    actual.sort_unstable();
    let mut expected = selected.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected);
    assert_eq!(actual.iter().collect::<BTreeSet<_>>().len(), actual.len());
}

fn finish_for_elo(store: &SqliteStore, first: i64, second: i64) {
    let config = RaceConfig {
        lanes: 2,
        driver_ids: vec![Some(first), Some(second)],
        start_sequence_ms: 10,
        restart_sequence_ms: 10,
        minimum_lap_time_ms: 1,
        finish_condition: FinishCondition::Laps(1),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    };
    store
        .execute("elo-source", Command::StartRace { config, at: 0 })
        .unwrap();
    store
        .execute("elo-source", Command::AdvanceRace { to: 10 })
        .unwrap();
    store
        .execute(
            "elo-source",
            Command::SensorTriggered {
                lane: 1,
                at: 20,
                edge: SignalEdge::Rising,
            },
        )
        .unwrap();
    // Corrected history is authoritative: the original loser now ranks first.
    store
        .execute(
            "elo-source",
            Command::CorrectLaps {
                lane: 2,
                delta_thousandths: 2_000,
                at: 21,
            },
        )
        .unwrap();
}

#[tokio::test]
async fn tournament_generation_is_deterministic() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let store = SqliteStore::open(&path).unwrap();
    let drivers: Vec<_> = (1..=7)
        .map(|index| store.create_driver(&format!("Driver {index}")).unwrap().id)
        .collect();
    finish_for_elo(&store, drivers[0], drivers[1]);
    let runtime = RaceRuntime::new(store.clone(), "control-race")
        .await
        .unwrap();
    let app = local_router(runtime.clone());

    let random = generated(
        &app,
        "  Random Cup  ",
        drivers.clone(),
        3,
        "random",
        u64::MAX,
    )
    .await;
    let mut reversed = drivers.clone();
    reversed.reverse();
    let random_reordered = generated(
        &app,
        "Random Again",
        reversed.clone(),
        3,
        "random",
        u64::MAX,
    )
    .await;
    assert_eq!(random.name, "Random Cup");
    assert_eq!(assignment_ids(&random), assignment_ids(&random_reordered));
    // Locks the documented SplitMix64 + reverse Fisher-Yates contract.
    assert_eq!(
        assignment_ids(&random),
        vec![vec![6, 5, 7], vec![3, 2, 4], vec![1]]
    );
    assert_eq!(
        random.generation.as_ref().unwrap().mode,
        TournamentGenerationMode::Random
    );
    assert_eq!(random.generation.as_ref().unwrap().seed, u64::MAX);
    assert_eq!(random.generation.as_ref().unwrap().lane_count, 3);
    assert_valid_heats(&random, &drivers, 3);

    let elo = generated(
        &app,
        "Elo Cup",
        drivers.clone(),
        3,
        "elo_balanced",
        u64::MAX - 1,
    )
    .await;
    let elo_reordered =
        generated(&app, "Elo Again", reversed, 3, "elo_balanced", u64::MAX - 1).await;
    assert_eq!(assignment_ids(&elo), assignment_ids(&elo_reordered));
    assert_valid_heats(&elo, &drivers, 3);
    assert_eq!(elo.heats.len(), 3);
    // Elo sorting sees the correction in the generation snapshot, then snakes high to low.
    assert_eq!(elo.heats[0].assignments[0].driver_id, drivers[1]);
    assert_eq!(elo.heats[0].assignments[2].driver_id, drivers[0]);
    assert_eq!(
        elo.generation.as_ref().unwrap().mode,
        TournamentGenerationMode::EloBalanced
    );

    let one_lane = generated(&app, "One lane", drivers[..2].to_vec(), 1, "random", 0).await;
    assert_eq!(one_lane.heats.len(), 2);
    assert_valid_heats(&one_lane, &drivers[..2], 1);
    let four_lanes = generated(&app, "Four lanes", drivers[..2].to_vec(), 4, "random", 0).await;
    assert_eq!(four_lanes.heats.len(), 1);
    assert_valid_heats(&four_lanes, &drivers[..2], 4);

    let reopened = SqliteStore::open(&path).unwrap();
    assert_eq!(reopened.tournament(random.id).unwrap(), random);
    let (status, body) = response(
        &app,
        Request::get(format!("/api/tournaments/{}", random.id))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let fetched_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(fetched_json["generation"]["seed"], u64::MAX.to_string());
    let fetched: Tournament = serde_json::from_value(fetched_json).unwrap();
    assert_eq!(fetched.generation, random.generation);

    let before_failures = store.tournaments().unwrap().len();
    let archived = store.create_driver("Archived").unwrap();
    store.archive_driver(archived.id).unwrap();
    for body in [
        serde_json::json!({ "name": "one", "driver_ids": [drivers[0]], "lane_count": 2, "mode": "random", "seed": 0 }),
        serde_json::json!({ "name": "duplicate", "driver_ids": [drivers[0], drivers[0]], "lane_count": 2, "mode": "random", "seed": 0 }),
        serde_json::json!({ "name": "zero lanes", "driver_ids": drivers[..2], "lane_count": 0, "mode": "random", "seed": 0 }),
        serde_json::json!({ "name": "five lanes", "driver_ids": drivers[..2], "lane_count": 5, "mode": "random", "seed": 0 }),
        serde_json::json!({ "name": "archived", "driver_ids": [drivers[0], archived.id], "lane_count": 2, "mode": "random", "seed": 0 }),
        serde_json::json!({ "name": "", "driver_ids": drivers[..2], "lane_count": 2, "mode": "random", "seed": 0 }),
        serde_json::json!({ "name": "bad mode", "driver_ids": drivers[..2], "lane_count": 2, "mode": "swiss", "seed": 0 }),
    ] {
        assert_eq!(
            response(&app, post("/api/tournaments/generate", body))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(store.tournaments().unwrap().len(), before_failures);
    }
    assert_eq!(
        response(
            &app,
            Request::post("/api/tournaments/generate")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("name=not-json"))
                .unwrap(),
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    // Force a failure after tournament and heat rows are attempted; the transaction rolls back all.
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_generated_assignment BEFORE INSERT ON tournament_heat_assignments
             WHEN NEW.driver_id = {} BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
            drivers[0]
        ))
        .unwrap();
    assert_eq!(
        response(
            &app,
            post(
                "/api/tournaments/generate",
                serde_json::json!({ "name": "Atomic", "driver_ids": drivers[..2], "lane_count": 2, "mode": "random", "seed": 0 }),
            ),
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(store.tournaments().unwrap().len(), before_failures);
    connection
        .execute_batch("DROP TRIGGER fail_generated_assignment")
        .unwrap();

    for request in [
        Request::post("/api/tournaments/generate")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
        Request::get(format!("/api/tournaments/{}", random.id))
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_eq!(
            public_router(runtime.clone())
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
