use crate::domain::{ProtocolMillis, SignalEdge};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::HashSet,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

#[cfg(feature = "gpio")]
pub mod gpio;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullMode {
    Off,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputConfig {
    pub bcm_pin: u8,
    pub active_edge: SignalEdge,
    pub pull: PullMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayConfig {
    pub bcm_pin: u8,
    pub active_high: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneHardwareConfig {
    pub lane: u8,
    pub input: InputConfig,
    pub relay: RelayConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HardwareConfig {
    pub lanes: Vec<LaneHardwareConfig>,
}

impl HardwareConfig {
    pub fn new(lanes: Vec<LaneHardwareConfig>) -> Result<Self, HardwareError> {
        if !(1..=4).contains(&lanes.len()) {
            return Err(HardwareError::new("hardware requires 1 to 4 lane mappings"));
        }
        let mut lane_numbers = HashSet::new();
        let mut pins = HashSet::new();
        for mapping in &lanes {
            if !lane_numbers.insert(mapping.lane) {
                return Err(HardwareError::new("hardware lanes must be unique"));
            }
            if !pins.insert(mapping.input.bcm_pin) || !pins.insert(mapping.relay.bcm_pin) {
                return Err(HardwareError::new("hardware BCM pins must be unique"));
            }
        }
        if !(1..=lanes.len() as u8).all(|lane| lane_numbers.contains(&lane)) {
            return Err(HardwareError::new(
                "hardware lanes must be contiguous from 1",
            ));
        }
        Ok(Self { lanes })
    }

    pub fn from_compact(value: &str) -> Result<Self, HardwareError> {
        let lanes = value
            .split(',')
            .map(|mapping| {
                let fields: Vec<_> = mapping.split(':').collect();
                if fields.len() != 6 {
                    return Err(HardwareError::new(
                        "hardware mapping must be lane:input:edge:pull:relay:polarity",
                    ));
                }
                Ok(LaneHardwareConfig {
                    lane: parse_number(fields[0], "lane")?,
                    input: InputConfig {
                        bcm_pin: parse_number(fields[1], "input BCM pin")?,
                        active_edge: match fields[2] {
                            "rising" => SignalEdge::Rising,
                            "falling" => SignalEdge::Falling,
                            _ => return Err(HardwareError::new("edge must be rising or falling")),
                        },
                        pull: match fields[3] {
                            "off" => PullMode::Off,
                            "up" => PullMode::Up,
                            "down" => PullMode::Down,
                            _ => return Err(HardwareError::new("pull must be off, up, or down")),
                        },
                    },
                    relay: RelayConfig {
                        bcm_pin: parse_number(fields[4], "relay BCM pin")?,
                        active_high: match fields[5] {
                            "high" | "active_high" => true,
                            "low" | "active_low" => false,
                            _ => {
                                return Err(HardwareError::new(
                                    "polarity must be active_high or active_low",
                                ));
                            }
                        },
                    },
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(lanes)
    }

    pub(crate) fn lane(&self, lane: u8) -> Option<&LaneHardwareConfig> {
        self.lanes.iter().find(|mapping| mapping.lane == lane)
    }
}

impl<'de> Deserialize<'de> for HardwareConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Config {
            lanes: Vec<LaneHardwareConfig>,
        }
        let config = Config::deserialize(deserializer)?;
        Self::new(config.lanes).map_err(serde::de::Error::custom)
    }
}

fn parse_number(value: &str, name: &str) -> Result<u8, HardwareError> {
    value
        .parse()
        .map_err(|_| HardwareError::new(format!("invalid {name}")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapturedAt {
    Simulation(Instant),
    KernelMonotonic(Duration),
}

#[derive(Clone, Debug)]
pub struct RawEdge {
    pub lane: u8,
    pub edge: SignalEdge,
    pub captured_at: CapturedAt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeSnapshot {
    pub lane: u8,
    pub bcm_pin: u8,
    pub edge: SignalEdge,
    pub protocol_at: Option<ProtocolMillis>,
    pub level: bool,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareSnapshot {
    pub config: HardwareConfig,
    pub input_levels: [Option<bool>; 4],
    pub latest_edges: [Option<EdgeSnapshot>; 4],
    pub commanded_outputs: [bool; 4],
    pub output_levels: [Option<bool>; 4],
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct EdgeSink(Arc<dyn Fn(RawEdge) -> Result<(), HardwareError> + Send + Sync>);

impl EdgeSink {
    pub(crate) fn new(
        send: impl Fn(RawEdge) -> Result<(), HardwareError> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(send))
    }

    pub fn accept(&self, edge: RawEdge) -> Result<(), HardwareError> {
        (self.0)(edge)
    }
}

pub trait TimingSource: Send {
    fn start(&mut self, sink: EdgeSink) -> Result<(), HardwareError>;

    fn initial_levels(&self) -> Vec<(u8, bool)> {
        Vec::new()
    }
}

pub trait PowerOutput: Send {
    fn set_lane_power(&mut self, lanes: [bool; 4]) -> Result<(), HardwareError>;
}

#[derive(Clone, Copy, Default)]
pub struct NoopPowerOutput;

impl PowerOutput for NoopPowerOutput {
    fn set_lane_power(&mut self, _lanes: [bool; 4]) -> Result<(), HardwareError> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct SimulationPowerOutput(Arc<Mutex<[bool; 4]>>);

impl SimulationPowerOutput {
    pub fn lane_power(&self) -> [bool; 4] {
        *self.0.lock().expect("simulation power lock poisoned")
    }
}

impl PowerOutput for SimulationPowerOutput {
    fn set_lane_power(&mut self, lanes: [bool; 4]) -> Result<(), HardwareError> {
        *self.0.lock().expect("simulation power lock poisoned") = lanes;
        Ok(())
    }
}

#[derive(Default)]
struct SimulationTimingState {
    sink: Option<EdgeSink>,
    initial_levels: Vec<(u8, bool)>,
}

#[derive(Clone, Default)]
pub struct SimulationTimingSource(Arc<Mutex<SimulationTimingState>>);

impl SimulationTimingSource {
    pub fn with_initial_levels(levels: Vec<(u8, bool)>) -> Self {
        Self(Arc::new(Mutex::new(SimulationTimingState {
            sink: None,
            initial_levels: levels,
        })))
    }

    pub fn emit(&self, edge: RawEdge) -> Result<(), HardwareError> {
        self.0
            .lock()
            .expect("simulation timing lock poisoned")
            .sink
            .as_ref()
            .ok_or_else(|| HardwareError::new("simulation timing source is not started"))?
            .accept(edge)
    }
}

impl TimingSource for SimulationTimingSource {
    fn start(&mut self, sink: EdgeSink) -> Result<(), HardwareError> {
        self.0.lock().expect("simulation timing lock poisoned").sink = Some(sink);
        Ok(())
    }

    fn initial_levels(&self) -> Vec<(u8, bool)> {
        self.0
            .lock()
            .expect("simulation timing lock poisoned")
            .initial_levels
            .clone()
    }
}

#[derive(Clone)]
pub(crate) struct HardwareMonitor(Arc<Mutex<HardwareSnapshot>>);

impl HardwareMonitor {
    pub(crate) fn new(config: HardwareConfig) -> Self {
        Self(Arc::new(Mutex::new(HardwareSnapshot {
            config,
            input_levels: [None; 4],
            latest_edges: std::array::from_fn(|_| None),
            commanded_outputs: [false; 4],
            output_levels: [None; 4],
            last_error: None,
        })))
    }

    pub(crate) fn snapshot(&self) -> HardwareSnapshot {
        self.0
            .lock()
            .expect("hardware monitor lock poisoned")
            .clone()
    }

    pub(crate) fn record_initial_levels(&self, levels: &[(u8, bool)]) {
        let mut snapshot = self.0.lock().expect("hardware monitor lock poisoned");
        for &(lane, level) in levels {
            if snapshot.config.lane(lane).is_some() {
                snapshot.input_levels[usize::from(lane - 1)] = Some(level);
            } else {
                snapshot.last_error = Some(format!(
                    "timing source reported initial level for invalid lane {lane}"
                ));
            }
        }
    }

    pub(crate) fn record_edge(&self, edge: &RawEdge, at: ProtocolMillis) -> bool {
        let mut snapshot = self.0.lock().expect("hardware monitor lock poisoned");
        let Some(mapping) = snapshot.config.lane(edge.lane).cloned() else {
            snapshot.last_error =
                Some(format!("timing source reported invalid lane {}", edge.lane));
            return false;
        };
        let index = usize::from(edge.lane - 1);
        let level = edge.edge == SignalEdge::Rising;
        let active = edge.edge == mapping.input.active_edge;
        snapshot.input_levels[index] = Some(level);
        snapshot.latest_edges[index] = Some(EdgeSnapshot {
            lane: edge.lane,
            bcm_pin: mapping.input.bcm_pin,
            edge: edge.edge,
            protocol_at: Some(at),
            level,
            active,
        });
        active
    }

    pub(crate) fn record_outputs(&self, outputs: [bool; 4]) {
        let mut snapshot = self.0.lock().expect("hardware monitor lock poisoned");
        snapshot.commanded_outputs = outputs;
        snapshot.output_levels = [None; 4];
        for mapping in &snapshot.config.lanes.clone() {
            let index = usize::from(mapping.lane - 1);
            snapshot.output_levels[index] = Some(outputs[index] == mapping.relay.active_high);
        }
    }

    pub(crate) fn record_error(&self, error: impl Into<String>) {
        self.0
            .lock()
            .expect("hardware monitor lock poisoned")
            .last_error = Some(error.into());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardwareError(String);

impl HardwareError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for HardwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HardwareError {}
