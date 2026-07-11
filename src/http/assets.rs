use axum::{
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use include_dir::{Dir, include_dir};

static UI_BUILD: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/ui/build");

pub(super) async fn rennscreen() -> Response {
    page("index.html")
}

pub(super) async fn control() -> Response {
    page("control.html")
}

fn page(name: &str) -> Response {
    match UI_BUILD.get_file(name) {
        Some(file) => Html(file.contents()).into_response(),
        None => (StatusCode::NOT_FOUND, "ui build is missing").into_response(),
    }
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
