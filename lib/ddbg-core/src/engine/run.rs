use super::*;

impl Engine {
    /// Execution resumed: no frame/variable reference survives this.
    pub(super) fn mark_resumed(&mut self, announce: bool) {
        self.halt.send_replace(None);
        self.session.on_resume();
        let was_stopped = self.session.status.is_stopped();
        self.session.status = SessionStatus::Running;
        let watches = self.session.watches.snapshot();
        if !watches.is_empty() {
            self.emit(DebugEvent::WatchesChanged(watches));
        }
        if announce && was_stopped {
            self.emit(DebugEvent::SessionContinued);
        }
    }

    pub(super) async fn cmd_run(&mut self, target: Option<LaunchTarget>) -> Result<Reply> {
        if let Some(t) = target {
            self.session.target = Some(DebugTarget::Launch(t));
        }
        let target = match &self.session.target {
            Some(DebugTarget::Launch(t)) => t.clone(),
            Some(DebugTarget::Attach(_)) => {
                bail!("the current target is attached; use `attach <pid>` or `run <program>`")
            }
            None => bail!(NO_TARGET),
        };
        if self.conn.is_some() {
            self.shutdown().await;
        }
        match self
            .start_target(&DebugTarget::Launch(target.clone()))
            .await
        {
            Ok(()) => Ok(Reply::Launched(target.absolute_program())),
            Err(e) => {
                self.shutdown().await;
                let program = target.absolute_program();
                // Adapters report a missing executable obscurely, e.g. lldb-dap's
                // "Process is in an unexpected state" from configurationDone.
                if target.program.components().count() > 1 && !program.exists() {
                    bail!(
                        "{} does not exist; build it first (debugger error: {e})",
                        program.display()
                    );
                }
                Err(e)
            }
        }
    }

    pub(super) async fn cmd_attach(&mut self, target: AttachTarget) -> Result<Reply> {
        if target.pid == 0 {
            bail!("process ID must be greater than zero");
        }
        // Validate arguments before replacing a live session.
        self.adapter.build_attach_request(&target)?;
        self.shutdown().await;
        self.session.target = Some(DebugTarget::Attach(target.clone()));
        match self
            .start_target(&DebugTarget::Attach(target.clone()))
            .await
        {
            Ok(()) => Ok(Reply::Attached(target.pid)),
            Err(e) => {
                self.shutdown().await;
                Err(e)
            }
        }
    }

    pub(super) async fn start_target(&mut self, target: &DebugTarget) -> Result<()> {
        tokio::time::timeout(STARTUP_TIMEOUT, self.start(target))
            .await
            .map_err(|_| anyhow!("debug adapter startup timed out after {STARTUP_TIMEOUT:?}"))?
    }

    /// Shared DAP lifecycle: initialize → launch/attach → `initialized`
    /// → breakpoints → configurationDone → launch/attach response.
    pub(super) async fn start(&mut self, target: &DebugTarget) -> Result<()> {
        let (args, attached) = match target {
            DebugTarget::Launch(t) => (
                self.adapter.build_launch_request(&with_color_env(t))?,
                false,
            ),
            DebugTarget::Attach(t) => (self.adapter.build_attach_request(t)?, true),
        };
        let conn = (self.connector)(self.adapter.as_ref())?;
        let client = conn.client.clone();
        let mut incoming = conn.incoming;
        self.conn = Some(ActiveConnection {
            client: client.clone(),
            child: conn.child,
            attached,
        });
        self.session.status = SessionStatus::Initializing;
        self.session.exit_code = None;
        self.halt.send_replace(None);

        let caps = client
            .request(InitializeArguments::new(self.adapter.id()))
            .await?;
        self.session.capabilities = caps;

        // Conditions can be configured before capabilities are known. Refuse
        // startup rather than silently installing unconditional breakpoints.
        if self
            .session
            .breakpoints
            .iter()
            .any(|bp| bp.condition.is_some())
            && !self.session.supports(Feature::ConditionalBreakpoints)
        {
            bail!(NO_CONDITIONAL_BREAKPOINTS);
        }

        let mut start_response = if attached {
            client.send(AttachArguments(args))?
        } else {
            client.send(LaunchArguments(args))?
        };
        let mut started = false;
        loop {
            tokio::select! {
                r = &mut start_response, if !started => {
                    r?;
                    started = true;
                }
                msg = incoming.recv() => match msg {
                    Some(Incoming::Event(DapEvent::Initialized)) => break,
                    None => return Err(channel_closed()),
                    other => self.handle_incoming(other).await,
                }
            }
        }

        if matches!(self.session.status, SessionStatus::Initializing) {
            self.session.status = SessionStatus::Configuring;
        }
        for file in self.session.breakpoints.files() {
            self.sync_breakpoints(&file).await?;
        }
        if self.session.breakpoints.has_functions() {
            self.sync_function_breakpoints().await?;
        }
        self.resumed_at = Some(Instant::now());
        if self.session.supports(Feature::ConfigurationDone) {
            client.request(ConfigurationDoneArguments {}).await?;
        }
        if !started {
            start_response.await?;
        }
        self.incoming = Some(incoming);
        // A `stopped` event (e.g. stop-on-entry) may already have arrived.
        if matches!(self.session.status, SessionStatus::Configuring) {
            self.session.status = SessionStatus::Running;
        }
        self.emit(DebugEvent::SessionStarted);
        Ok(())
    }

    pub(super) async fn try_pause(&mut self) -> Result<()> {
        if !matches!(self.session.status, SessionStatus::Running) {
            self.pause_pending = false;
            return Ok(());
        }
        self.refresh_threads().await?;
        let tid = self
            .session
            .selected_thread
            .filter(|id| self.session.threads.iter().any(|t| t.id == *id))
            .or_else(|| self.session.threads.first().map(|t| t.id));
        if let Some(tid) = tid {
            self.client()?
                .request(PauseArguments { thread_id: tid.0 })
                .await?;
            self.pause_pending = false;
        }
        // No threads yet: retry from the main loop, keeping incoming events
        // and interruption commands usable instead of sleeping inside a command.
        Ok(())
    }

    /// End the session: launched processes terminate, attached ones detach.
    pub(super) async fn shutdown(&mut self) {
        let terminate = self.conn.as_ref().is_some_and(|c| !c.attached);
        if let Err(e) = self.disconnect(terminate).await {
            tracing::warn!("could not disconnect debug adapter: {e}");
        }
    }

    pub(super) async fn disconnect(&mut self, terminate: bool) -> Result<()> {
        self.pause_pending = false;
        let Some(mut conn) = self.conn.take() else {
            return Ok(());
        };
        let result = tokio::time::timeout(
            SHUTDOWN_TIMEOUT,
            conn.client.request(DisconnectArguments {
                terminate_debuggee: self
                    .session
                    .supports(Feature::TerminateDebuggee)
                    .then_some(terminate),
            }),
        )
        .await;
        if let Some(child) = conn.child.as_mut()
            && tokio::time::timeout(SHUTDOWN_TIMEOUT, child.wait())
                .await
                .is_err()
        {
            let _ = child.kill().await;
        }
        self.incoming = None;
        let was_active = self.session.status.is_active() || self.session.exit_code.is_some();
        self.session.on_terminated();
        if was_active {
            self.emit(DebugEvent::SessionTerminated);
        }
        result.map_err(|_| anyhow!("debug adapter disconnect timed out"))??;
        Ok(())
    }
    pub(super) async fn cmd_detach(&mut self) -> Result<Reply> {
        if !self.session.status.is_active() {
            bail!(NOT_RUNNING);
        }
        if !self.conn.as_ref().is_some_and(|c| c.attached)
            && !self.session.supports(Feature::TerminateDebuggee)
        {
            bail!("the debug adapter does not support detaching from a launched process");
        }
        self.disconnect(false).await?;
        Ok(Reply::Ok)
    }

    pub(super) async fn cmd_continue(&mut self) -> Result<Reply> {
        let tid = self.require_stopped()?;
        self.continuing = true;
        self.resumed_at = Some(Instant::now());
        let client = self.client()?;
        client
            .request(ContinueArguments { thread_id: tid.0 })
            .await?;
        self.mark_resumed(true);
        Ok(Reply::Ok)
    }

    pub(super) async fn cmd_pause(&mut self) -> Result<Reply> {
        match self.session.status {
            SessionStatus::Running => {}
            SessionStatus::Stopped(_) => bail!(ALREADY_STOPPED),
            _ => bail!(NOT_RUNNING),
        }
        self.pause_pending = true;
        if let Err(e) = self.try_pause().await {
            self.pause_pending = false;
            return Err(e);
        }
        Ok(Reply::Ok)
    }

    pub(super) async fn cmd_kill(&mut self) -> Result<Reply> {
        if !self.session.status.is_active() {
            bail!(NOT_RUNNING);
        }
        if self.conn.as_ref().is_some_and(|c| c.attached)
            && !self.session.supports(Feature::TerminateDebuggee)
        {
            bail!(
                "the debug adapter does not support terminating an attached process; use `detach`"
            );
        }
        self.disconnect(true).await?;
        Ok(Reply::Ok)
    }

    pub(super) async fn cmd_next(&mut self) -> Result<Reply> {
        let tid = self.require_stopped()?;
        self.continuing = false;
        self.resumed_at = Some(Instant::now());
        self.client()?
            .request(NextArguments { thread_id: tid.0 })
            .await?;
        self.mark_resumed(false);
        Ok(Reply::Ok)
    }

    pub(super) async fn cmd_step(&mut self) -> Result<Reply> {
        let tid = self.require_stopped()?;
        self.continuing = false;
        self.resumed_at = Some(Instant::now());
        self.client()?
            .request(StepInArguments { thread_id: tid.0 })
            .await?;
        self.mark_resumed(false);
        Ok(Reply::Ok)
    }

    pub(super) async fn cmd_finish(&mut self) -> Result<Reply> {
        let tid = self.require_stopped()?;
        self.continuing = false;
        self.resumed_at = Some(Instant::now());
        self.client()?
            .request(StepOutArguments { thread_id: tid.0 })
            .await?;
        self.mark_resumed(false);
        Ok(Reply::Ok)
    }

    pub(super) async fn cmd_quit(&mut self) -> Result<Reply> {
        self.shutdown().await;
        Ok(Reply::Quit)
    }
}

/// The debuggee's output is piped through the adapter, so programs would
/// normally disable colors. Ask common runtimes to emit ANSI colors anyway,
/// unless `NO_COLOR` is set or the variable is already provided.
fn with_color_env(target: &LaunchTarget) -> LaunchTarget {
    let mut t = target.clone();
    if std::env::var_os("NO_COLOR").is_some() || t.env.contains_key("NO_COLOR") {
        return t;
    }
    for (key, value) in [
        ("DOTNET_SYSTEM_CONSOLE_ALLOW_ANSI_COLOR_REDIRECTION", "1"),
        (
            "Logging__Console__FormatterOptions__ColorBehavior",
            "Enabled",
        ),
        ("FORCE_COLOR", "1"),
        ("CLICOLOR_FORCE", "1"),
        ("CARGO_TERM_COLOR", "always"),
    ] {
        if std::env::var_os(key).is_none() {
            t.env
                .entry(key.to_owned())
                .or_insert_with(|| value.to_owned());
        }
    }
    t
}
