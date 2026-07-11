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
    pending: VecDeque<Ingress>,
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
                pending: VecDeque::new(),
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
            if let Some(ingress) = self.pending.pop_front() {
                self.handle_ingress(ingress).await;
                continue;
            }
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
            .unwrap_or(self.refresh_at);
        due_at.min(self.refresh_at)
    }

    async fn handle_ingress(&mut self, ingress: Ingress) {
        match ingress {
            Ingress::Command { request, response } => {
                if let Some(due) = self.crossed_due(&request) {
                    self.settle_edges_through(due).await;
                }
                let _ = response.send(self.handle_command(request).await);
            }
            Ingress::RawEdge(edge) => self.handle_edge_batch(edge).await,
            Ingress::Snapshot { response } => {
                let result = self.refresh_external().await.map(|()| self.head.clone());
                let _ = response.send(result);
            }
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
        if let Some(due_at) = due.filter(|due| self.hardware.is_some() && *due <= now) {
            self.settle_edges_through(due_at).await;
            now = match self.shared.protocol_now() {
                Ok(now) => now,
                Err(error) => {
                    eprintln!("race clock failed after edge settle: {error}");
                    self.timer_retry_at = Instant::now() + REFRESH_INTERVAL;
                    return;
                }
            };
            due = self.head.state.next_due_at().ok().flatten();
        }
        let result = if due.is_some_and(|due| due <= now) {
            self.handle_command(CommandRequest::Exact(Command::AdvanceRace { to: now }))
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

    fn crossed_due(&self, request: &CommandRequest) -> Option<ProtocolMillis> {
        self.hardware.as_ref()?;
        let at = match request {
            CommandRequest::Exact(command) => command.timestamp(),
            CommandRequest::Now(_) => self.shared.protocol_now().ok()?,
        };
        self.head
            .state
            .next_due_at()
            .ok()
            .flatten()
            .filter(|due| *due <= at)
    }

    async fn handle_edge_batch(&mut self, first: RawEdge) {
        let mut edges = vec![first];
        let mut remaining = VecDeque::new();
        while let Some(ingress) = self.pending.pop_front() {
            match ingress {
                Ingress::RawEdge(edge) => edges.push(edge),
                ingress => remaining.push_back(ingress),
            }
        }
        self.pending = remaining;
        while let Ok(ingress) = self.receiver.try_recv() {
            match ingress {
                Ingress::RawEdge(edge) => edges.push(edge),
                ingress => self.pending.push_back(ingress),
            }
        }
        self.apply_edges(edges).await;
    }

    async fn settle_edges_through(&mut self, due: ProtocolMillis) {
        tokio::time::sleep(CAPTURE_SETTLE_WINDOW).await;
        while let Ok(ingress) = self.receiver.try_recv() {
            self.pending.push_back(ingress);
        }

        let mut edges = Vec::new();
        let mut remaining = VecDeque::new();
        while let Some(ingress) = self.pending.pop_front() {
            match ingress {
                Ingress::RawEdge(edge) => match self.shared.protocol_at(edge.captured_at) {
                    Ok(at) if at > due => remaining.push_back(Ingress::RawEdge(edge)),
                    Ok(_) | Err(_) => edges.push(edge),
                },
                ingress => remaining.push_back(ingress),
            }
        }
        self.pending = remaining;
        self.apply_edges(edges).await;
    }

    async fn apply_edges(&mut self, mut edges: Vec<RawEdge>) {
        edges.sort_by_key(|edge| (edge.captured_at, edge.lane));
        for edge in edges {
            if let Err(error) = self.handle_edge(edge).await {
                if let Some(hardware) = &self.hardware {
                    hardware.monitor.record_error(error.to_string());
                }
                eprintln!("timing edge apply failed: {error}");
            }
        }
    }

    async fn handle_edge(&mut self, edge: RawEdge) -> CommandResult {
        let at = self.shared.protocol_at(edge.captured_at)?;
        let Some(hardware) = &self.hardware else {
            return Ok(self.head.clone());
        };
        if !hardware.monitor.record_edge(&edge, at) {
            return Ok(self.head.clone());
        }
        self.execute_command(
            Command::SensorTriggered {
                lane: edge.lane,
                edge: edge.edge,
                at,
            },
            false,
        )
        .await
    }

    async fn handle_command(&mut self, request: CommandRequest) -> CommandResult {
        match request {
            CommandRequest::Exact(command) => self.execute_command(command, false).await,
            CommandRequest::Now(command) => {
                let command = command(self.shared.protocol_now()?);
                self.execute_command(command, true).await
            }
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
        self.project_head(true, resume).await
    }

    async fn refresh_external(&mut self) -> Result<(), RuntimeError> {
        if self.hardware.is_some() {
            self.project_head(false, false).await?;
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
    let mut power = power
        .lock()
        .map_err(|_| HardwareError::new("power output lock poisoned"))?;
    match power.set_lane_power(desired) {
        Ok(()) => {
            monitor.record_outputs(desired);
            Ok(())
        }
        Err(error) => {
            let message = match power.set_lane_power([false; 4]) {
                Ok(()) => {
                    monitor.record_outputs([false; 4]);
                    error.to_string()
                }
                Err(fail_safe) => format!("{error}; fail-safe all-off also failed: {fail_safe}"),
            };
            monitor.record_error(message.clone());
            Err(HardwareError::new(message))
        }
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
