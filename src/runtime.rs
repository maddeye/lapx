pub use crate::store::StateSnapshot;
use crate::{
    domain::{Command, ProtocolMillis},
    store::{SqliteStore, StoreError},
};
use std::{fmt, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, Notify, broadcast},
    time::Instant,
};

#[derive(Debug)]
pub enum RuntimeError {
    Store(StoreError),
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

pub struct RaceRuntime {
    store: SqliteStore,
    race_id: String,
    apply_boundary: Mutex<()>,
    updates: broadcast::Sender<StateSnapshot>,
    due_changed: Arc<Notify>,
    protocol_anchor: ProtocolMillis,
    instant_anchor: Instant,
}

impl RaceRuntime {
    pub fn new(store: SqliteStore, race_id: impl Into<String>) -> Result<Arc<Self>, StoreError> {
        let race_id = race_id.into();
        let protocol_anchor = store.load(&race_id)?.state.last_event_at.unwrap_or(0);
        let (updates, _) = broadcast::channel(16);
        let runtime = Arc::new(Self {
            store,
            race_id,
            apply_boundary: Mutex::new(()),
            updates,
            due_changed: Arc::new(Notify::new()),
            protocol_anchor,
            instant_anchor: Instant::now(),
        });
        Self::spawn_due_task(&runtime);
        Ok(runtime)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StateSnapshot> {
        self.updates.subscribe()
    }

    pub async fn snapshot(&self) -> Result<StateSnapshot, StoreError> {
        let _guard = self.apply_boundary.lock().await;
        self.store.load(&self.race_id)
    }

    pub async fn apply(&self, command: Command) -> Result<StateSnapshot, RuntimeError> {
        let _guard = self.apply_boundary.lock().await;
        self.execute(command)
    }

    pub async fn apply_now(
        &self,
        command: impl FnOnce(ProtocolMillis) -> Command,
    ) -> Result<StateSnapshot, RuntimeError> {
        let _guard = self.apply_boundary.lock().await;
        self.execute(command(self.protocol_now()?))
    }

    pub fn protocol_now(&self) -> Result<ProtocolMillis, RuntimeError> {
        let elapsed = u64::try_from(self.instant_anchor.elapsed().as_millis())
            .map_err(|_| RuntimeError::ClockOverflow)?;
        self.protocol_anchor
            .checked_add(elapsed)
            .ok_or(RuntimeError::ClockOverflow)
    }

    fn execute(&self, command: Command) -> Result<StateSnapshot, RuntimeError> {
        let snapshot = self.store.execute(&self.race_id, command)?;
        let _ = self.updates.send(snapshot.clone());
        self.due_changed.notify_one();
        Ok(snapshot)
    }

    fn spawn_due_task(runtime: &Arc<Self>) {
        let runtime = Arc::downgrade(runtime);
        tokio::spawn(async move {
            loop {
                let Some(current) = runtime.upgrade() else {
                    break;
                };
                let notify = current.due_changed.clone();
                let wait = match current.wait_until_due().await {
                    Ok(wait) => wait,
                    Err(_) => break,
                };
                drop(current);

                match wait {
                    Some(Duration::ZERO) => {
                        let Some(current) = runtime.upgrade() else {
                            break;
                        };
                        if current
                            .apply_now(|to| Command::AdvanceRace { to })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(duration) => {
                        tokio::select! {
                            () = tokio::time::sleep(duration) => {}
                            () = notify.notified() => {}
                        }
                    }
                    None => notify.notified().await,
                }
            }
        });
    }

    async fn wait_until_due(&self) -> Result<Option<Duration>, RuntimeError> {
        let snapshot = self.snapshot().await?;
        let Some(due_at) = snapshot.state.next_due_at().map_err(StoreError::Domain)? else {
            return Ok(None);
        };
        Ok(Some(Duration::from_millis(
            due_at.saturating_sub(self.protocol_now()?),
        )))
    }
}
