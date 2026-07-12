use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{http::local_router as router, runtime::RaceRuntime, store::SqliteStore};
use tempfile::tempdir;
use tower::ServiceExt;

#[tokio::test]
async fn debug_page_loads_and_references_every_control() {
    let dir = tempdir().unwrap();
    let response = router(
        RaceRuntime::new(
            SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
            "race",
        )
        .await
        .unwrap(),
    )
    .oneshot(Request::get("/debug").body(Body::empty()).unwrap())
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    let page = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    for reference in [
        "EventSource",
        "/api/state/stream",
        "/api/start",
        "/api/sensor",
        "/api/pause",
        "/api/resume",
        "/api/chaos",
        "/api/correct-laps",
        "data-lane=\"1\"",
        "data-lane=\"2\"",
        "data-lane=\"3\"",
        "data-lane=\"4\"",
    ] {
        assert!(page.contains(reference), "missing {reference}");
    }
}
