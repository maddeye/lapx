use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    http::{local_router, public_router},
    runtime::RaceRuntime,
    store::{Driver, SqliteStore},
};
use std::collections::HashSet;
use tempfile::tempdir;
use tower::ServiceExt;

async fn json(app: &Router, request: Request<Body>, expected: StatusCode) -> serde_json::Value {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, expected, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

fn post(path: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn driver_crud_round_trip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let runtime = RaceRuntime::new(SqliteStore::open(&path).unwrap(), "race")
        .await
        .unwrap();
    let app = local_router(runtime);

    let blank = app
        .clone()
        .oneshot(post(
            "/api/drivers",
            serde_json::json!({ "display_name": " \t" }),
        ))
        .await
        .unwrap();
    assert_eq!(blank.status(), StatusCode::BAD_REQUEST);

    let created: Driver = serde_json::from_value(
        json(
            &app,
            post(
                "/api/drivers",
                serde_json::json!({ "display_name": "  Ada  " }),
            ),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    assert_eq!(created.display_name, "Ada");
    assert!(created.archived_at.is_none());
    let listed: Vec<Driver> = serde_json::from_value(
        json(
            &app,
            Request::get("/api/drivers").body(Body::empty()).unwrap(),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    assert_eq!(listed, vec![created.clone()]);

    let blank_rename = app
        .clone()
        .oneshot(post(
            &format!("/api/drivers/{}/rename", created.id),
            serde_json::json!({ "display_name": "  " }),
        ))
        .await
        .unwrap();
    assert_eq!(blank_rename.status(), StatusCode::BAD_REQUEST);

    let renamed: Driver = serde_json::from_value(
        json(
            &app,
            post(
                &format!("/api/drivers/{}/rename", created.id),
                serde_json::json!({ "display_name": "  Grace  " }),
            ),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    assert_eq!(renamed.id, created.id);
    assert_eq!(renamed.display_name, "Grace");

    let form_archive = app
        .clone()
        .oneshot(
            Request::post(format!("/api/drivers/{}/archive", created.id))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("confirm=yes"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(form_archive.status(), StatusCode::BAD_REQUEST);

    let archived: Driver = serde_json::from_value(
        json(
            &app,
            post(
                &format!("/api/drivers/{}/archive", created.id),
                serde_json::json!({}),
            ),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    assert_eq!(archived.id, created.id);
    assert!(archived.archived_at.is_some());
    let listed: Vec<Driver> = serde_json::from_value(
        json(
            &app,
            Request::get("/api/drivers").body(Body::empty()).unwrap(),
            StatusCode::OK,
        )
        .await,
    )
    .unwrap();
    assert_eq!(listed, vec![archived.clone()]);

    let delete = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/drivers/{}", created.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NOT_FOUND);

    drop(app);
    let reopened = SqliteStore::open(&path).unwrap();
    assert_eq!(reopened.drivers().unwrap(), vec![archived.clone()]);
    let next = reopened.create_driver("Lin").unwrap();
    assert_ne!(next.id, created.id);
    assert_eq!(reopened.drivers().unwrap()[0].id, created.id);
}

#[test]
fn existing_database_initializes_driver_storage_without_losing_data() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE existing_data (value TEXT); INSERT INTO existing_data VALUES ('kept');",
        )
        .unwrap();
    drop(connection);

    let store = SqliteStore::open(&path).unwrap();
    let driver = store.create_driver("Ada").unwrap();
    assert_eq!(store.drivers().unwrap(), vec![driver]);
    let connection = rusqlite::Connection::open(path).unwrap();
    let value: String = connection
        .query_row("SELECT value FROM existing_data", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "kept");
}

#[tokio::test]
async fn driver_admin_is_local_only_including_its_assets() {
    let dir = tempdir().unwrap();
    let runtime = RaceRuntime::new(
        SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
        "race",
    )
    .await
    .unwrap();

    let local = local_router(runtime.clone())
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(local.status(), StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(local.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("lang=\"de\""));

    for (method, path) in [
        ("GET", "/admin"),
        ("GET", "/api/drivers"),
        ("POST", "/api/drivers"),
        ("POST", "/api/drivers/1/rename"),
        ("POST", "/api/drivers/1/archive"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let response = public_router(runtime.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "public {method} {path}"
        );
    }

    let public: HashSet<String> = serde_json::from_slice(
        &std::fs::read(format!(
            "{}/ui/build/public-assets.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap();
    let admin_assets: HashSet<String> = html
        .split('"')
        .filter_map(|part| part.find("_app/").map(|at| part[at..].to_owned()))
        .collect();
    let private_assets: Vec<_> = admin_assets.difference(&public).collect();
    assert!(
        !private_assets.is_empty(),
        "admin needs a private build asset"
    );
    for asset in private_assets {
        let response = public_router(runtime.clone())
            .oneshot(
                Request::get(format!("/{asset}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "public /{asset}");
    }
}
