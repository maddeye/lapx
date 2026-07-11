use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use lapx::{
    http::{local_router, public_router},
    runtime::RaceRuntime,
    store::SqliteStore,
};
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

async fn runtime() -> Arc<RaceRuntime> {
    let dir = tempdir().unwrap();
    let path = dir.path().join("lapx.db");
    std::mem::forget(dir);
    RaceRuntime::new(SqliteStore::open(path).unwrap(), "race")
        .await
        .unwrap()
}

#[tokio::test]
async fn serves_static_rennscreen() {
    let runtime = runtime().await;
    let response = public_router(runtime.clone())
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("lang=\"de\""), "Rennscreen must be German");
    assert!(
        html.contains("/_app/"),
        "Rennscreen must reference built assets"
    );

    // Every asset referenced by the page must be served with a sensible MIME type.
    for (path, expected) in referenced_assets(&html) {
        let response = public_router(runtime.clone())
            .oneshot(Request::get(&path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "asset {path}");
        let mime = response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(mime.starts_with(&expected), "asset {path}: {mime}");
    }
}

#[tokio::test]
async fn asset_routes_are_read_only_and_do_not_leak() {
    let runtime = runtime().await;
    let missing = public_router(runtime.clone())
        .oneshot(
            Request::get("/_app/does-not-exist.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // No fallback route: unknown paths never resolve to a page publicly.
    let unknown = public_router(runtime.clone())
        .oneshot(Request::get("/anything").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let post = public_router(runtime)
        .oneshot(Request::post("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn local_router_also_serves_rennscreen() {
    let runtime = runtime().await;
    let response = local_router(runtime)
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

fn referenced_assets(html: &str) -> Vec<(String, String)> {
    let mut assets = Vec::new();
    for (marker, mime) in [("href=\"", ""), ("src=\"", ""), ("import(\"", "")] {
        let _ = mime;
        for chunk in html.split(marker).skip(1) {
            let Some(path) = chunk.split('"').next() else {
                continue;
            };
            let path = path.trim_start_matches('.');
            if !path.starts_with("/_app/") {
                continue;
            }
            let expected = if path.ends_with(".js") {
                "text/javascript"
            } else if path.ends_with(".css") {
                "text/css"
            } else {
                continue;
            };
            assets.push((path.to_owned(), expected.to_owned()));
        }
    }
    assert!(!assets.is_empty(), "page must reference assets");
    assets
}
