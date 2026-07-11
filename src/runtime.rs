pub use crate::store::StateSnapshot;
use crate::{
    domain::{Command, ProtocolMillis, RaceControl, RaceStatus},
    hardware::{
        self, HardwareConfig, HardwareError, HardwareMonitor, HardwareSnapshot, PowerOutput,
        RawEdge, TimingSource,
    },
    store::{SqliteStore, StoreError},
};
use std::{
    fmt,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Notify, broadcast, mpsc},
    task::JoinError,
    time::Instant,
};

const REFRESH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub enum RuntimeError {
    Store(StoreError),
    Task(JoinError),
    Hardware(HardwareError),
    PowerAfterCommit {
        sequence: u64,
        source: HardwareError,
    },
    ClockOverflow,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PowerAfterCommit { sequence, source } => write!(
                f,
                "race state committed at sequence {sequence}; power synchronization failed: {source}"
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
}

impl ProtocolClock {
    fn now(&self) -> Result<ProtocolMillis, RuntimeError> {
        self.at(Instant::now())
    }

    fn at(&self, instant: Instant) -> Result<ProtocolMillis, RuntimeError> {
        if instant >= self.instant_anchor {
            self.protocol_at_anchor
                .checked_add(duration_millis(instant - self.instant_anchor)?)
                .ok_or(RuntimeError::ClockOverflow)
        } else {
            self.protocol_at_anchor
                .checked_sub(duration_millis(self.instant_anchor - instant)?)
                .ok_or(RuntimeError::ClockOverflow)
        }
    }

    fn observe(&mut self, at: ProtocolMillis) {
        if self.now().is_ok_and(|now| at > now) {
            self.protocol_at_anchor = at;
            self.instant_anchor = Instant::now();
        }
    }
}

fn duration_millis(duration: Duration) -> Result<u64, RuntimeError> {
    u64::try_from(duration.as_millis()).map_err(|_| RuntimeError::ClockOverflow)
}

struct HardwareRuntime {
    monitor: HardwareMonitor,
    power: StdMutex<Box<dyn PowerOutput>>,
    timing: StdMutex<Box<dyn TimingSource>>,
}

pub struct RaceRuntime {
    store: SqliteStore,
    race_id: String,
    apply_boundary: Mutex<()>,
    updates: broadcast::Sender<StateSnapshot>,
    due_changed: Arc<Notify>,
    published_sequence: AtomicU64,
    clock: StdMutex<ProtocolClock>,
    hardware: Option<HardwareRuntime>,
}

impl RaceRuntime {
    pub async fn new(
        store: SqliteStore,
        race_id: impl Into<String>,
    ) -> Result<Arc<Self>, RuntimeError> {
        let race_id = race_id.into();
        let initial = load_snapshot(&store, &race_id).await?;
        let runtime = Self::build(store, race_id, initial, None);
        Self::spawn_due_task(&runtime);
        Ok(runtime)
    }

    pub async fn with_hardware<T, P>(
        store: SqliteStore,
        race_id: impl Into<String>,
        config: HardwareConfig,
        timing: T,
        mut power: P,
    ) -> Result<Arc<Self>, RuntimeError>
    where
        T: TimingSource + 'static,
        P: PowerOutput + 'static,
    {
        power.set_lane_power([false; 4])?;
        let race_id = race_id.into();
        let initial = load_snapshot(&store, &race_id).await?;
        let (monitor, sink, receiver) = hardware::channel(config);
        monitor.record_outputs([false; 4]);
        let hardware = HardwareRuntime {
            monitor,
            power: StdMutex::new(Box::new(power)),
            timing: StdMutex::new(Box::new(timing)),
        };
        let recovery_needed = matches!(
            &initial.state.status,
            RaceStatus::Active(active)
                if matches!(active.control, RaceControl::Live | RaceControl::Restarting { .. })
        );
        let runtime = Self::build(store, race_id, initial, Some(hardware));

        if recovery_needed {
            runtime.apply_now(|at| Command::PauseRace { at }).await?;
        }
        runtime
            .hardware
            .as_ref()
            .expect("hardware runtime exists")
            .timing
            .lock()
            .map_err(|_| RuntimeError::Hardware(HardwareError::new("timing source lock poisoned")))?
            .start(sink)?;
        Self::spawn_edge_task(&runtime, receiver);
        Self::spawn_due_task(&runtime);
        Ok(runtime)
    }

    fn build(
        store: SqliteStore,
        race_id: String,
        initial: StateSnapshot,
        hardware: Option<HardwareRuntime>,
    ) -> Arc<Self> {
        let protocol_anchor = initial.state.last_event_at.unwrap_or(0);
        let (updates, _) = broadcast::channel(16);
        Arc::new(Self {
            store,
            race_id,
            apply_boundary: Mutex::new(()),
            updates,
            due_changed: Arc::new(Notify::new()),
            published_sequence: AtomicU64::new(initial.sequence),
            clock: StdMutex::new(ProtocolClock {
                protocol_at_anchor: protocol_anchor,
                instant_anchor: Instant::now(),
            }),
            hardware,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StateSnapshot> {
        self.updates.subscribe()
    }

    pub async fn snapshot(&self) -> Result<StateSnapshot, RuntimeError> {
        let _guard = self.apply_boundary.lock().await;
        self.load().await
    }

    pub fn hardware_snapshot(&self) -> Option<HardwareSnapshot> {
        let hardware = self.hardware.as_ref()?;
        if let Ok(clock) = self.clock.lock() {
            hardware
                .monitor
                .map_protocols(|captured_at| clock.at(captured_at).ok());
        }
        Some(hardware.monitor.snapshot())
    }

    pub async fn apply(&self, command: Command) -> Result<StateSnapshot, RuntimeError> {
        let _guard = self.apply_boundary.lock().await;
        self.execute(command).await
    }

    pub async fn apply_now<F>(&self, command: F) -> Result<StateSnapshot, RuntimeError>
    where
        F: FnOnce(ProtocolMillis) -> Command + Send + 'static,
    {
        let _guard = self.apply_boundary.lock().await;
        self.execute_now(self.protocol_now()?, command).await
    }

    pub fn protocol_now(&self) -> Result<ProtocolMillis, RuntimeError> {
        self.clock
            .lock()
            .map_err(|_| RuntimeError::ClockOverflow)?
            .now()
    }

    async fn apply_edge(&self, edge: RawEdge) -> Result<StateSnapshot, RuntimeError> {
        let _guard = self.apply_boundary.lock().await;
        let at = self
            .clock
            .lock()
            .map_err(|_| RuntimeError::ClockOverflow)?
            .at(edge.captured_at)?;
        if let Some(hardware) = &self.hardware {
            hardware.monitor.record_protocol(&edge, at);
        }
        self.execute_now(at, move |at| Command::SensorTriggered {
            lane: edge.lane,
            edge: edge.edge,
            at,
        })
        .await
    }

    async fn execute(&self, command: Command) -> Result<StateSnapshot, RuntimeError> {
        let store = self.store.clone();
        let race_id = self.race_id.clone();
        let snapshot =
            tokio::task::spawn_blocking(move || store.execute(&race_id, command)).await??;
        self.after_commit(snapshot)
    }

    async fn execute_now<F>(
        &self,
        proposed_at: ProtocolMillis,
        command: F,
    ) -> Result<StateSnapshot, RuntimeError>
    where
        F: FnOnce(ProtocolMillis) -> Command + Send + 'static,
    {
        let store = self.store.clone();
        let race_id = self.race_id.clone();
        let snapshot =
            tokio::task::spawn_blocking(move || store.execute_now(&race_id, proposed_at, command))
                .await??;
        self.after_commit(snapshot)
    }

    fn after_commit(&self, snapshot: StateSnapshot) -> Result<StateSnapshot, RuntimeError> {
        self.due_changed.notify_one();
        if let Err(source) = self.sync_power(&snapshot) {
            self.publish(&snapshot);
            return Err(RuntimeError::PowerAfterCommit {
                sequence: snapshot.sequence,
                source,
            });
        }
        self.publish(&snapshot);
        Ok(snapshot)
    }

    fn sync_power(&self, snapshot: &StateSnapshot) -> Result<(), HardwareError> {
        let Some(hardware) = &self.hardware else {
            return Ok(());
        };
        let desired = std::array::from_fn(|index| {
            snapshot
                .state
                .intended_lane_power(index as u8 + 1)
                .unwrap_or(false)
        });
        let mut power = hardware
            .power
            .lock()
            .map_err(|_| HardwareError::new("power output lock poisoned"))?;
        hardware.monitor.record_outputs(desired);
        if let Err(error) = power.set_lane_power(desired) {
            hardware.monitor.record_outputs([false; 4]);
            let message = match power.set_lane_power([false; 4]) {
                Ok(()) => error.to_string(),
                Err(fail_safe) => format!("{error}; fail-safe all-off also failed: {fail_safe}"),
            };
            hardware.monitor.record_error(message.clone());
            return Err(HardwareError::new(message));
        }
        Ok(())
    }

    async fn load(&self) -> Result<StateSnapshot, RuntimeError> {
        load_snapshot(&self.store, &self.race_id).await
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

    fn spawn_edge_task(runtime: &Arc<Self>, mut receiver: mpsc::UnboundedReceiver<RawEdge>) {
        let runtime = Arc::downgrade(runtime);
        tokio::spawn(async move {
            while let Some(edge) = receiver.recv().await {
                let Some(runtime) = runtime.upgrade() else {
                    break;
                };
                if let Err(error) = runtime.apply_edge(edge).await {
                    if let Some(hardware) = &runtime.hardware {
                        hardware.monitor.record_error(error.to_string());
                    }
                    eprintln!("timing edge apply failed: {error}");
                }
            }
        });
    }

    fn spawn_due_task(runtime: &Arc<Self>) {
        let runtime = Arc::downgrade(runtime);
        tokio::spawn(async move {
            loop {
                let Some(current) = runtime.upgrade() else {
                    break;
                };
                let notify = current.due_changed.clone();
                let wait = match current.refresh_and_wait().await {
                    Ok(Some(Duration::ZERO)) => {
                        if let Err(error) =
                            current.apply_now(|to| Command::AdvanceRace { to }).await
                        {
                            eprintln!("race due apply failed: {error}");
                            Some(REFRESH_INTERVAL)
                        } else {
                            None
                        }
                    }
                    Ok(wait) => Some(wait.unwrap_or(REFRESH_INTERVAL).min(REFRESH_INTERVAL)),
                    Err(error) => {
                        eprintln!("race state refresh failed: {error}");
                        Some(REFRESH_INTERVAL)
                    }
                };
                drop(current);

                if let Some(wait) = wait {
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        () = notify.notified() => {}
                    }
                }
            }
        });
    }

    async fn refresh_and_wait(&self) -> Result<Option<Duration>, RuntimeError> {
        let _guard = self.apply_boundary.lock().await;
        let snapshot = self.load().await?;
        if snapshot.sequence > self.published_sequence.load(Ordering::Acquire) {
            self.after_commit(snapshot.clone())?;
        } else {
            self.publish(&snapshot);
        }
        let Some(due_at) = snapshot.state.next_due_at().map_err(StoreError::Domain)? else {
            return Ok(None);
        };
        Ok(Some(Duration::from_millis(
            due_at.saturating_sub(self.protocol_now()?),
        )))
    }
}

async fn load_snapshot(store: &SqliteStore, race_id: &str) -> Result<StateSnapshot, RuntimeError> {
    let store = store.clone();
    let race_id = race_id.to_owned();
    Ok(tokio::task::spawn_blocking(move || store.load(&race_id)).await??)
}
