use super::*;

impl Engine {
    pub(super) fn breakpoint(&self, id: BreakpointId) -> Result<&Breakpoint> {
        self.session
            .breakpoints
            .get(id)
            .ok_or_else(|| anyhow!("no breakpoint {id}"))
    }

    pub(super) fn can_set_breakpoints(&self) -> bool {
        self.conn.is_some()
            && matches!(
                self.session.status,
                SessionStatus::Configuring | SessionStatus::Running | SessionStatus::Stopped(_)
            )
    }

    pub(super) fn check_condition(&self, condition: Option<&str>) -> Result<()> {
        if let Some(condition) = condition {
            if condition.trim().is_empty() {
                bail!("breakpoint condition must not be empty; omit it to clear the condition");
            }
            if self.conn.is_some() && !self.session.supports(Feature::ConditionalBreakpoints) {
                bail!(NO_CONDITIONAL_BREAKPOINTS);
            }
        }
        Ok(())
    }

    pub(super) async fn cmd_break(
        &mut self,
        location: Location,
        condition: Option<String>,
    ) -> Result<Reply> {
        self.check_condition(condition.as_deref())?;
        if matches!(location, Location::Function(_))
            && self.conn.is_some()
            && !self.session.supports(Feature::FunctionBreakpoints)
        {
            bail!(NO_FUNCTION_BREAKPOINTS);
        }
        let before = self.session.breakpoints.clone();
        let (id, new) = self.session.breakpoints.add(location, &self.cwd);
        if !new && let Some(condition) = &condition {
            let bp = self.breakpoint(id)?;
            if bp.condition.as_ref() != Some(condition) {
                bail!(
                    "breakpoint {id} already exists; use `condition {id} <expression>` to change it"
                );
            }
        }
        if new {
            self.session.breakpoints.set_condition(id, condition);
            if self.can_set_breakpoints() {
                let bp = self.breakpoint(id)?.clone();
                if let Err(e) = self.sync_for(&bp).await {
                    self.session.breakpoints = before;
                    return Err(e);
                }
            }
        }
        let breakpoint = self.breakpoint(id)?.clone();
        Ok(Reply::BreakpointSet { breakpoint, new })
    }

    pub(super) async fn cmd_condition(
        &mut self,
        id: BreakpointId,
        condition: Option<String>,
    ) -> Result<Reply> {
        let bp = self
            .session
            .breakpoints
            .get(id)
            .ok_or_else(|| anyhow!("no breakpoint {id}"))?
            .clone();
        self.check_condition(condition.as_deref())?;
        if bp.condition != condition {
            let before = self.session.breakpoints.clone();
            self.session.breakpoints.set_condition(id, condition);
            if self.can_set_breakpoints()
                && let Err(e) = self.sync_for(&bp).await
            {
                self.session.breakpoints = before;
                return Err(e);
            }
        }
        let breakpoint = self.breakpoint(id)?.clone();
        Ok(Reply::BreakpointSet {
            breakpoint,
            new: false,
        })
    }

    pub(super) async fn sync_breakpoints(&mut self, path: &Path) -> Result<()> {
        let args = self.session.breakpoints.request_for(path);
        let resp = self.client()?.request(args).await?;
        self.session
            .breakpoints
            .apply_results(path, &resp.breakpoints);
        Ok(())
    }

    /// Re-send whichever breakpoint set `bp` belongs to.
    pub(super) async fn sync_for(&mut self, bp: &Breakpoint) -> Result<()> {
        match &bp.requested {
            Location::Source(s) => self.sync_breakpoints(&s.path).await,
            Location::Function(_) => self.sync_function_breakpoints().await,
        }
    }

    pub(super) async fn sync_function_breakpoints(&mut self) -> Result<()> {
        if !self.session.supports(Feature::FunctionBreakpoints) {
            self.session
                .breakpoints
                .reject_functions(NO_FUNCTION_BREAKPOINTS);
            return Ok(());
        }
        let args = self.session.breakpoints.function_request();
        let resp = self.client()?.request(args).await?;
        self.session
            .breakpoints
            .apply_function_results(&resp.breakpoints);
        Ok(())
    }

    pub(super) async fn cmd_delete_breakpoint(&mut self, id: BreakpointId) -> Result<Reply> {
        let bp = self
            .session
            .breakpoints
            .remove(id)
            .ok_or_else(|| anyhow!("no breakpoint {id}"))?;
        if self.can_set_breakpoints() {
            self.sync_for(&bp).await?;
        }
        Ok(Reply::BreakpointDeleted(id))
    }

    pub(super) fn cmd_breakpoints(&self) -> Result<Reply> {
        Ok(Reply::Breakpoints(
            self.session.breakpoints.iter().cloned().collect(),
        ))
    }
}
