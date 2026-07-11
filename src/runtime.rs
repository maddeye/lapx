pub use crate::store::StateSnapshot;
use crate::{
    domain::{Command, ProtocolMillis},
    store::{SqliteStore, StoreError},
};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Notify, broadcast},
    task::JoinError,
    time::Instant,
};

const REFRESH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub enum RuntimeError {
    Store(StoreError),
    Task(JoinError),
    ClockOverflow,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
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

pub struct RaceRuntime {
    store: SqliteStore,
    race_id: String,
    apply_boundary: Mutex<()>,
    updates: broadcast::Sender<StateSnapshot>,
    due_changed: Arc<Notify>,
    published_sequence: AtomicU64,
    protocol_anchor: ProtocolMillis,
    instant_anchor: Instant,
}

impl RaceRuntime {
    pub async fn new(
        store: SqliteStore,
        race_id: impl Into<String>,
    ) -> Result<Arc<Self>, RuntimeError> {
        let race_id = race_id.into();
        let initial_store = store.clone();
        let initial_race_id = race_id.clone();
        let initial =
            tokio::task::spawn_blocking(move || initial_store.load(&initial_race_id)).await??;
        let protocol_anchor = initial.state.last_event_at.unwrap_or(0);
        let (updates, _) = broadcast::channel(16);
        let runtime = Arc::new(Self {
            store,
            race_id,
            apply_boundary: Mutex::new(()),
            updates,
            due_changed: Arc::new(Notify::new()),
            published_sequence: AtomicU64::new(initial.sequence),
            protocol_anchor,
            instant_anchor: Instant::now(),
        });
        Self::spawn_due_task(&runtime);
        Ok(runtime)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StateSnapshot> {
        self.updates.subscribe()
    }

    pub async fn snapshot(&self) -> Result<StateSnapshot, RuntimeError> {
        let _guard = self.apply_boundary.lock().await;
        self.load().await
    }

    pub async fn apply(&self, command: Command) -> Result<StateSnapshot, RuntimeError> {
        let _guard = self.apply_boundary.lock().await;
        self.execute(command).await
    }

    pub async fn apply_now(
        &self,
        command: impl FnOnce(ProtocolMillis) -> Command,
    ) -> Result<StateSnapshot, RuntimeError> {
        let _guard = self.apply_boundary.lock().await;
        self.execute(command(self.protocol_now()?)).await
    }

    pub fn protocol_now(&self) -> Result<ProtocolMillis, RuntimeError> {
        let elapsed = u64::try_from(self.instant_anchor.elapsed().as_millis())
            .map_err(|_| RuntimeError::ClockOverflow)?;
        self.protocol_anchor
            .checked_add(elapsed)
            .ok_or(RuntimeError::ClockOverflow)
    }

    async fn execute(&self, command: Command) -> Result<StateSnapshot, RuntimeError> {
        let store = self.store.clone();
        let race_id = self.race_id.clone();
        let snapshot =
            tokio::task::spawn_blocking(move || store.execute(&race_id, command)).await??;
        self.publish(&snapshot);
        self.due_changed.notify_one();
        Ok(snapshot)
    }

    async fn load(&self) -> Result<StateSnapshot, RuntimeError> {
        let store = self.store.clone();
        let race_id = self.race_id.clone();
        Ok(tokio::task::spawn_blocking(move || store.load(&race_id)).await??)
    }

    fn publish(&self, snapshot: &StateSnapshot) {
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
        self.publish(&snapshot);
        let Some(due_at) = snapshot.state.next_due_at().map_err(StoreError::Domain)? else {
            return Ok(None);
        };
        Ok(Some(Duration::from_millis(
            due_at.saturating_sub(self.protocol_now()?),
        )))
    }
}
