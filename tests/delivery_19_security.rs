use std::process::Command;

#[test]
fn lapxd_rejects_non_loopback_local_bind() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lapxd"))
        .env("LAPX_DB", dir.path().join("lapx.db"))
        .env("LAPX_LOCAL_BIND", "0.0.0.0:0")
        .env("LAPX_PUBLIC_BIND", "127.0.0.1:0")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("loopback"), "stderr: {stderr}");
}

#[tokio::test]
async fn local_surface_rejects_dns_rebinding_hosts() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use lapx::{
        http::{local_router, local_server_router},
        runtime::RaceRuntime,
        store::SqliteStore,
    };
    use tower::ServiceExt;

    let dir = tempfile::tempdir().unwrap();
    let runtime = RaceRuntime::new(
        SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
        "race",
    )
    .await
    .unwrap();
    for host in [
        "attacker.example:39123",
        "localhost.attacker.example",
        "localhost..",
        "192.168.1.2",
        "attacker@localhost",
        "localhost:evil",
        "localhost:+80",
        "localhost:-80",
        "localhost:65536",
        "[::1]evil",
    ] {
        let response = local_router(runtime.clone())
            .oneshot(
                Request::get("/api/state")
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST, "{host}");
    }
    let absolute = Request::get("http://attacker.example/control")
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        local_server_router(runtime.clone())
            .oneshot(absolute)
            .await
            .unwrap()
            .status(),
        StatusCode::MISDIRECTED_REQUEST
    );

    let mut duplicate = Request::get("/control").body(Body::empty()).unwrap();
    duplicate
        .headers_mut()
        .append("host", "localhost".parse().unwrap());
    duplicate
        .headers_mut()
        .append("host", "attacker.example".parse().unwrap());
    assert_eq!(
        local_router(runtime.clone())
            .oneshot(duplicate)
            .await
            .unwrap()
            .status(),
        StatusCode::MISDIRECTED_REQUEST
    );

    assert_eq!(
        local_server_router(runtime.clone())
            .oneshot(Request::get("/control").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::MISDIRECTED_REQUEST
    );

    for host in [
        "localhost:39123",
        "localhost.",
        "127.0.0.1:39123",
        "127.2.3.4",
        "[::1]:39123",
    ] {
        let response = local_server_router(runtime.clone())
            .oneshot(
                Request::get("/control")
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{host}");
    }
}

mod control_assets {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use lapx::{
        http::{local_router, public_router},
        runtime::RaceRuntime,
        store::SqliteStore,
    };
    use std::{collections::HashSet, path::Path};
    use tower::ServiceExt;

    fn public_assets() -> HashSet<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/build");
        let assets: Vec<String> =
            serde_json::from_slice(&std::fs::read(root.join("public-assets.json")).unwrap())
                .unwrap();
        assets
            .into_iter()
            .map(|asset| format!("/{asset}"))
            .collect()
    }

    fn page_assets(root: &Path, page: &str) -> HashSet<String> {
        let html = std::fs::read_to_string(root.join(page)).unwrap();
        html.split('"')
            .filter_map(|token| token.find("_app/").map(|at| format!("/{}", &token[at..])))
            .collect()
    }

    fn all_assets(dir: &Path, root: &Path, assets: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                all_assets(&path, root, assets);
            } else {
                assets.push(format!("/{}", path.strip_prefix(root).unwrap().display()));
            }
        }
    }

    #[tokio::test]
    async fn generated_public_allowlist_excludes_control_assets() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RaceRuntime::new(
            SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
            "race",
        )
        .await
        .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/build");
        let public = public_assets();
        assert!(!public.is_empty());

        let control = page_assets(&root, "control.html");
        let index = page_assets(&root, "index.html");
        let direct_control_only: Vec<_> = control.difference(&index).collect();
        assert!(!direct_control_only.is_empty());
        for path in direct_control_only {
            assert!(!public.contains(path), "control asset allowlisted: {path}");
            let response = public_router(runtime.clone())
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "public {path}");
        }

        for path in &public {
            let response = public_router(runtime.clone())
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "public {path}");
        }

        let mut all = Vec::new();
        all_assets(&root.join("_app"), &root, &mut all);
        let control_only = all.iter().find(|asset| !public.contains(*asset)).unwrap();
        let response = public_router(runtime.clone())
            .oneshot(Request::get(control_only).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let local = local_router(runtime.clone())
            .oneshot(Request::get(control_only).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(local.status(), StatusCode::OK);

        let suffix = control_only.strip_prefix("/_app/").unwrap();
        for bypass in [
            format!("/_app//{suffix}"),
            format!("/_app/./{suffix}"),
            format!("/_app/%2e/{suffix}"),
        ] {
            let response = public_router(runtime.clone())
                .oneshot(Request::get(&bypass).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{bypass}");
        }
    }
}
