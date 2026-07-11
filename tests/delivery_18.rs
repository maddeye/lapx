use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use lapx::{
    domain::SignalEdge,
    hardware::{
        HardwareConfig, HardwareSnapshot, InputConfig, LaneHardwareConfig, PullMode, RawEdge,
        RelayConfig, SimulationPowerOutput, SimulationTimingSource,
    },
    http::router,
    runtime::RaceRuntime,
    store::SqliteStore,
};
use tempfile::tempdir;
use tokio::time::Instant;
use tower::ServiceExt;

fn config() -> HardwareConfig {
    HardwareConfig::new(vec![LaneHardwareConfig {
        lane: 1,
        input: InputConfig {
            bcm_pin: 27,
            active_edge: SignalEdge::Falling,
            pull: PullMode::Up,
        },
        relay: RelayConfig {
            bcm_pin: 23,
            active_high: false,
        },
    }])
    .unwrap()
}

#[tokio::test]
async fn hardware_page_loads_snapshot_with_lane_pin_mapping_and_polling() {
    let dir = tempdir().unwrap();
    let timing = SimulationTimingSource::default();
    let runtime = RaceRuntime::with_hardware(
        SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
        "race",
        config(),
        timing.clone(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    timing
        .emit(RawEdge {
            lane: 1,
            edge: SignalEdge::Rising,
            captured_at: Instant::now(),
        })
        .unwrap();
    let app = router(runtime);

    let response = app
        .clone()
        .oneshot(Request::get("/api/hardware").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot: HardwareSnapshot =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(snapshot.config, config());
    assert_eq!(snapshot.input_levels[0], Some(true));
    let edge = snapshot.latest_edges[0].as_ref().unwrap();
    assert_eq!((edge.lane, edge.bcm_pin), (1, 27));
    assert_eq!(edge.edge, SignalEdge::Rising);
    assert!(!edge.active);
    assert!(edge.protocol_at.is_some());

    let response = app
        .oneshot(Request::get("/hardware").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(page.contains("/api/hardware"));
    assert!(page.contains("setInterval"));
    assert!(page.contains("500"));
    assert!(!page.contains("method: 'POST'"));
}

#[tokio::test]
async fn hardware_snapshot_is_unavailable_without_configuration() {
    let dir = tempdir().unwrap();
    let app = router(
        RaceRuntime::new(
            SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
            "race",
        )
        .await
        .unwrap(),
    );
    let response = app
        .oneshot(Request::get("/api/hardware").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
