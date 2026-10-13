//! Child debug sessions presented as a single adapter connection.
//!
//! Some adapters (notably js-debug) start one session per debuggee process
//! by sending a `startDebugging` reverse request. Each child session is a
//! separate DAP connection with its own thread, frame and variable IDs.
//! [`spawn`] answers those requests, configures every child with the
//! breakpoints the client has set, and relays everything over one
//! connection, translating IDs so they are unique across sessions.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

use crate::client::{DapClient, Incoming, RawIncoming};
use crate::codec::{Decoder, encode};
use crate::protocol::{Event, Message, Request, Response};
use anyhow::{Result, anyhow, bail};

/// One raw adapter connection.
pub type RawConnection = (DapClient, mpsc::UnboundedReceiver<RawIncoming>);
pub type ConnectFuture = Pin<Box<dyn Future<Output = Result<RawConnection>> + Send>>;
/// Opens a new connection to the adapter, for the root and each child.
pub type Connect = Arc<dyn Fn() -> ConnectFuture + Send + Sync>;

const ROOT: usize = 0;
const CHILD_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Start a multiplexer. `setup` runs in the background (e.g. starting an
/// adapter server); if it fails, every request is answered with its error.
pub fn spawn<S>(setup: S) -> (DapClient, mpsc::UnboundedReceiver<Incoming>)
where
    S: Future<Output = Result<Connect>> + Send + 'static,
{
    let (front, back) = tokio::io::duplex(1 << 20);
    let (r, w) = tokio::io::split(front);
    let (client, incoming) = DapClient::connect(r, w);
    tokio::spawn(run(back, setup));
    (client, incoming)
}

async fn run<S>(io: DuplexStream, setup: S)
where
    S: Future<Output = Result<Connect>> + Send + 'static,
{
    let (r, w) = tokio::io::split(io);
    let mut requests = read_requests(r);
    let mut front = Front { writer: w, seq: 0 };
    let root = match setup.await {
        Ok(connect) => connect().await.map(|root| (connect, root)),
        Err(e) => Err(e),
    };
    let (connect, (client, rx)) = match root {
        Ok(ok) => ok,
        Err(e) => {
            tracing::warn!(target: "dap", "debug adapter failed to start: {e:#}");
            let message = format!("{e:#}");
            while let Some(req) = requests.recv().await {
                front
                    .respond(&req, false, Some(message.clone()), None)
                    .await;
                if req.command == "disconnect" {
                    break;
                }
            }
            return;
        }
    };
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (children_tx, mut children) = mpsc::unbounded_channel();
    let (responses_tx, mut responses) = mpsc::unbounded_channel();
    let mut mux = Mux {
        front,
        connect,
        sessions: Vec::new(),
        threads: IdMap::default(),
        frames: IdMap::default(),
        vars: IdMap::default(),
        bp_ids: IdMap::default(),
        breakpoints: HashMap::new(),
        config: Vec::new(),
        init_args: json!({}),
        last_stopped: None,
        events_tx,
        children_tx,
        responses_tx,
    };
    mux.add_session(client, rx);
    loop {
        tokio::select! {
            req = requests.recv() => {
                let Some(req) = req else { break };
                let done = req.command == "disconnect";
                mux.request(req).await;
                if done {
                    break;
                }
            }
            Some((s, msg)) = events.recv() => {
                if !mux.session_message(s, msg).await {
                    break;
                }
            }
            Some(child) = children.recv() => mux.add_child(child).await,
            Some((req, resp)) = responses.recv() => mux.reply(&req, resp).await,
        }
    }
}

/// Engine-side requests from the front end of the duplex.
fn read_requests(mut reader: ReadHalf<DuplexStream>) -> mpsc::UnboundedReceiver<Request> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut decoder = Decoder::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => decoder.extend(&buf[..n]),
            }
            loop {
                match decoder.decode() {
                    Ok(Some(Message::Request(req))) => {
                        if tx.send(req).is_err() {
                            return;
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(e) => tracing::warn!(target: "dap", "discarding message: {e}"),
                }
            }
        }
    });
    rx
}

struct Front {
    writer: WriteHalf<DuplexStream>,
    seq: i64,
}

impl Front {
    async fn send(&mut self, msg: Message) {
        let Ok(bytes) = encode(&msg) else { return };
        let _ = self.writer.write_all(&bytes).await;
    }

    async fn respond(
        &mut self,
        req: &Request,
        success: bool,
        message: Option<String>,
        body: Option<Value>,
    ) {
        self.seq += 1;
        let msg = Message::Response(Response {
            seq: self.seq,
            request_seq: req.seq,
            success,
            command: req.command.clone(),
            message,
            body,
        });
        self.send(msg).await;
    }

    async fn event(&mut self, event: &str, body: Option<Value>) {
        self.seq += 1;
        let msg = Message::Event(Event {
            seq: self.seq,
            event: event.into(),
            body,
        });
        self.send(msg).await;
    }
}

/// Translates `(session, local id)` pairs to IDs unique across sessions.
#[derive(Default)]
struct IdMap {
    next: i64,
    forward: HashMap<(usize, i64), i64>,
    back: HashMap<i64, (usize, i64)>,
}

impl IdMap {
    fn fresh(&mut self) -> i64 {
        self.next += 1;
        self.next
    }

    fn global(&mut self, session: usize, local: i64) -> i64 {
        if let Some(g) = self.forward.get(&(session, local)) {
            return *g;
        }
        let g = self.fresh();
        self.forward.insert((session, local), g);
        self.back.insert(g, (session, local));
        g
    }

    /// Make `(session, local)` translate to an existing global ID.
    fn alias(&mut self, session: usize, local: i64, global: i64) {
        self.forward.insert((session, local), global);
    }

    fn known(&self, session: usize, local: i64) -> Option<i64> {
        self.forward.get(&(session, local)).copied()
    }

    fn forget(&mut self, session: usize, local: i64) {
        if let Some(g) = self.forward.remove(&(session, local)) {
            self.back.remove(&g);
        }
    }

    fn local(&self, global: i64) -> Option<(usize, i64)> {
        self.back.get(&global).copied()
    }

    fn of_session(&self, session: usize) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .forward
            .iter()
            .filter(|((s, _), _)| *s == session)
            .map(|(_, g)| *g)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// Breakpoints of one `setBreakpoints` source or the function breakpoints,
/// by position in the client's request.
struct BreakpointSet {
    ids: Vec<i64>,
    verified: Vec<bool>,
}

struct Session {
    client: DapClient,
}

struct ChildReady {
    client: DapClient,
    rx: mpsc::UnboundedReceiver<RawIncoming>,
    /// Messages received before `initialized`.
    buffered: Vec<RawIncoming>,
    launch: Option<crate::client::RawResponseFuture>,
}

struct Mux {
    front: Front,
    connect: Connect,
    sessions: Vec<Option<Session>>,
    threads: IdMap,
    frames: IdMap,
    vars: IdMap,
    bp_ids: IdMap,
    breakpoints: HashMap<String, BreakpointSet>,
    /// Configuration requests replayed to new children: (key, command, args).
    config: Vec<(String, String, Value)>,
    init_args: Value,
    last_stopped: Option<usize>,
    events_tx: mpsc::UnboundedSender<(usize, Option<RawIncoming>)>,
    children_tx: mpsc::UnboundedSender<Result<ChildReady>>,
    /// Responses to requests forwarded without waiting.
    responses_tx: mpsc::UnboundedSender<(Request, Result<Response>)>,
}

fn breakpoint_key(command: &str, args: &Value) -> Option<String> {
    match command {
        "setBreakpoints" => Some(format!(
            "source:{}",
            args.pointer("/source/path")
                .and_then(Value::as_str)
                .unwrap_or_default()
        )),
        "setFunctionBreakpoints" => Some("functions".into()),
        _ => None,
    }
}

fn remap(v: &mut Value, key: &str, map: &mut IdMap, session: usize) {
    if let Some(id) = v.get(key).and_then(Value::as_i64) {
        v[key] = json!(map.global(session, id));
    }
}

/// Like [`remap`], but `0` (no reference) is kept.
fn remap_ref(v: &mut Value, key: &str, map: &mut IdMap, session: usize) {
    if v.get(key).and_then(Value::as_i64).is_some_and(|id| id != 0) {
        remap(v, key, map, session);
    }
}

fn remap_all(
    body: &mut Value,
    list: &str,
    key: &str,
    map: &mut IdMap,
    session: usize,
    f: fn(&mut Value, &str, &mut IdMap, usize),
) {
    if let Some(items) = body.get_mut(list).and_then(Value::as_array_mut) {
        for item in items {
            f(item, key, map, session);
        }
    }
}

impl Mux {
    fn add_session(
        &mut self,
        client: DapClient,
        mut rx: mpsc::UnboundedReceiver<RawIncoming>,
    ) -> usize {
        let index = self.sessions.len();
        self.sessions.push(Some(Session { client }));
        let tx = self.events_tx.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if tx.send((index, Some(msg))).is_err() {
                    return;
                }
            }
            let _ = tx.send((index, None));
        });
        index
    }

    fn live(&self) -> Vec<usize> {
        (0..self.sessions.len())
            .filter(|i| self.sessions[*i].is_some())
            .collect()
    }

    fn client(&self, s: usize) -> Result<DapClient> {
        self.sessions
            .get(s)
            .and_then(Option::as_ref)
            .map(|s| s.client.clone())
            .ok_or_else(|| anyhow!("debug session has ended"))
    }

    async fn forward(&self, s: usize, command: &str, args: Option<Value>) -> Result<Response> {
        self.client(s)?.send_raw(command, args)?.await
    }

    /// Session for requests without a thread, frame or variable reference.
    fn default_session(&self) -> usize {
        let alive = |s: &usize| self.sessions.get(*s).is_some_and(Option::is_some);
        self.last_stopped
            .filter(alive)
            .or_else(|| self.live().into_iter().rfind(|s| *s != ROOT))
            .unwrap_or(ROOT)
    }

    async fn request(&mut self, req: Request) {
        // Adapters may answer launch/attach only after configurationDone,
        // so other requests must not wait for them.
        if matches!(req.command.as_str(), "launch" | "attach") {
            let sent = self
                .client(ROOT)
                .and_then(|c| c.send_raw(&req.command, req.arguments.clone()));
            let tx = self.responses_tx.clone();
            tokio::spawn(async move {
                let resp = match sent {
                    Ok(f) => f.await,
                    Err(e) => Err(e),
                };
                let _ = tx.send((req, resp));
            });
            return;
        }
        let resp = self.route(&req).await;
        self.reply(&req, resp).await;
    }

    async fn reply(&mut self, req: &Request, resp: Result<Response>) {
        match resp {
            Ok(resp) => {
                self.front
                    .respond(req, resp.success, resp.message, resp.body)
                    .await
            }
            Err(e) => {
                self.front
                    .respond(req, false, Some(format!("{e:#}")), None)
                    .await
            }
        }
    }

    async fn route(&mut self, req: &Request) -> Result<Response> {
        let command = req.command.as_str();
        let mut args = req.arguments.clone();
        match command {
            "initialize" => {
                let mut a = args.unwrap_or_else(|| json!({}));
                a["supportsStartDebuggingRequest"] = json!(true);
                self.init_args = a.clone();
                return self.forward(ROOT, command, Some(a)).await;
            }
            "setBreakpoints" | "setFunctionBreakpoints" | "setExceptionBreakpoints" => {
                return self
                    .configure(command, args.unwrap_or_else(|| json!({})))
                    .await;
            }
            "threads" => return self.threads().await,
            "disconnect" | "terminate" => {
                let children: Vec<_> = self
                    .live()
                    .into_iter()
                    .filter(|s| *s != ROOT)
                    .filter_map(|s| self.client(s).ok()?.send_raw(command, args.clone()).ok())
                    .collect();
                let _ = tokio::time::timeout(CHILD_DISCONNECT_TIMEOUT, async {
                    for c in children {
                        let _ = c.await;
                    }
                })
                .await;
                return self.forward(ROOT, command, args).await;
            }
            _ => {}
        }
        let mut session = None;
        if let Some(a) = args.as_mut() {
            for (key, map) in [
                ("threadId", &self.threads),
                ("frameId", &self.frames),
                ("variablesReference", &self.vars),
            ] {
                if let Some(id) = a.get(key).and_then(Value::as_i64) {
                    let (s, local) = map.local(id).ok_or_else(|| anyhow!("unknown {key} {id}"))?;
                    a[key] = json!(local);
                    session = Some(s);
                    break;
                }
            }
        }
        let session = session.unwrap_or_else(|| match command {
            "configurationDone" => ROOT,
            _ => self.default_session(),
        });
        let mut resp = self.forward(session, command, args).await?;
        if let Some(body) = resp.body.as_mut() {
            self.rewrite_body(session, command, body);
        }
        Ok(resp)
    }

    fn rewrite_body(&mut self, s: usize, command: &str, body: &mut Value) {
        match command {
            "stackTrace" => remap_all(body, "stackFrames", "id", &mut self.frames, s, remap),
            "scopes" => remap_all(
                body,
                "scopes",
                "variablesReference",
                &mut self.vars,
                s,
                remap_ref,
            ),
            "variables" => remap_all(
                body,
                "variables",
                "variablesReference",
                &mut self.vars,
                s,
                remap_ref,
            ),
            "evaluate" | "setVariable" | "setExpression" => {
                remap_ref(body, "variablesReference", &mut self.vars, s)
            }
            _ => {}
        }
    }

    async fn threads(&mut self) -> Result<Response> {
        let mut threads = Vec::new();
        let mut first = None;
        for s in self.live() {
            let Ok(resp) = self.forward(s, "threads", None).await else {
                continue;
            };
            let list = resp
                .body
                .as_ref()
                .and_then(|b| b.get("threads"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for mut t in list {
                remap(&mut t, "id", &mut self.threads, s);
                threads.push(t);
            }
            first.get_or_insert(resp);
        }
        let mut resp = first.ok_or_else(|| anyhow!("debug session has ended"))?;
        resp.success = true;
        resp.body = Some(json!({ "threads": threads }));
        Ok(resp)
    }

    /// Breakpoint configuration goes to every session. Breakpoints get IDs
    /// shared across sessions, by position; a breakpoint is reported verified
    /// if any session verified it.
    async fn configure(&mut self, command: &str, args: Value) -> Result<Response> {
        let key = breakpoint_key(command, &args).unwrap_or_else(|| command.to_owned());
        match self.config.iter_mut().find(|(k, _, _)| *k == key) {
            Some(entry) => entry.2 = args.clone(),
            None => self
                .config
                .push((key.clone(), command.to_owned(), args.clone())),
        }
        let mut responses = Vec::new();
        for s in self.live() {
            match self.forward(s, command, Some(args.clone())).await {
                Ok(r) => responses.push((s, r)),
                Err(e) if s == ROOT => return Err(e),
                Err(e) => tracing::debug!(target: "dap", "{command} in session {s}: {e}"),
            }
        }
        if command == "setExceptionBreakpoints" {
            return responses
                .into_iter()
                .next()
                .map(|(_, r)| r)
                .ok_or_else(|| anyhow!("debug session has ended"));
        }
        let count = args
            .get("breakpoints")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let ids: Vec<i64> = (0..count).map(|_| self.bp_ids.fresh()).collect();
        let mut merged = vec![Value::Null; count];
        let mut verified = vec![false; count];
        let mut result = None;
        for (s, resp) in responses {
            if resp.success {
                let list = resp
                    .body
                    .as_ref()
                    .and_then(|b| b.get("breakpoints"))
                    .and_then(Value::as_array);
                for (i, bp) in list.into_iter().flatten().take(count).enumerate() {
                    if let Some(local) = bp.get("id").and_then(Value::as_i64) {
                        self.bp_ids.alias(s, local, ids[i]);
                    }
                    let v = bp.get("verified").and_then(Value::as_bool) == Some(true);
                    if merged[i].is_null() || (v && !verified[i]) {
                        merged[i] = bp.clone();
                        verified[i] = v;
                    }
                }
            }
            if result.is_none() || resp.success {
                result.get_or_insert(resp);
            }
        }
        let mut resp = result.ok_or_else(|| anyhow!("debug session has ended"))?;
        if resp.success {
            for (bp, id) in merged.iter_mut().zip(&ids) {
                if bp.is_null() {
                    *bp = json!({ "verified": false });
                }
                bp["id"] = json!(id);
            }
            resp.body = Some(json!({ "breakpoints": merged }));
            self.breakpoints
                .insert(key, BreakpointSet { ids, verified });
        }
        Ok(resp)
    }

    /// Handle a message from session `s`. Returns `false` once the root
    /// session is gone.
    async fn session_message(&mut self, s: usize, msg: Option<RawIncoming>) -> bool {
        match msg {
            None => {
                self.end_session(s).await;
                s != ROOT
            }
            Some(RawIncoming::Request(req)) => {
                self.reverse_request(s, req);
                true
            }
            Some(RawIncoming::Event(e)) => {
                self.session_event(s, e).await;
                true
            }
        }
    }

    fn reverse_request(&mut self, s: usize, req: Request) {
        let Ok(client) = self.client(s) else { return };
        if req.command != "startDebugging" {
            let _ = client.respond(
                &req,
                false,
                Some(format!("{} is not supported by ddbg", req.command)),
                None,
            );
            return;
        }
        let args = req.arguments.clone().unwrap_or_default();
        let request = args
            .get("request")
            .and_then(Value::as_str)
            .unwrap_or("launch")
            .to_owned();
        let configuration = args.get("configuration").cloned().unwrap_or_default();
        let _ = client.respond(&req, true, None, None);
        let connect = self.connect.clone();
        let init = self.init_args.clone();
        let tx = self.children_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(start_child(connect, init, request, configuration).await);
        });
    }

    async fn session_event(&mut self, s: usize, mut e: Event) {
        let body = e.body.get_or_insert_with(|| json!({}));
        match e.event.as_str() {
            "loadedSource" => return,
            "initialized" | "exited" | "terminated" if s != ROOT => {
                if e.event == "terminated" {
                    self.end_session(s).await;
                }
                return;
            }
            "stopped" => {
                remap(body, "threadId", &mut self.threads, s);
                if let Some(ids) = body
                    .get_mut("hitBreakpointIds")
                    .and_then(Value::as_array_mut)
                {
                    *ids = ids
                        .iter()
                        .filter_map(Value::as_i64)
                        .filter_map(|id| self.bp_ids.known(s, id))
                        .map(Value::from)
                        .collect();
                }
                self.last_stopped = Some(s);
            }
            "continued" => remap(body, "threadId", &mut self.threads, s),
            "thread" => {
                let local = body.get("threadId").and_then(Value::as_i64);
                remap(body, "threadId", &mut self.threads, s);
                if let Some(local) = local
                    && body.get("reason").and_then(Value::as_str) == Some("exited")
                {
                    self.threads.forget(s, local);
                }
            }
            "breakpoint" => {
                let bp = &mut body["breakpoint"];
                let local = bp.get("id").and_then(Value::as_i64);
                let Some(global) = local.and_then(|id| self.bp_ids.known(s, id)) else {
                    return;
                };
                if bp.get("verified").and_then(Value::as_bool) != Some(true) {
                    return;
                }
                bp["id"] = json!(global);
                self.mark_verified(global);
            }
            _ => {}
        }
        self.front.event(&e.event, e.body).await;
    }

    fn mark_verified(&mut self, global: i64) -> bool {
        for set in self.breakpoints.values_mut() {
            if let Some(i) = set.ids.iter().position(|id| *id == global) {
                let changed = !set.verified[i];
                set.verified[i] = true;
                return changed;
            }
        }
        false
    }

    async fn end_session(&mut self, s: usize) {
        let Some(session) = self.sessions.get_mut(s).and_then(Option::take) else {
            return;
        };
        if s == ROOT {
            return;
        }
        let _ = session.client.send_raw("disconnect", None);
        for thread in self.threads.of_session(s) {
            self.front
                .event(
                    "thread",
                    Some(json!({ "reason": "exited", "threadId": thread })),
                )
                .await;
        }
        if self.last_stopped == Some(s) {
            self.last_stopped = None;
        }
    }

    async fn add_child(&mut self, child: Result<ChildReady>) {
        let child = match child {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(target: "dap", "child debug session failed: {e:#}");
                let output = format!("ddbg: could not start child debug session: {e:#}\n");
                self.front
                    .event(
                        "output",
                        Some(json!({ "category": "console", "output": output })),
                    )
                    .await;
                return;
            }
        };
        let ChildReady {
            client,
            rx,
            buffered,
            launch,
        } = child;
        // The relayed stream is handled after this returns, so buffered
        // messages keep their order.
        let s = self.add_session(client.clone(), rx);
        for (key, command, args) in self.config.clone() {
            let resp = match client.send_raw(&command, Some(args)) {
                Ok(f) => f.await,
                Err(e) => Err(e),
            };
            let Ok(resp) = resp else { continue };
            let list = resp
                .body
                .as_ref()
                .and_then(|b| b.get("breakpoints"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let Some(ids) = self.breakpoints.get(&key).map(|set| set.ids.clone()) else {
                continue;
            };
            for (mut bp, global) in list.into_iter().zip(ids) {
                if let Some(local) = bp.get("id").and_then(Value::as_i64) {
                    self.bp_ids.alias(s, local, global);
                }
                if bp.get("verified").and_then(Value::as_bool) == Some(true)
                    && self.mark_verified(global)
                {
                    bp["id"] = json!(global);
                    self.front
                        .event(
                            "breakpoint",
                            Some(json!({ "reason": "changed", "breakpoint": bp })),
                        )
                        .await;
                }
            }
        }
        match client.send_raw("configurationDone", Some(json!({}))) {
            Ok(f) => {
                tokio::spawn(async move {
                    if let Err(e) = f.await {
                        tracing::debug!(target: "dap", "child configurationDone: {e}");
                    }
                });
            }
            Err(e) => tracing::debug!(target: "dap", "child configurationDone: {e}"),
        }
        if let Some(launch) = launch {
            tokio::spawn(async move {
                match launch.await {
                    Ok(r) if !r.success => {
                        tracing::warn!(target: "dap", "child launch failed: {:?}", r.message)
                    }
                    Err(e) => tracing::debug!(target: "dap", "child launch: {e}"),
                    _ => {}
                }
            });
        }
        for msg in buffered {
            self.session_message(s, Some(msg)).await;
        }
    }
}

/// Connect and initialize a child session, up to its `initialized` event.
async fn start_child(
    connect: Connect,
    init: Value,
    request: String,
    configuration: Value,
) -> Result<ChildReady> {
    let (client, mut rx) = connect().await?;
    let resp = client.send_raw("initialize", Some(init))?.await?;
    if !resp.success {
        bail!("initialize failed: {}", resp.message.unwrap_or_default());
    }
    let mut launch = Some(client.send_raw(&request, Some(configuration))?);
    let mut buffered = Vec::new();
    loop {
        tokio::select! {
            r = async { launch.as_mut().expect("pending launch").await }, if launch.is_some() => {
                launch = None;
                let r = r?;
                if !r.success {
                    bail!("{request} failed: {}", r.message.unwrap_or_default());
                }
            }
            msg = rx.recv() => match msg {
                Some(RawIncoming::Event(e)) if e.event == "initialized" => break,
                Some(m) => buffered.push(m),
                None => bail!("child session closed during startup"),
            }
        }
    }
    Ok(ChildReady {
        client,
        rx,
        buffered,
        launch,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use tokio::io::duplex;

    /// A scripted adapter session on the other end of an in-memory pipe.
    struct Fake {
        io: DuplexStream,
        decoder: Decoder,
        seq: i64,
    }

    impl Fake {
        async fn recv(&mut self) -> Message {
            let mut buf = [0u8; 4096];
            loop {
                if let Some(m) = self.decoder.decode().unwrap() {
                    return m;
                }
                let n = self.io.read(&mut buf).await.unwrap();
                assert!(n > 0, "mux closed the connection");
                self.decoder.extend(&buf[..n]);
            }
        }

        async fn request(&mut self, command: &str) -> Request {
            match self.recv().await {
                Message::Request(r) if r.command == command => r,
                other => panic!("expected {command} request, got {other:?}"),
            }
        }

        async fn send(&mut self, msg: Message) {
            self.io.write_all(&encode(&msg).unwrap()).await.unwrap();
        }

        async fn respond(&mut self, req: &Request, body: Value) {
            self.seq += 1;
            let msg = Message::Response(Response {
                seq: self.seq,
                request_seq: req.seq,
                success: true,
                command: req.command.clone(),
                message: None,
                body: Some(body),
            });
            self.send(msg).await;
        }

        async fn event(&mut self, event: &str, body: Value) {
            self.seq += 1;
            let msg = Message::Event(Event {
                seq: self.seq,
                event: event.into(),
                body: Some(body),
            });
            self.send(msg).await;
        }
    }

    fn pair() -> (RawConnection, Fake) {
        let (a, b) = duplex(1 << 16);
        let (r, w) = tokio::io::split(a);
        let fake = Fake {
            io: b,
            decoder: Decoder::new(),
            seq: 100,
        };
        (DapClient::connect_raw(r, w), fake)
    }

    async fn next_event(rx: &mut mpsc::UnboundedReceiver<Incoming>) -> crate::protocol::DapEvent {
        match rx.recv().await {
            Some(Incoming::Event(e)) => e,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn child_sessions_share_breakpoints_and_get_unique_ids() {
        let (root_conn, mut root) = pair();
        let (child_conn, mut child) = pair();
        let queue = Arc::new(Mutex::new(VecDeque::from([root_conn, child_conn])));
        let connect: Connect = Arc::new(move || {
            let conn = queue.lock().unwrap().pop_front();
            Box::pin(async move { conn.ok_or_else(|| anyhow!("no more connections")) })
        });
        let (client, mut incoming) = spawn(async move { Ok(connect) });

        let init = client.send_raw("initialize", Some(json!({}))).unwrap();
        let req = root.request("initialize").await;
        assert_eq!(
            req.arguments.as_ref().unwrap()["supportsStartDebuggingRequest"],
            true
        );
        root.respond(&req, json!({})).await;
        assert!(init.await.unwrap().success);

        // launch is answered late; other requests must not wait for it.
        let launch = client.send_raw("launch", Some(json!({}))).unwrap();
        let launch_req = root.request("launch").await;
        let bps = json!({"source": {"path": "/a.ts"}, "breakpoints": [{"line": 3}]});
        let set = client.send_raw("setBreakpoints", Some(bps)).unwrap();
        let req = root.request("setBreakpoints").await;
        root.respond(&req, json!({"breakpoints": [{"id": 7, "verified": false}]}))
            .await;
        let set = set.await.unwrap().body.unwrap();
        let bp_id = set["breakpoints"][0]["id"].as_i64().unwrap();
        assert_eq!(set["breakpoints"][0]["verified"], false);
        root.respond(&launch_req, json!({})).await;
        assert!(launch.await.unwrap().success);

        // A child session is started and configured.
        root.seq += 1;
        let start = Message::Request(Request {
            seq: root.seq,
            command: "startDebugging".into(),
            arguments: Some(json!({
                "request": "launch",
                "configuration": {"name": "child", "__pendingTargetId": "x"},
            })),
        });
        root.send(start).await;
        let Message::Response(r) = root.recv().await else {
            panic!("expected startDebugging response")
        };
        assert!(r.success);
        let req = child.request("initialize").await;
        child.respond(&req, json!({})).await;
        let child_launch = child.request("launch").await;
        assert_eq!(
            child_launch.arguments.as_ref().unwrap()["__pendingTargetId"],
            "x"
        );
        child.event("initialized", json!({})).await;
        let req = child.request("setBreakpoints").await;
        assert_eq!(req.arguments.as_ref().unwrap()["source"]["path"], "/a.ts");
        child
            .respond(
                &req,
                json!({"breakpoints": [{"id": 1, "verified": true, "line": 3}]}),
            )
            .await;
        let crate::protocol::DapEvent::Breakpoint(b) = next_event(&mut incoming).await else {
            panic!("expected breakpoint event")
        };
        assert_eq!(
            (b.breakpoint.id, b.breakpoint.verified),
            (Some(bp_id), true)
        );
        let req = child.request("configurationDone").await;
        child.respond(&req, json!({})).await;
        child.respond(&child_launch, json!({})).await;

        // Thread 0 of the child gets a global ID; hit breakpoints are shared.
        child
            .event(
                "stopped",
                json!({"reason": "breakpoint", "threadId": 0, "hitBreakpointIds": [1]}),
            )
            .await;
        let crate::protocol::DapEvent::Stopped(s) = next_event(&mut incoming).await else {
            panic!("expected stopped event")
        };
        assert_eq!(s.hit_breakpoint_ids, [bp_id]);
        let thread = s.thread_id.unwrap();

        let threads = client.send_raw("threads", None).unwrap();
        let req = root.request("threads").await;
        root.respond(&req, json!({"threads": []})).await;
        let req = child.request("threads").await;
        child
            .respond(&req, json!({"threads": [{"id": 0, "name": "main"}]}))
            .await;
        let threads = threads.await.unwrap().body.unwrap();
        assert_eq!(threads["threads"], json!([{"id": thread, "name": "main"}]));

        let trace = client
            .send_raw("stackTrace", Some(json!({"threadId": thread})))
            .unwrap();
        let req = child.request("stackTrace").await;
        assert_eq!(req.arguments.as_ref().unwrap()["threadId"], 0);
        child
            .respond(
                &req,
                json!({"stackFrames": [{"id": 0, "name": "f", "line": 3, "column": 1}]}),
            )
            .await;
        let frame = trace.await.unwrap().body.unwrap()["stackFrames"][0]["id"].clone();

        let scopes = client
            .send_raw("scopes", Some(json!({"frameId": frame})))
            .unwrap();
        let req = child.request("scopes").await;
        assert_eq!(req.arguments.as_ref().unwrap()["frameId"], 0);
        child
            .respond(
                &req,
                json!({"scopes": [{"name": "Local", "variablesReference": 5}]}),
            )
            .await;
        let vars = scopes.await.unwrap().body.unwrap()["scopes"][0]["variablesReference"].clone();
        let variables = client
            .send_raw("variables", Some(json!({"variablesReference": vars})))
            .unwrap();
        let req = child.request("variables").await;
        assert_eq!(req.arguments.as_ref().unwrap()["variablesReference"], 5);
        child.respond(&req, json!({"variables": []})).await;
        assert!(variables.await.unwrap().success);

        // A child ending is not the end of the session.
        child.event("terminated", json!({})).await;
        let crate::protocol::DapEvent::Thread(t) = next_event(&mut incoming).await else {
            panic!("expected thread event")
        };
        assert_eq!((t.reason.as_str(), t.thread_id), ("exited", thread));
        root.event("terminated", json!({})).await;
        assert!(matches!(
            next_event(&mut incoming).await,
            crate::protocol::DapEvent::Terminated
        ));
    }

    #[tokio::test]
    async fn setup_failure_is_reported_to_requests() {
        let (client, _incoming) = spawn(async { Err(anyhow!("js-debug was not found")) });
        let resp = client.send_raw("initialize", None).unwrap().await.unwrap();
        assert!(!resp.success);
        assert_eq!(resp.message.as_deref(), Some("js-debug was not found"));
    }
}
