use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::*,
    http::{local_router, public_router},
    runtime::RaceRuntime,
    store::{EloDelta, EloRaceDelta, EloRating, EloSummary, SqliteStore},
};
use tempfile::tempdir;
use tower::ServiceExt;

fn config(driver_ids: Vec<Option<i64>>, finish_mode: FinishMode) -> RaceConfig {
    RaceConfig {
        lanes: driver_ids.len() as u8,
        driver_ids,
        start_sequence_ms: 10,
        restart_sequence_ms: 10,
        minimum_lap_time_ms: 1,
        finish_condition: FinishCondition::Laps(1),
        finish_mode,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}

fn start(store: &SqliteStore, race_id: &str, driver_ids: Vec<Option<i64>>, mode: FinishMode) {
    store
        .execute(
            race_id,
            Command::StartRace {
                config: config(driver_ids, mode),
                at: 0,
            },
        )
        .unwrap();
    store
        .execute(race_id, Command::AdvanceRace { to: 10 })
        .unwrap();
}

fn lap(store: &SqliteStore, race_id: &str, lane: u8, at: u64) {
    store
        .execute(
            race_id,
            Command::SensorTriggered {
                lane,
                at,
                edge: SignalEdge::Rising,
            },
        )
        .unwrap();
}

fn finish(
    store: &SqliteStore,
    race_id: &str,
    driver_ids: Vec<Option<i64>>,
    winner_lane: u8,
    at: u64,
) {
    start(store, race_id, driver_ids, FinishMode::Immediate);
    lap(store, race_id, winner_lane, at);
}

fn deltas(race_id: &str, values: &[(i64, i64)]) -> EloRaceDelta {
    EloRaceDelta {
        race_id: race_id.into(),
        deltas: values
            .iter()
            .map(|&(driver_id, delta)| EloDelta { driver_id, delta })
            .collect(),
    }
}

#[test]
fn elo_is_reproducible() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let store = SqliteStore::open(&path).unwrap();
    let ada = store.create_driver("Ada").unwrap();
    let grace = store.create_driver("Grace").unwrap();
    finish(&store, "race", vec![Some(ada.id), Some(grace.id)], 1, 20);

    let expected = EloSummary {
        ratings: vec![
            EloRating {
                driver_id: ada.id,
                rating: 1516,
            },
            EloRating {
                driver_id: grace.id,
                rating: 1484,
            },
        ],
        races: vec![deltas("race", &[(ada.id, 16), (grace.id, -16)])],
    };
    assert_eq!(store.elo().unwrap(), expected);
    assert_eq!(store.elo().unwrap(), expected);
    assert_eq!(SqliteStore::open(path).unwrap().elo().unwrap(), expected);
}

#[test]
fn correction_rebuilds_later_elo() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    let ada = store.create_driver("Ada").unwrap();
    let grace = store.create_driver("Grace").unwrap();

    // Completion order is authoritative even though the first race's clock is later.
    finish(
        &store,
        "first",
        vec![Some(ada.id), Some(grace.id)],
        1,
        1_000,
    );
    finish(&store, "later", vec![Some(ada.id), Some(grace.id)], 1, 20);
    let before = store.elo().unwrap();
    assert_eq!(
        before.races[1],
        deltas("later", &[(ada.id, 15), (grace.id, -15)])
    );

    store
        .execute(
            "first",
            Command::CorrectLaps {
                lane: 2,
                delta_thousandths: 2_000,
                at: 1_001,
            },
        )
        .unwrap();

    let rebuilt = store.elo().unwrap();
    assert_eq!(
        rebuilt.races,
        vec![
            deltas("first", &[(ada.id, -16), (grace.id, 16)]),
            deltas("later", &[(ada.id, 17), (grace.id, -17)]),
        ]
    );
    assert_eq!(
        rebuilt.ratings,
        vec![
            EloRating {
                driver_id: ada.id,
                rating: 1501,
            },
            EloRating {
                driver_id: grace.id,
                rating: 1499,
            },
        ]
    );
}

#[test]
fn elo_handles_ties_multilane_and_anonymous_results() {
    let tie_dir = tempdir().unwrap();
    let tie_store = SqliteStore::open(tie_dir.path().join("lapx.db")).unwrap();
    let first = tie_store.create_driver("First").unwrap();
    let second = tie_store.create_driver("Second").unwrap();
    start(
        &tie_store,
        "tie",
        vec![Some(first.id), Some(second.id)],
        FinishMode::AllCurrentLap,
    );
    lap(&tie_store, "tie", 2, 20);
    lap(&tie_store, "tie", 1, 20);
    assert_eq!(
        tie_store.elo().unwrap().races,
        vec![deltas("tie", &[(first.id, 0), (second.id, 0)])]
    );

    let multi_dir = tempdir().unwrap();
    let multi = SqliteStore::open(multi_dir.path().join("lapx.db")).unwrap();
    let a = multi.create_driver("A").unwrap();
    let b = multi.create_driver("B").unwrap();
    let c = multi.create_driver("C").unwrap();
    finish(
        &multi,
        "multi",
        vec![Some(a.id), Some(b.id), Some(c.id)],
        1,
        20,
    );
    assert_eq!(
        multi.elo().unwrap().ratings,
        vec![
            EloRating {
                driver_id: a.id,
                rating: 1516,
            },
            EloRating {
                driver_id: b.id,
                rating: 1492,
            },
            EloRating {
                driver_id: c.id,
                rating: 1492,
            },
        ]
    );

    let anonymous_dir = tempdir().unwrap();
    let anonymous = SqliteStore::open(anonymous_dir.path().join("lapx.db")).unwrap();
    let a = anonymous.create_driver("A").unwrap();
    let b = anonymous.create_driver("B").unwrap();
    finish(
        &anonymous,
        "anonymous-winner",
        vec![Some(a.id), Some(b.id), None],
        3,
        20,
    );
    finish(&anonymous, "one-assigned", vec![Some(a.id), None], 1, 20);
    assert_eq!(
        anonymous.elo().unwrap(),
        EloSummary {
            ratings: vec![
                EloRating {
                    driver_id: a.id,
                    rating: 1500,
                },
                EloRating {
                    driver_id: b.id,
                    rating: 1500,
                },
            ],
            races: vec![
                deltas("anonymous-winner", &[(a.id, 0), (b.id, 0)]),
                deltas("one-assigned", &[]),
            ],
        }
    );
}

#[tokio::test]
async fn elo_api_is_local_only() {
    let dir = tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("lapx.db")).unwrap();
    let ada = store.create_driver("Ada").unwrap();
    let runtime = RaceRuntime::new(store, "control-race").await.unwrap();

    let response = local_router(runtime.clone())
        .oneshot(Request::get("/api/elo").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<EloSummary>(
            &to_bytes(response.into_body(), usize::MAX).await.unwrap()
        )
        .unwrap(),
        EloSummary {
            ratings: vec![EloRating {
                driver_id: ada.id,
                rating: 1500,
            }],
            races: vec![],
        }
    );
    assert_eq!(
        public_router(runtime)
            .oneshot(Request::get("/api/elo").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}
