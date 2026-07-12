use axum::{
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use include_dir::{Dir, include_dir};
use std::{collections::HashSet, sync::LazyLock};

static UI_BUILD: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/ui/build");

/// Build-generated allowlist for the read-only Rennscreen. Public serving is
/// fail-closed; control-only chunks never become reachable through path tricks.
static PUBLIC_ASSETS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let manifest = UI_BUILD
        .get_file("public-assets.json")
        .expect("ui build must contain public-assets.json");
    serde_json::from_slice::<Vec<String>>(manifest.contents())
        .expect("public asset manifest must be valid JSON")
        .into_iter()
        .collect()
});

pub(super) async fn rennscreen() -> Response {
    page("index.html")
}

pub(super) async fn control() -> Response {
    page("control.html")
}

pub(super) async fn admin() -> Response {
    page("admin.html")
}

fn page(name: &str) -> Response {
    match UI_BUILD.get_file(name) {
        Some(file) => Html(file.contents()).into_response(),
        None => (StatusCode::NOT_FOUND, "ui build is missing").into_response(),
    }
}

/// Public bind: serves only generated Rennscreen assets.
pub(super) async fn public_app_asset(path: axum::extract::Path<String>) -> Response {
    if !canonical_asset_path(&path.0) || !PUBLIC_ASSETS.contains(&format!("_app/{}", path.0)) {
        return StatusCode::NOT_FOUND.into_response();
    }
    app_asset(path).await
}

fn canonical_asset_path(path: &str) -> bool {
    !path.contains('\\')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

pub(super) async fn app_asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let Some(file) = UI_BUILD.get_file(format!("_app/{path}")) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = match path.rsplit('.').next() {
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, mime)], file.contents()).into_response()
}
