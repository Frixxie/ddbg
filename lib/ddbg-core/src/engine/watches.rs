use super::*;

use super::inspect::evaluate_error;

impl Engine {
    pub(super) fn watch(&self, id: WatchId) -> Result<&crate::watch::Watch> {
        self.session
            .watches
            .get(id)
            .ok_or_else(|| anyhow!("no watch {id}"))
    }

    /// Evaluate only the summary, without expanding children or invoking
    /// ToString(). Each failure belongs to its watch, not to the debugger stop.
    pub(super) async fn evaluate_watch(&mut self, id: WatchId) {
        let Some(expression) = self.session.watches.get(id).map(|w| w.expression.clone()) else {
            return;
        };
        let result = match self.session.current_frame().map(|f| f.id.0) {
            Some(frame_id) => match self.client() {
                Ok(client) => client
                    .request(EvaluateArguments {
                        expression: expression.clone(),
                        frame_id: Some(frame_id),
                        context: Some("watch".into()),
                    })
                    .await
                    .map(|r| WatchValue::from(Evaluation::from(r)))
                    .map_err(|e| evaluate_error(&expression, e).to_string()),
                Err(e) => Err(e.to_string()),
            },
            None => Err("no frame selected".into()),
        };
        self.session.watches.set_result(id, result);
    }

    pub(super) async fn evaluate_watches(&mut self) {
        if !self.session.status.is_stopped() {
            self.session.watches.invalidate();
            return;
        }
        for w in self.session.watches.snapshot() {
            self.evaluate_watch(w.id).await;
        }
    }

    pub(super) async fn refresh_watches(&mut self) {
        self.evaluate_watches().await;
        let watches = self.session.watches.snapshot();
        if !watches.is_empty() {
            self.emit(DebugEvent::WatchesChanged(watches));
        }
    }

    pub(super) async fn cmd_watch(&mut self, expression: String) -> Result<Reply> {
        let expression = expression.trim().to_owned();
        if expression.is_empty() {
            bail!("watch expression must not be empty");
        }
        let (id, new) = self.session.watches.add(expression);
        if new && self.session.status.is_stopped() {
            self.evaluate_watch(id).await;
        }
        Ok(Reply::WatchSet {
            watch: self.watch(id)?.clone(),
            new,
        })
    }

    pub(super) fn cmd_unwatch(&mut self, id: WatchId) -> Result<Reply> {
        self.session
            .watches
            .remove(id)
            .ok_or_else(|| anyhow!("no watch {id}"))?;
        Ok(Reply::WatchDeleted(id))
    }

    pub(super) fn cmd_watches(&self) -> Result<Reply> {
        Ok(Reply::Watches(self.session.watches.snapshot()))
    }
}
