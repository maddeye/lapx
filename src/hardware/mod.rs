use crate::domain::{ProtocolMillis, SignalEdge};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::HashSet,
    fmt,
    sync::{Arc, Mutex},
};
use tokio::{sync::mpsc, time::Instant};

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
            if !(1..=4).contains(&mapping.lane) {
                return Err(HardwareError::new("hardware lane must be between 1 and 4"));
            }
            if !lane_numbers.insert(mapping.lane) {
                return Err(HardwareError::new("hardware lanes must be unique"));
            }
            if !pins.insert(mapping.input.bcm_pin) || !pins.insert(mapping.relay.bcm_pin) {
                return Err(HardwareError::new("hardware BCM pins must be unique"));
            }
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

#[derive(Clone, Debug)]
pub struct RawEdge {
    pub lane: u8,
    pub bcm_pin: u8,
    pub edge: SignalEdge,
    pub captured_at: Instant,
    pub level: bool,
    pub active: bool,
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
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct EdgeSink {
    monitor: HardwareMonitor,
    sender: mpsc::UnboundedSender<RawEdge>,
}

impl EdgeSink {
    pub fn accept(&self, edge: RawEdge) -> Result<(), HardwareError> {
        self.monitor.record_edge(edge.clone());
        if edge.active {
            self.sender
                .send(edge)
                .map_err(|_| HardwareError::new("timing consumer stopped"))?;
        }
        Ok(())
    }
}

pub trait TimingSource: Send {
    fn start(&mut self, sink: EdgeSink) -> Result<(), HardwareError>;
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

#[derive(Clone, Default)]
pub struct SimulationTimingSource(Arc<Mutex<Option<EdgeSink>>>);

impl SimulationTimingSource {
    pub fn emit(&self, edge: RawEdge) -> Result<(), HardwareError> {
        self.0
            .lock()
            .expect("simulation timing lock poisoned")
            .as_ref()
            .ok_or_else(|| HardwareError::new("simulation timing source is not started"))?
            .accept(edge)
    }
}

impl TimingSource for SimulationTimingSource {
    fn start(&mut self, sink: EdgeSink) -> Result<(), HardwareError> {
        *self.0.lock().expect("simulation timing lock poisoned") = Some(sink);
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct HardwareMonitor(Arc<Mutex<MonitorState>>);

struct MonitorState {
    snapshot: HardwareSnapshot,
    captured_at: [Option<Instant>; 4],
}

impl HardwareMonitor {
    pub(crate) fn new(config: HardwareConfig) -> Self {
        Self(Arc::new(Mutex::new(MonitorState {
            snapshot: HardwareSnapshot {
                config,
                input_levels: [None; 4],
                latest_edges: std::array::from_fn(|_| None),
                commanded_outputs: [false; 4],
                last_error: None,
            },
            captured_at: [None; 4],
        })))
    }

    pub(crate) fn snapshot(&self) -> HardwareSnapshot {
        self.0
            .lock()
            .expect("hardware monitor lock poisoned")
            .snapshot
            .clone()
    }

    fn record_edge(&self, edge: RawEdge) {
        let Some(index) = edge.lane.checked_sub(1).map(usize::from).filter(|i| *i < 4) else {
            self.record_error(format!("timing source reported invalid lane {}", edge.lane));
            return;
        };
        let mut state = self.0.lock().expect("hardware monitor lock poisoned");
        state.snapshot.input_levels[index] = Some(edge.level);
        state.captured_at[index] = Some(edge.captured_at);
        state.snapshot.latest_edges[index] = Some(EdgeSnapshot {
            lane: edge.lane,
            bcm_pin: edge.bcm_pin,
            edge: edge.edge,
            protocol_at: None,
            level: edge.level,
            active: edge.active,
        });
    }

    pub(crate) fn record_protocol(&self, edge: &RawEdge, at: ProtocolMillis) {
        let Some(index) = edge.lane.checked_sub(1).map(usize::from).filter(|i| *i < 4) else {
            return;
        };
        let mut state = self.0.lock().expect("hardware monitor lock poisoned");
        if state.captured_at[index] == Some(edge.captured_at)
            && let Some(latest) = &mut state.snapshot.latest_edges[index]
        {
            latest.protocol_at = Some(at);
        }
    }

    pub(crate) fn map_protocols(&self, mut map: impl FnMut(Instant) -> Option<ProtocolMillis>) {
        let mut state = self.0.lock().expect("hardware monitor lock poisoned");
        for index in 0..4 {
            if state.snapshot.latest_edges[index]
                .as_ref()
                .is_some_and(|edge| edge.protocol_at.is_none())
                && let Some(captured_at) = state.captured_at[index]
                && let Some(protocol_at) = map(captured_at)
                && let Some(edge) = &mut state.snapshot.latest_edges[index]
            {
                edge.protocol_at = Some(protocol_at);
            }
        }
    }

    pub(crate) fn record_outputs(&self, outputs: [bool; 4]) {
        self.0
            .lock()
            .expect("hardware monitor lock poisoned")
            .snapshot
            .commanded_outputs = outputs;
    }

    pub(crate) fn record_error(&self, error: impl Into<String>) {
        self.0
            .lock()
            .expect("hardware monitor lock poisoned")
            .snapshot
            .last_error = Some(error.into());
    }
}

pub(crate) fn channel(
    config: HardwareConfig,
) -> (HardwareMonitor, EdgeSink, mpsc::UnboundedReceiver<RawEdge>) {
    let monitor = HardwareMonitor::new(config);
    let (sender, receiver) = mpsc::unbounded_channel();
    let sink = EdgeSink {
        monitor: monitor.clone(),
        sender,
    };
    (monitor, sink, receiver)
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
