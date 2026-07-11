use lapx::{
    domain::SignalEdge,
    hardware::{
        HardwareConfig, InputConfig, LaneHardwareConfig, PullMode, RelayConfig,
        SimulationPowerOutput, SimulationTimingSource,
    },
    runtime::RaceRuntime,
    store::SqliteStore,
};
use tempfile::tempdir;

fn mapping(lane: u8, input: u8, relay: u8) -> LaneHardwareConfig {
    LaneHardwareConfig {
        lane,
        input: InputConfig {
            bcm_pin: input,
            active_edge: SignalEdge::Falling,
            pull: PullMode::Up,
        },
        relay: RelayConfig {
            bcm_pin: relay,
            active_high: false,
        },
    }
}

#[test]
fn hardware_config_validates_lane_and_pin_mapping() {
    assert!(HardwareConfig::new(vec![]).is_err());
    assert!(HardwareConfig::new(vec![mapping(1, 17, 17)]).is_err());
    assert!(HardwareConfig::new(vec![mapping(1, 17, 22), mapping(1, 27, 23)]).is_err());
    assert!(HardwareConfig::new(vec![mapping(1, 17, 22), mapping(2, 17, 23)]).is_err());

    let config = HardwareConfig::new(vec![mapping(1, 17, 22), mapping(2, 27, 23)]).unwrap();
    assert_eq!(
        serde_json::from_str::<HardwareConfig>(&serde_json::to_string(&config).unwrap()).unwrap(),
        config
    );
    let invalid = serde_json::json!({"lanes": [mapping(1, 17, 22), mapping(2, 17, 23)]});
    assert!(serde_json::from_value::<HardwareConfig>(invalid).is_err());
}

#[test]
fn compact_hardware_config_carries_edge_pull_and_polarity() {
    let config = HardwareConfig::from_compact(
        "1:17:rising:up:22:active_high,2:27:falling:down:23:active_low",
    )
    .unwrap();
    assert_eq!(
        config.lanes[0],
        LaneHardwareConfig {
            lane: 1,
            input: InputConfig {
                bcm_pin: 17,
                active_edge: SignalEdge::Rising,
                pull: PullMode::Up,
            },
            relay: RelayConfig {
                bcm_pin: 22,
                active_high: true,
            },
        }
    );
    assert!(!config.lanes[1].relay.active_high);
}

#[tokio::test]
async fn hardware_snapshot_starts_with_configured_mapping_and_all_levels_unknown() {
    let dir = tempdir().unwrap();
    let config = HardwareConfig::new(vec![mapping(1, 17, 22)]).unwrap();
    let runtime = RaceRuntime::with_hardware(
        SqliteStore::open(dir.path().join("lapx.db")).unwrap(),
        "race",
        config.clone(),
        SimulationTimingSource::default(),
        SimulationPowerOutput::default(),
    )
    .await
    .unwrap();
    let snapshot = runtime.hardware_snapshot().unwrap();
    assert_eq!(snapshot.config, config);
    assert_eq!(snapshot.input_levels, [None; 4]);
    assert_eq!(snapshot.commanded_outputs, [false; 4]);
}

#[cfg(feature = "gpio")]
#[test]
fn gpio_types_are_available_without_opening_gpio() {
    use lapx::hardware::gpio::{GpioPowerOutput, GpioTimingSource};
    let config = HardwareConfig::new(vec![mapping(1, 17, 22)]).unwrap();
    let _timing = GpioTimingSource::new(config);
    let _constructor: fn(&HardwareConfig) -> Result<GpioPowerOutput, _> = GpioPowerOutput::new;
}
