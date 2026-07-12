use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::*,
    http::{local_router, public_router},
    runtime::RaceRuntime,
    store::{SqliteStore, Tournament},
};
use tempfile::tempdir;
use tower::ServiceExt;

fn config(driver_ids: Vec<i64>) -> RaceConfig {
    RaceConfig {
        lanes: driver_ids.len() as u8,
        driver_ids: driver_ids.into_iter().map(Some).collect(),
        start_sequence_ms: 10,
        restart_sequence_ms: 10,
        minimum_lap_time_ms: 1,
        finish_condition: FinishCondition::Laps(1),
        finish_mode: FinishMode::Immediate,
        false_start_consequence: Consequence::Abort,
        chaos_consequence: Consequence::Abort,
    }
}

fn finish(store: &SqliteStore, race_id: &str, driver_ids: Vec<i64>) {
    store
        .execute(
            race_id,
            Command::StartRace {
                config: config(driver_ids),
                at: 0,
            },
        )
        .unwrap();
    store
        .execute(race_id, Command::AdvanceRace { to: 10 })
        .unwrap();
    store
        .execute(
            race_id,
            Command::SensorTriggered {
                lane: 1,
                at: 20,
                edge: SignalEdge::Rising,
            },
        )
        .unwrap();
}

fn post(path: impl AsRef<str>, body: serde_json::Value) -> Request<Body> {
    Request::post(path.as_ref())
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json(app: &Router, request: Request<Body>, expected: StatusCode) -> serde_json::Value {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, expected, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn manual_tournament_flow() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let store = SqliteStore::open(&path).unwrap();
    let ada = store.create_driver("Ada").unwrap();
    let grace = store.create_driver("Grace").unwrap();
    let lin = store.create_driver("Lin").unwrap();
    let runtime = RaceRuntime::new(store.clone(), "control-race")
        .await
        .unwrap();
    let app = local_router(runtime.clone());

    let mut tournament: Tournament = serde_json::from_value(
        json(
            &app,
            post(
                "/api/tournaments",
                serde_json::json!({ "name": "  Sommer-Cup  " }),
            ),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    assert_eq!(tournament.name, "Sommer-Cup");

    for assignments in [
        serde_json::json!([
            { "lane": 1, "driver_id": ada.id },
            { "lane": 2, "driver_id": grace.id }
        ]),
        serde_json::json!([
            { "lane": 1, "driver_id": grace.id },
            { "lane": 2, "driver_id": lin.id }
        ]),
    ] {
        tournament = serde_json::from_value(
            json(
                &app,
                post(
                    format!("/api/tournaments/{}/heats", tournament.id),
                    serde_json::json!({ "assignments": assignments }),
                ),
                StatusCode::OK,
            )
            .await,
        )
        .unwrap();
    }
    assert_eq!(tournament.heats[0].position, 1);
    assert_eq!(tournament.heats[1].position, 2);
    let first_heat = tournament.heats[0].id;

    finish(&store, "wrong-drivers", vec![grace.id, ada.id]);
    assert_eq!(
        app.clone()
            .oneshot(post(
                format!("/api/tournaments/{}/heats/{first_heat}/link", tournament.id),
                serde_json::json!({ "race_id": "wrong-drivers" }),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    finish(&store, "heat-one", vec![ada.id, grace.id]);
    tournament = serde_json::from_value(
        json(
            &app,
            post(
                format!("/api/tournaments/{}/heats/{first_heat}/link", tournament.id),
                serde_json::json!({ "race_id": "heat-one" }),
            ),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        tournament.heats[0].results.as_ref().unwrap()[0].driver_id,
        Some(ada.id)
    );

    store
        .execute(
            "heat-one",
            Command::CorrectLaps {
                lane: 2,
                delta_thousandths: 2_000,
                at: 21,
            },
        )
        .unwrap();
    tournament = serde_json::from_value(
        json(
            &app,
            Request::get(format!("/api/tournaments/{}", tournament.id))
                .body(Body::empty())
                .unwrap(),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    let results = tournament.heats[0].results.as_ref().unwrap();
    assert_eq!(results[0].driver_id, Some(grace.id));
    assert_eq!(results[0].corrected_laps_thousandths, 2_000);
    assert_eq!(
        SqliteStore::open(&path)
            .unwrap()
            .tournament(tournament.id)
            .unwrap(),
        tournament
    );

    assert_eq!(
        app.clone()
            .oneshot(post(
                format!("/api/tournaments/{}/heats/{first_heat}/link", tournament.id),
                serde_json::json!({ "race_id": "wrong-drivers" }),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        app.clone()
            .oneshot(post(
                format!("/api/tournaments/{}/heats", tournament.id),
                serde_json::json!({ "assignments": [{ "lane": 1, "driver_id": lin.id }] }),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );

    let validation: Tournament = serde_json::from_value(
        json(
            &app,
            post(
                "/api/tournaments",
                serde_json::json!({ "name": "Validation" }),
            ),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    for assignments in [
        serde_json::json!([
            { "lane": 1, "driver_id": ada.id },
            { "lane": 1, "driver_id": grace.id }
        ]),
        serde_json::json!([
            { "lane": 1, "driver_id": ada.id },
            { "lane": 2, "driver_id": ada.id }
        ]),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(post(
                    format!("/api/tournaments/{}/heats", validation.id),
                    serde_json::json!({ "assignments": assignments }),
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    store.archive_driver(lin.id).unwrap();
    assert_eq!(
        app.clone()
            .oneshot(post(
                format!("/api/tournaments/{}/heats", validation.id),
                serde_json::json!({
                    "assignments": [{ "lane": 1, "driver_id": lin.id }]
                }),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert!(store.tournament(validation.id).unwrap().heats.is_empty());

    let listed: Vec<Tournament> = serde_json::from_value(
        json(
            &app,
            Request::get("/api/tournaments")
                .body(Body::empty())
                .unwrap(),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    assert_eq!(listed.len(), 2);

    for (method, path) in [
        ("GET", "/api/tournaments".to_owned()),
        ("POST", "/api/tournaments".to_owned()),
        ("GET", format!("/api/tournaments/{}", tournament.id)),
        ("POST", format!("/api/tournaments/{}/heats", tournament.id)),
        (
            "POST",
            format!("/api/tournaments/{}/heats/{first_heat}/link", tournament.id),
        ),
    ] {
        assert_eq!(
            public_router(runtime.clone())
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(&path)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND,
            "public {method} {path}"
        );
    }

    for path in [
        "/api/tournaments",
        &format!("/api/tournaments/{}/heats", validation.id),
        &format!("/api/tournaments/{}/heats/{first_heat}/link", tournament.id),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(
                    Request::post(path)
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from("name=no-json"))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST,
            "mutation without JSON {path}"
        );
    }

    for method in ["PUT", "PATCH", "DELETE"] {
        assert_eq!(
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(format!(
                            "/api/tournaments/{}/heats/{first_heat}",
                            tournament.id
                        ))
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
