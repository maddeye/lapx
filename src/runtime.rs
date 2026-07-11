mod dispatcher;

pub use crate::store::StateSnapshot;
use crate::{
    domain::{Command, ProtocolMillis, RaceControl, RaceStatus},
    hardware::{
        CapturedAt, EdgeSink, HardwareConfig, HardwareError, HardwareMonitor, HardwareSnapshot,
        PowerOutput, RawEdge, TimingSource,
    },
    store::{ImmediateError, SqliteStore, StoreError},
};
use dispatcher::{Dispatcher, DispatcherHardware, needs_pause};
use std::{
    fmt,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinError,
    time::Instant,
};

const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
// Allows kernel callback threads to enqueue edges captured just before a due timestamp.
const CAPTURE_SETTLE_WINDOW: Duration = Duration::from_millis(5);

type CommandResult = Result<StateSnapshot, RuntimeError>;

#[derive(Debug)]
pub enum RuntimeError {
    Store(StoreError),
    Task(JoinError),
    Hardware(HardwareError),
    HardwareLaneMismatch {
        configured: u8,
        requested: u8,
    },
    PowerAfterCommit {
        snapshot: Box<StateSnapshot>,
        source: HardwareError,
    },
    DispatcherStopped,
    ClockOverflow,
    MonotonicClock(std::io::Error),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HardwareLaneMismatch {
                configured,
                requested,
            } => write!(
                f,
                "race requests {requested} lanes but hardware configures {configured}"
            ),
            Self::PowerAfterCommit { snapshot, source } => write!(
                f,
                "race state committed at sequence {}; power synchronization failed: {source}",
                snapshot.sequence
            ),
            _ => write!(f, "{self:?}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<StoreError> for RuntimeError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

impl From<JoinError> for RuntimeError {
    fn from(value: JoinError) -> Self {
        Self::Task(value)
    }
}

impl From<HardwareError> for RuntimeError {
    fn from(value: HardwareError) -> Self {
        Self::Hardware(value)
    }
}

struct ProtocolClock {
    protocol_at_anchor: ProtocolMillis,
    instant_anchor: Instant,
    monotonic_anchor: Option<Duration>,
}

impl ProtocolClock {
    fn now(&self) -> Result<ProtocolMillis, RuntimeError> {
        self.at(CapturedAt::Simulation(Instant::now()))
    }

    fn at(&self, captured_at: CapturedAt) -> Result<ProtocolMillis, RuntimeError> {
        let (after_anchor, elapsed) = match captured_at {
            CapturedAt::Simulation(instant) if instant >= self.instant_anchor => {
                (true, instant - self.instant_anchor)
            }
            CapturedAt::Simulation(instant) => (false, self.instant_anchor - instant),
            CapturedAt::KernelMonotonic(timestamp) => {
                let anchor = self.monotonic_anchor.ok_or_else(|| {
                    HardwareError::new("kernel monotonic clock was not calibrated")
                })?;
                if timestamp >= anchor {
                    (true, timestamp - anchor)
                } else {
                    (false, anchor - timestamp)
                }
            }
        };
        let elapsed = duration_millis(elapsed)?;
        if after_anchor {
            self.protocol_at_anchor
                .checked_add(elapsed)
                .ok_or(RuntimeError::ClockOverflow)
        } else {
            self.protocol_at_anchor
                .checked_sub(elapsed)
                .ok_or(RuntimeError::ClockOverflow)
        }
    }

    fn observe(&mut self, at: ProtocolMillis) {
        if self.now().is_ok_and(|now| at > now) {
            self.protocol_at_anchor = at;
            self.instant_anchor = Instant::now();
            self.monotonic_anchor = kernel_monotonic_now().ok().flatten();
        }
    }
}

fn duration_millis(duration: Duration) -> Result<u64, RuntimeError> {
    u64::try_from(duration.as_millis()).map_err(|_| RuntimeError::ClockOverflow)
}

struct Shared {
    updates: broadcast::Sender<StateSnapshot>,
    published_sequence: AtomicU64,
    clock: StdMutex<ProtocolClock>,
}

impl Shared {
    fn protocol_now(&self) -> Result<ProtocolMillis, RuntimeError> {
        self.clock
            .lock()
            .map_err(|_| RuntimeError::ClockOverflow)?
            .now()
    }

    fn protocol_at(&self, captured_at: CapturedAt) -> Result<ProtocolMillis, RuntimeError> {
        self.clock
            .lock()
            .map_err(|_| RuntimeError::ClockOverflow)?
            .at(captured_at)
    }

    fn publish(&self, snapshot: &StateSnapshot) {
        if let Some(at) = snapshot.state.last_event_at
            && let Ok(mut clock) = self.clock.lock()
        {
            clock.observe(at);
        }
        if self
            .published_sequence
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |sequence| {
                (snapshot.sequence > sequence).then_some(snapshot.sequence)
            })
            .is_ok()
        {
            let _ = self.updates.send(snapshot.clone());
        }
    }
}

struct HardwareRuntime {
    monitor: HardwareMonitor,
    _timing: StdMutex<Box<dyn TimingSource>>,
}

pub struct RaceRuntime {
    ingress: mpsc::UnboundedSender<Ingress>,
    shared: Arc<Shared>,
    hardware: Option<HardwareRuntime>,
}

enum CommandRequest {
    Exact(Command),
    Now(Command),
}

struct TimedEdge {
    lane: u8,
    edge: crate::domain::SignalEdge,
    at: ProtocolMillis,
}

enum Ingress {
    Command {
        request: CommandRequest,
        response: oneshot::Sender<CommandResult>,
    },
    Edge(TimedEdge),
    Snapshot {
        response: oneshot::Sender<CommandResult>,
    },
}

impl RaceRuntime {
    pub async fn new(
        store: SqliteStore,
        race_id: impl Into<String>,
    ) -> Result<Arc<Self>, RuntimeError> {
        let race_id = race_id.into();
        let initial = load_snapshot(&store, &race_id).await?;
        let (ingress, receiver) = mpsc::unbounded_channel();
        let shared = shared(&initial, false)?;
        let runtime = Arc::new(Self {
            ingress,
            shared: shared.clone(),
            hardware: None,
        });
        Dispatcher::spawn(store, race_id, initial, receiver, shared, None);
        Ok(runtime)
    }

    pub async fn with_hardware<T, P>(
        store: SqliteStore,
        race_id: impl Into<String>,
        config: HardwareConfig,
        mut timing: T,
        mut power: P,
    ) -> Result<Arc<Self>, RuntimeError>
    where
        T: TimingSource + 'static,
        P: PowerOutput + 'static,
    {
        let race_id = race_id.into();
        power.set_lane_power([false; 4])?;
        let mut initial = load_snapshot(&store, &race_id).await?;
        validate_hardware_snapshot(&initial, &config)?;
        if needs_pause(&initial) {
            initial = execute_now(
                &store,
                &race_id,
                initial.state.last_event_at.unwrap_or(0),
                |at| Command::PauseRace { at },
            )
            .await?;
        }

        let monitor = HardwareMonitor::new(config.clone());
        monitor.record_outputs([false; 4]);
        let (ingress, receiver) = mpsc::unbounded_channel();
        let shared = shared(&initial, true)?;
        let edge_ingress = ingress.clone();
        let edge_shared = shared.clone();
        let edge_monitor = monitor.clone();
        timing.start(EdgeSink::new(
            move |RawEdge {
                      lane,
                      edge,
                      captured_at,
                  }| {
                let at = edge_shared.protocol_at(captured_at).map_err(|error| {
                    let error =
                        HardwareError::new(format!("edge timestamp conversion failed: {error}"));
                    edge_monitor.record_error(error.to_string());
                    error
                })?;
                edge_ingress
                    .send(Ingress::Edge(TimedEdge { lane, edge, at }))
                    .map_err(|_| HardwareError::new("timing consumer stopped"))
            },
        ))?;
        monitor.record_initial_levels(&timing.initial_levels());

        let runtime = Arc::new(Self {
            ingress,
            shared: shared.clone(),
            hardware: Some(HardwareRuntime {
                monitor: monitor.clone(),
                _timing: StdMutex::new(Box::new(timing)),
            }),
        });
        Dispatcher::spawn(
            store,
            race_id,
            initial,
            receiver,
            shared,
            Some(DispatcherHardware {
                config,
                monitor,
                power: Arc::new(StdMutex::new(Box::new(power))),
                fault: None,
            }),
        );
        Ok(runtime)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StateSnapshot> {
        self.shared.updates.subscribe()
    }

    pub async fn snapshot(&self) -> Result<StateSnapshot, RuntimeError> {
        let (response, receive) = oneshot::channel();
        self.ingress
            .send(Ingress::Snapshot { response })
            .map_err(|_| RuntimeError::DispatcherStopped)?;
        receive.await.map_err(|_| RuntimeError::DispatcherStopped)?
    }

    pub fn hardware_snapshot(&self) -> Option<HardwareSnapshot> {
        Some(self.hardware.as_ref()?.monitor.snapshot())
    }

    pub async fn apply(&self, command: Command) -> Result<StateSnapshot, RuntimeError> {
        self.request(CommandRequest::Exact(command)).await
    }

    pub async fn apply_now<F>(&self, command: F) -> Result<StateSnapshot, RuntimeError>
    where
        F: FnOnce(ProtocolMillis) -> Command + Send + 'static,
    {
        let proposed_at = self.shared.protocol_now()?;
        self.request(CommandRequest::Now(command(proposed_at)))
            .await
    }

    pub fn protocol_now(&self) -> Result<ProtocolMillis, RuntimeError> {
        self.shared.protocol_now()
    }

    async fn request(&self, request: CommandRequest) -> CommandResult {
        let (response, receive) = oneshot::channel();
        self.ingress
            .send(Ingress::Command { request, response })
            .map_err(|_| RuntimeError::DispatcherStopped)?;
        receive.await.map_err(|_| RuntimeError::DispatcherStopped)?
    }
}

fn shared(initial: &StateSnapshot, hardware: bool) -> Result<Arc<Shared>, RuntimeError> {
    let (updates, _) = broadcast::channel(16);
    Ok(Arc::new(Shared {
        updates,
        published_sequence: AtomicU64::new(initial.sequence),
        clock: StdMutex::new(ProtocolClock {
            protocol_at_anchor: initial.state.last_event_at.unwrap_or(0),
            instant_anchor: Instant::now(),
            monotonic_anchor: hardware.then(kernel_monotonic_now).transpose()?.flatten(),
        }),
    }))
}

fn validate_hardware_snapshot(
    snapshot: &StateSnapshot,
    hardware: &HardwareConfig,
) -> Result<(), RuntimeError> {
    let Some(config) = snapshot.state.config() else {
        return Ok(());
    };
    let configured = hardware.lanes.len() as u8;
    if config.lanes == configured {
        Ok(())
    } else {
        Err(RuntimeError::HardwareLaneMismatch {
            configured,
            requested: config.lanes,
        })
    }
}

fn hardware_lane_error(snapshot: &StateSnapshot, configured: u8) -> Option<HardwareError> {
    let requested = snapshot.state.config()?.lanes;
    (requested != configured).then(|| {
        HardwareError::new(
            RuntimeError::HardwareLaneMismatch {
                configured,
                requested,
            }
            .to_string(),
        )
    })
}

#[cfg(feature = "gpio")]
fn kernel_monotonic_now() -> Result<Option<Duration>, RuntimeError> {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timestamp` is valid writable storage for one `timespec`.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) } != 0 {
        return Err(RuntimeError::MonotonicClock(std::io::Error::last_os_error()));
    }
    Ok(Some(Duration::new(
        timestamp
            .tv_sec
            .try_into()
            .map_err(|_| RuntimeError::ClockOverflow)?,
        timestamp
            .tv_nsec
            .try_into()
            .map_err(|_| RuntimeError::ClockOverflow)?,
    )))
}

#[cfg(not(feature = "gpio"))]
fn kernel_monotonic_now() -> Result<Option<Duration>, RuntimeError> {
    Ok(None)
}

async fn load_snapshot(store: &SqliteStore, race_id: &str) -> Result<StateSnapshot, RuntimeError> {
    let store = store.clone();
    let race_id = race_id.to_owned();
    Ok(tokio::task::spawn_blocking(move || store.load(&race_id)).await??)
}

async fn execute_now<F>(
    store: &SqliteStore,
    race_id: &str,
    proposed_at: ProtocolMillis,
    command: F,
) -> Result<StateSnapshot, RuntimeError>
where
    F: FnOnce(ProtocolMillis) -> Command + Send + 'static,
{
    let store = store.clone();
    let race_id = race_id.to_owned();
    Ok(
        tokio::task::spawn_blocking(move || store.execute_now(&race_id, proposed_at, command))
            .await??,
    )
}
