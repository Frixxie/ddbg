use super::*;

impl Engine {
    // -----------------------------------------------------------------------
    // Adapter messages
    // -----------------------------------------------------------------------

    pub(super) async fn handle_incoming(&mut self, msg: Option<Incoming>) {
        match msg {
            Some(Incoming::Event(event)) => {
                if let Err(e) = self.handle_event(event).await {
                    tracing::warn!("error handling adapter event: {e}");
                }
            }
            Some(Incoming::Request(req)) => {
                tracing::debug!("unsupported reverse request {}", req.command);
                if let Some(conn) = &self.conn {
                    let _ = conn.client.respond(
                        &req,
                        false,
                        Some(format!("{} is not supported by ddbg", req.command)),
                        None,
                    );
                }
            }
            None => {
                // Adapter went away.
                self.incoming = None;
                self.shutdown().await;
            }
        }
    }

    pub(super) async fn handle_event(&mut self, event: DapEvent) -> Result<()> {
        match event {
            DapEvent::Stopped(s) => self.on_stopped(s).await?,
            DapEvent::Continued(_) => {
                if self.session.status.is_stopped() {
                    self.resumed_at = Some(Instant::now());
                    self.mark_resumed(true);
                }
            }
            DapEvent::Exited(e) => {
                self.pause_pending = false;
                self.session.exit_code = Some(e.exit_code);
                self.session.on_resume();
                self.session.status = SessionStatus::Terminated;
                self.emit(DebugEvent::SessionExited(e.exit_code));
            }
            DapEvent::Terminated => self.shutdown().await,
            DapEvent::Output(o) => {
                if o.category.as_deref() != Some("telemetry") {
                    self.emit(DebugEvent::Output(Output::from_dap(
                        o.category.as_deref(),
                        o.output,
                    )));
                }
            }
            DapEvent::Thread(t) => {
                if t.reason == "exited" {
                    self.session.threads.retain(|th| th.id.0 != t.thread_id);
                }
                self.emit(DebugEvent::ThreadsChanged);
            }
            DapEvent::Breakpoint(b) => {
                let before = b
                    .breakpoint
                    .id
                    .and_then(|id| self.session.breakpoints.by_adapter_id(id))
                    .cloned();
                if let Some(bp) = self
                    .session
                    .breakpoints
                    .apply_event(&b.breakpoint)
                    .and_then(|id| self.session.breakpoints.get(id))
                    .cloned()
                    && before.as_ref() != Some(&bp)
                {
                    self.emit(DebugEvent::BreakpointChanged(bp));
                }
            }
            DapEvent::Initialized => {}
            DapEvent::Other(e) => tracing::trace!("ignoring event {}", e.event),
        }
        Ok(())
    }

    pub(super) async fn on_stopped(&mut self, s: dap::StoppedEvent) -> Result<()> {
        self.pause_pending = false;
        let hit_bps: Vec<Breakpoint> = s
            .hit_breakpoint_ids
            .iter()
            .filter_map(|id| self.session.breakpoints.by_adapter_id(*id))
            .cloned()
            .collect();
        if s.reason == "breakpoint"
            && !hit_bps.is_empty()
            && hit_bps.iter().all(|b| b.scope().is_some())
            && let Some(tid) = s.thread_id
            && self.skip_out_of_scope(tid, &hit_bps).await?
        {
            return Ok(());
        }
        let elapsed = self.resumed_at.take().map(|t| t.elapsed());
        let hit: Vec<BreakpointId> = hit_bps.iter().map(|b| b.id).collect();
        let reason = StopReason::from_dap(&s.reason, s.text.clone(), hit);
        self.session.on_resume();
        self.session.status = SessionStatus::Stopped(reason.clone());

        // threads → stackTrace(selected) → top frame → scopes
        self.refresh_threads().await?;
        // The stopping thread is authoritative: lldb-dap's `threads` reply
        // can lag behind newly spawned threads (e.g. libtest's test thread).
        if let Some(t) = s.thread_id.map(ThreadId)
            && !self.session.threads.iter().any(|th| th.id == t)
        {
            self.session.threads.push(Thread {
                id: t,
                name: format!("Thread {t}"),
            });
        }
        let exists = |t: ThreadId| self.session.threads.iter().any(|th| th.id == t);
        let tid = s
            .thread_id
            .map(ThreadId)
            .or(self.session.selected_thread.filter(|t| exists(*t)))
            .or(self.session.threads.first().map(|t| t.id));
        self.session.selected_thread = tid;
        if let Some(tid) = tid {
            self.load_stack(tid).await?;
        }
        if s.reason == "breakpoint" {
            let at = self.session.stack.first().and_then(|f| {
                f.path
                    .as_ref()
                    .map(|p| crate::breakpoint::SourceLocation::new(p.clone(), f.line))
            });
            let hit: Vec<BreakpointId> = hit_bps.iter().map(|b| b.id).collect();
            for id in self.session.breakpoints.mark_hit(&hit, at.as_ref()) {
                if let Some(bp) = self.session.breakpoints.get(id).cloned() {
                    self.emit(DebugEvent::BreakpointChanged(bp));
                }
            }
        }
        let exception = match (&reason, tid) {
            (StopReason::Exception(_), Some(tid)) => self.exception_info(tid).await,
            _ => None,
        };

        self.evaluate_watches().await;
        let mut description = s.description.or(s.text);
        if std::mem::take(&mut self.continuing) && reason == StopReason::Step {
            // e.g. lldb keeps a `finish` that was interrupted by a breakpoint
            // on another thread pending, and completes it on the next resume.
            description = Some(format!(
                "{}: an unfinished step from an earlier `next`/`step`/`finish` completed",
                description.as_deref().unwrap_or("step"),
            ));
        }
        self.emit(DebugEvent::SessionStopped(Box::new(StopInfo {
            reason,
            thread: tid,
            description,
            frame: self.session.stack.first().cloned(),
            exception,
            elapsed,
            watches: self.session.watches.snapshot(),
        })));
        Ok(())
    }

    /// Best effort: a failing `exceptionInfo` must not hide the stop.
    pub(super) async fn exception_info(&self, tid: ThreadId) -> Option<ExceptionInfo> {
        if !self.session.supports(Feature::ExceptionInfo) {
            return None;
        }
        let client = self.client().ok()?;
        match client
            .request(ExceptionInfoArguments { thread_id: tid.0 })
            .await
        {
            Ok(resp) => Some(resp.into()),
            Err(e) => {
                tracing::debug!("exceptionInfo failed: {e}");
                None
            }
        }
    }

    /// Scoped function breakpoints: DAP matches by name only, so a stop in
    /// a same-named function of another file is resumed transparently.
    /// Returns `true` if execution was resumed.
    pub(super) async fn skip_out_of_scope(&mut self, tid: i64, hit: &[Breakpoint]) -> Result<bool> {
        let resp = self
            .client()?
            .request(StackTraceArguments {
                thread_id: tid,
                start_frame: None,
                levels: Some(1),
            })
            .await?;
        let path = resp
            .stack_frames
            .first()
            .and_then(|f| f.source.as_ref())
            .and_then(|s| s.path.as_deref())
            .map(Path::new);
        if hit.iter().any(|b| b.in_scope(path)) {
            return Ok(false);
        }
        tracing::debug!("skipping stop outside breakpoint file scope: {path:?}");
        self.client()?
            .request(ContinueArguments { thread_id: tid })
            .await?;
        self.session.on_resume();
        self.session.status = SessionStatus::Running;
        Ok(true)
    }
}
