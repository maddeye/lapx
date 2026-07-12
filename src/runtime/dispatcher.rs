use super::*;

pub(super) struct DispatcherHardware {
    pub(super) config: HardwareConfig,
    pub(super) monitor: HardwareMonitor,
    pub(super) power: Arc<StdMutex<Box<dyn PowerOutput>>>,
    pub(super) fault: Option<HardwareError>,
}

pub(super) struct Dispatcher {
    store: SqliteStore,
    race_id: String,
    head: StateSnapshot,
    receiver: mpsc::UnboundedReceiver<Ingress>,
    shared: Arc<Shared>,
    hardware: Option<DispatcherHardware>,
    refresh_at: Instant,
    timer_retry_at: Instant,
}

impl Dispatcher {
    pub(super) fn spawn(
        store: SqliteStore,
        race_id: String,
        head: StateSnapshot,
        receiver: mpsc::UnboundedReceiver<Ingress>,
        shared: Arc<Shared>,
        hardware: Option<DispatcherHardware>,
    ) {
        tokio::spawn(
            Self {
                store,
                race_id,
                head,
                receiver,
                shared,
                hardware,
                refresh_at: Instant::now() + REFRESH_INTERVAL,
                timer_retry_at: Instant::now(),
            }
            .run(),
        );
    }

    async fn run(mut self) {
        loop {
            let wake_at = self.wake_at();
            tokio::select! {
                biased;
                ingress = self.receiver.recv() => {
                    let Some(ingress) = ingress else { break };
                    self.handle_ingress(ingress).await;
                }
                () = tokio::time::sleep_until(wake_at) => self.handle_timer().await,
            }
        }
    }

    fn wake_at(&self) -> Instant {
        let refresh_at = self.refresh_at.max(self.timer_retry_at);
        let due_at = self
            .head
            .state
            .next_due_at()
            .ok()
            .flatten()
            .and_then(|due| {
                self.shared
                    .protocol_now()
                    .ok()
                    .map(|now| Instant::now() + Duration::from_millis(due.saturating_sub(now)))
            })
            .map(|due| due.max(self.timer_retry_at))
            .unwrap_or(refresh_at);
        due_at.min(refresh_at)
    }

    async fn handle_ingress(&mut self, ingress: Ingress) {
        if self.hardware.is_some() {
            self.settle_batch(Some(ingress)).await;
        } else {
            self.process_ingress(ingress).await;
        }
    }

    async fn handle_timer(&mut self) {
        let mut now = match self.shared.protocol_now() {
            Ok(now) => now,
            Err(error) => {
                eprintln!("race clock failed: {error}");
                self.timer_retry_at = Instant::now() + REFRESH_INTERVAL;
                return;
            }
        };
        let mut due = self.head.state.next_due_at().ok().flatten();
        if self.hardware.is_some() && due.is_some_and(|due| due <= now) {
            self.settle_batch(None).await;
            now = match self.shared.protocol_now() {
                Ok(now) => now,
                Err(error) => {
                    eprintln!("race clock failed after capture settle: {error}");
                    self.timer_retry_at = Instant::now() + REFRESH_INTERVAL;
                    return;
                }
            };
            due = self.head.state.next_due_at().ok().flatten();
        }
        let result = if due.is_some_and(|due| due <= now) {
            self.execute_command(Command::AdvanceRace { to: now }, false)
                .await
                .map(|_| ())
        } else {
            self.refresh_external().await
        };
        match result {
            Ok(()) => self.timer_retry_at = Instant::now(),
            Err(error) => {
                eprintln!("race timer apply failed: {error}");
                self.timer_retry_at = Instant::now() + REFRESH_INTERVAL;
            }
        }
    }

    async fn settle_batch(&mut self, first: Option<Ingress>) {
        tokio::time::sleep(CAPTURE_SETTLE_WINDOW).await;
        let mut batch: Vec<_> = first.into_iter().collect();
        while let Ok(ingress) = self.receiver.try_recv() {
            batch.push(ingress);
        }

        let mut ordered = Vec::new();
        let mut snapshots = Vec::new();
        for (arrival, ingress) in batch.into_iter().enumerate() {
            if matches!(&ingress, Ingress::Snapshot { .. }) {
                snapshots.push(ingress);
            } else {
                ordered.push((arrival, ingress));
            }
        }
        self.process_ordered(ordered).await;
        for snapshot in snapshots {
            self.process_ingress(snapshot).await;
        }
    }

    async fn process_ordered(&mut self, mut items: Vec<(usize, Ingress)>) {
        items.sort_by_key(|(arrival, ingress)| {
            let (at, priority) = match ingress {
                Ingress::Edge(edge) => (edge.at, 0),
                Ingress::Command { request, .. } => (
                    match request {
                        CommandRequest::Exact(command) | CommandRequest::Now(command) => {
                            command.timestamp()
                        }
                    },
                    1,
                ),
                Ingress::SwitchRace { .. } => (u64::MAX, 2),
                Ingress::Snapshot { .. } => unreachable!("snapshots are batch barriers"),
            };
            (at, priority, *arrival)
        });
        for (_, ingress) in items {
            self.process_ingress(ingress).await;
        }
    }

    async fn process_ingress(&mut self, ingress: Ingress) {
        match ingress {
            Ingress::Command { request, response } => {
                let _ = response.send(self.handle_command(request).await);
            }
            Ingress::Edge(edge) => {
                if let Err(error) = self.handle_edge(edge).await {
                    if let Some(hardware) = &self.hardware {
                        hardware.monitor.record_error(error.to_string());
                    }
                    eprintln!("timing edge apply failed: {error}");
                }
            }
            Ingress::Snapshot { response } => {
                let result = self.refresh_external().await.map(|()| self.head.clone());
                let _ = response.send(result);
            }
            Ingress::SwitchRace {
                expected_race_id,
                next_race_id,
                response,
            } => {
                let _ = response.send(
                    self.handle_switch_race(expected_race_id, next_race_id)
                        .await,
                );
            }
        }
    }

    async fn handle_edge(&mut self, edge: TimedEdge) -> CommandResult {
        let Some(hardware) = &self.hardware else {
            return Ok(self.head.clone());
        };
        if !hardware.monitor.record_edge(edge.lane, edge.edge, edge.at) {
            return Ok(self.head.clone());
        }
        let command = Command::SensorTriggered {
            lane: edge.lane,
            edge: edge.edge,
            at: edge.at,
        };
        loop {
            match self.execute_command(command.clone(), false).await {
                Err(RuntimeError::Store(error)) if error.is_busy() => {
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                }
                result => return result,
            }
        }
    }

    async fn handle_switch_race(
        &mut self,
        expected_race_id: String,
        next_race_id: String,
    ) -> CommandResult {
        let store = self.store.clone();
        let hardware = self
            .hardware
            .as_ref()
            .map(|hardware| (hardware.power.clone(), hardware.monitor.clone()));
        let committed = tokio::task::spawn_blocking(move || {
            store.switch_current_race_with(&expected_race_id, &next_race_id, || {
                if let Some((power, monitor)) = &hardware {
                    write_outputs(power, monitor, [false; 4])?;
                }
                Ok::<_, HardwareError>(())
            })
        })
        .await?;
        let committed = match committed {
            Ok(snapshot) => snapshot,
            Err(ImmediateError::Store(error)) => return Err(error.into()),
            Err(ImmediateError::Callback { source, .. }) => return Err(source.into()),
        };
        if let Some(hardware) = &mut self.hardware {
            hardware.fault = None;
        }
        self.race_id = committed.race_id.clone();
        self.accept_head(committed.clone());
        Ok(committed)
    }

    async fn handle_command(&mut self, request: CommandRequest) -> CommandResult {
        match request {
            CommandRequest::Exact(command) => self.execute_command(command, false).await,
            CommandRequest::Now(command) => self.execute_command(command, true).await,
        }
    }

    async fn execute_command(&mut self, command: Command, retime_now: bool) -> CommandResult {
        self.validate_hardware_lanes(&command)?;
        let resume = matches!(command, Command::ResumeRace { .. });
        let committed = if retime_now {
            let proposed_at = command.timestamp();
            let store = self.store.clone();
            let race_id = self.race_id.clone();
            tokio::task::spawn_blocking(move || {
                store.execute_now(&race_id, proposed_at, |at| retime(command, at))
            })
            .await??
        } else {
            let store = self.store.clone();
            let race_id = self.race_id.clone();
            tokio::task::spawn_blocking(move || store.execute(&race_id, command)).await??
        };
        self.project_committed(committed, resume).await
    }

    fn validate_hardware_lanes(&self, command: &Command) -> Result<(), RuntimeError> {
        let (Some(hardware), Command::StartRace { config, .. }) = (&self.hardware, command) else {
            return Ok(());
        };
        let configured = hardware.config.lanes.len() as u8;
        if config.lanes != configured {
            return Err(RuntimeError::HardwareLaneMismatch {
                configured,
                requested: config.lanes,
            });
        }
        Ok(())
    }

    async fn project_committed(&mut self, committed: StateSnapshot, resume: bool) -> CommandResult {
        if self.hardware.is_none() {
            self.accept_head(committed.clone());
            return Ok(committed);
        }
        match self.project_head(true, resume).await {
            Err(error @ (RuntimeError::Store(_) | RuntimeError::Task(_))) => {
                self.fail_safe_after_projection_gap(committed, error).await
            }
            result => result,
        }
    }

    async fn fail_safe_after_projection_gap(
        &mut self,
        fallback: StateSnapshot,
        error: RuntimeError,
    ) -> CommandResult {
        let mut source = HardwareError::new(format!(
            "state committed but current-head power projection failed: {error}"
        ));
        let hardware = self.hardware.as_mut().expect("hardware checked above");
        if let Err(all_off) = write_outputs(&hardware.power, &hardware.monitor, [false; 4]) {
            source = HardwareError::new(format!("{source}; fail-safe all-off failed: {all_off}"));
        }
        hardware.monitor.record_error(source.to_string());
        hardware.fault = Some(source.clone());

        let current = load_snapshot(&self.store, &self.race_id)
            .await
            .unwrap_or(fallback);
        let (snapshot, pause_error) = self.pause_for_fault(current, source.clone()).await;
        self.accept_head(snapshot.clone());
        Err(RuntimeError::PowerAfterCommit {
            snapshot: Box::new(snapshot),
            source: pause_error.unwrap_or(source),
        })
    }

    async fn refresh_external(&mut self) -> Result<(), RuntimeError> {
        if self.hardware.is_some() {
            match self.project_head(false, false).await {
                Err(error @ (RuntimeError::Store(_) | RuntimeError::Task(_))) => {
                    self.fail_safe_after_projection_gap(self.head.clone(), error)
                        .await?;
                }
                Err(error) => return Err(error),
                Ok(_) => {}
            }
        } else {
            let snapshot = load_snapshot(&self.store, &self.race_id).await?;
            self.accept_head(snapshot);
        }
        self.refresh_at = Instant::now() + REFRESH_INTERVAL;
        Ok(())
    }

    async fn project_head(&mut self, force_write: bool, resume: bool) -> CommandResult {
        let hardware = self.hardware.as_ref().expect("hardware checked above");
        let store = self.store.clone();
        let race_id = self.race_id.clone();
        let power = hardware.power.clone();
        let monitor = hardware.monitor.clone();
        let faulted = hardware.fault.is_some();
        let configured = hardware.config.lanes.len() as u8;
        let known_sequence = self.head.sequence;
        let guarded = tokio::task::spawn_blocking(move || {
            store.with_immediate_head(&race_id, |snapshot| {
                if let Some(mismatch) = hardware_lane_error(snapshot, configured) {
                    if let Err(all_off) = write_outputs(&power, &monitor, [false; 4]) {
                        return Err(HardwareError::new(format!(
                            "{mismatch}; mismatch fail-safe all-off failed: {all_off}"
                        )));
                    }
                    return Err(mismatch);
                }
                let write = force_write || snapshot.sequence != known_sequence;
                if !write || (faulted && !resume) {
                    return Ok::<bool, HardwareError>(false);
                }
                let desired = if faulted && resume {
                    [false; 4]
                } else {
                    desired_outputs(snapshot)
                };
                write_outputs(&power, &monitor, desired)?;
                Ok(faulted && resume && is_restarting(snapshot))
            })
        })
        .await?;

        match guarded {
            Ok((snapshot, clear_fault)) => {
                if clear_fault {
                    self.hardware.as_mut().unwrap().fault = None;
                } else if faulted && resume {
                    let source = HardwareError::new(
                        "resume committed without a guarded restarting head; power remains faulted",
                    );
                    let (snapshot, pause_error) =
                        self.pause_for_fault(snapshot, source.clone()).await;
                    self.accept_head(snapshot.clone());
                    return Err(RuntimeError::PowerAfterCommit {
                        snapshot: Box::new(snapshot),
                        source: pause_error.unwrap_or(source),
                    });
                } else if faulted && needs_pause(&snapshot) {
                    let source = self.hardware.as_ref().unwrap().fault.clone().unwrap();
                    let (snapshot, pause_error) = self.pause_for_fault(snapshot, source).await;
                    self.accept_head(snapshot.clone());
                    if let Some(source) = pause_error {
                        return Err(RuntimeError::PowerAfterCommit {
                            snapshot: Box::new(snapshot),
                            source,
                        });
                    }
                    return Ok(snapshot);
                }
                self.accept_head(snapshot.clone());
                Ok(snapshot)
            }
            Err(ImmediateError::Store(error)) => Err(error.into()),
            Err(ImmediateError::Callback { snapshot, source }) => {
                let hardware = self.hardware.as_mut().unwrap();
                hardware.monitor.record_error(source.to_string());
                hardware.fault = Some(source.clone());
                let (snapshot, pause_error) = self.pause_for_fault(*snapshot, source.clone()).await;
                self.accept_head(snapshot.clone());
                Err(RuntimeError::PowerAfterCommit {
                    snapshot: Box::new(snapshot),
                    source: pause_error.unwrap_or(source),
                })
            }
        }
    }

    async fn pause_for_fault(
        &mut self,
        snapshot: StateSnapshot,
        source: HardwareError,
    ) -> (StateSnapshot, Option<HardwareError>) {
        if !needs_pause(&snapshot) {
            return (snapshot, None);
        }
        let proposed_at = self
            .shared
            .protocol_now()
            .unwrap_or_else(|_| snapshot.state.last_event_at.unwrap_or(0));
        match execute_now(&self.store, &self.race_id, proposed_at, |at| {
            Command::PauseRace { at }
        })
        .await
        {
            Ok(paused) => (paused, None),
            Err(error) => {
                let error =
                    HardwareError::new(format!("{source}; durable fault pause failed: {error}"));
                if let Some(hardware) = &self.hardware {
                    hardware.monitor.record_error(error.to_string());
                }
                (snapshot, Some(error))
            }
        }
    }

    fn accept_head(&mut self, snapshot: StateSnapshot) {
        self.shared.publish(&snapshot);
        self.head = snapshot;
        self.refresh_at = Instant::now() + REFRESH_INTERVAL;
    }
}

pub(super) fn needs_pause(snapshot: &StateSnapshot) -> bool {
    matches!(
        &snapshot.state.status,
        RaceStatus::Active(active)
            if matches!(active.control, RaceControl::Live | RaceControl::Restarting { .. })
    )
}

fn is_restarting(snapshot: &StateSnapshot) -> bool {
    matches!(
        &snapshot.state.status,
        RaceStatus::Active(active) if matches!(active.control, RaceControl::Restarting { .. })
    )
}

fn desired_outputs(snapshot: &StateSnapshot) -> [bool; 4] {
    std::array::from_fn(|index| {
        snapshot
            .state
            .intended_lane_power(index as u8 + 1)
            .unwrap_or(false)
    })
}

fn write_outputs(
    power: &StdMutex<Box<dyn PowerOutput>>,
    monitor: &HardwareMonitor,
    desired: [bool; 4],
) -> Result<(), HardwareError> {
    let mut power = match power.lock() {
        Ok(power) => power,
        Err(poisoned) => poisoned.into_inner(),
    };
    let result = power.set_lane_power(desired);
    let fail_safe = result
        .as_ref()
        .err()
        .map(|_| power.set_lane_power([false; 4]));
    drop(power);

    match (result, fail_safe) {
        (Ok(()), _) => {
            monitor.record_outputs(desired);
            Ok(())
        }
        (Err(error), Some(Ok(()))) => {
            monitor.record_outputs([false; 4]);
            monitor.record_error(error.to_string());
            Err(error)
        }
        (Err(error), Some(Err(fail_safe))) => {
            let message = format!("{error}; fail-safe all-off also failed: {fail_safe}");
            monitor.record_error(message.clone());
            Err(HardwareError::new(message))
        }
        (Err(_), None) => unreachable!("failed writes always attempt fail-safe all-off"),
    }
}

fn retime(command: Command, at: ProtocolMillis) -> Command {
    match command {
        Command::StartRace { config, .. } => Command::StartRace { config, at },
        Command::AdvanceRace { .. } => Command::AdvanceRace { to: at },
        Command::SensorTriggered { lane, edge, .. } => Command::SensorTriggered { lane, edge, at },
        Command::CorrectLaps {
            lane,
            delta_thousandths,
            ..
        } => Command::CorrectLaps {
            lane,
            delta_thousandths,
            at,
        },
        Command::PauseRace { .. } => Command::PauseRace { at },
        Command::ResumeRace { .. } => Command::ResumeRace { at },
        Command::TriggerChaos { source, .. } => Command::TriggerChaos { source, at },
    }
}
