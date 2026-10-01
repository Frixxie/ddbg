//! Request/response correlation and message dispatch.

use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::codec::{Decoder, encode};
use crate::protocol::{DapEvent, DapRequest, Message, Request, Response};
use anyhow::{Context as _, Result, anyhow};

/// Something received asynchronously from the adapter.
#[derive(Debug, Clone)]
pub enum Incoming {
    Event(DapEvent),
    /// Reverse request (adapter → client), e.g. `runInTerminal`.
    /// Must be answered with [`DapClient::respond`].
    Request(Request),
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Response>>>>;

/// Handle to a DAP connection. Cheap to clone.
#[derive(Clone)]
pub struct DapClient {
    seq: Arc<AtomicI64>,
    out_tx: mpsc::UnboundedSender<Message>,
    pending: Pending,
}

impl DapClient {
    /// Start reader/writer tasks over the given streams.
    ///
    /// Returns the client and the stream of incoming events/reverse
    /// requests. The receiver yields `None` once the adapter disconnects.
    pub fn connect<R, W>(reader: R, writer: W) -> (Self, mpsc::UnboundedReceiver<Incoming>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();

        tokio::spawn(write_loop(writer, out_rx));
        tokio::spawn(read_loop(reader, pending.clone(), in_tx));

        let client = Self {
            seq: Arc::new(AtomicI64::new(1)),
            out_tx,
            pending,
        };
        (client, in_rx)
    }

    fn next_seq(&self) -> i64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a request and wait for its response.
    pub async fn request<R: DapRequest>(&self, request: R) -> Result<R::Response> {
        self.send(request)?.await
    }

    /// Send a request without waiting. The returned future resolves to the
    /// response. The request is written even if the future is never polled.
    pub fn send<R: DapRequest>(&self, request: R) -> Result<ResponseFuture<R::Response>> {
        let arguments = serde_json::to_value(&request)
            .with_context(|| format!("failed to serialize {} arguments", R::COMMAND))?;
        let raw = self.send_raw(R::COMMAND, Some(arguments))?;
        Ok(ResponseFuture {
            command: R::COMMAND,
            raw,
            _marker: PhantomData,
        })
    }

    /// Send an untyped request.
    pub fn send_raw(&self, command: &str, arguments: Option<Value>) -> Result<RawResponseFuture> {
        let seq = self.next_seq();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(seq, tx);
        let msg = Message::Request(Request {
            seq,
            command: command.into(),
            arguments,
        });
        if self.out_tx.send(msg).is_err() {
            self.pending.lock().unwrap().remove(&seq);
            return Err(channel_closed());
        }
        Ok(RawResponseFuture { rx })
    }

    /// Answer a reverse request from the adapter.
    pub fn respond(
        &self,
        request: &Request,
        success: bool,
        message: Option<String>,
        body: Option<Value>,
    ) -> Result<()> {
        let msg = Message::Response(Response {
            seq: self.next_seq(),
            request_seq: request.seq,
            success,
            command: request.command.clone(),
            message,
            body,
        });
        self.out_tx.send(msg).map_err(|_| channel_closed())
    }
}

/// Message used when the adapter connection is gone.
pub const CHANNEL_CLOSED: &str = "debug adapter connection closed";

pub fn channel_closed() -> anyhow::Error {
    anyhow!(CHANNEL_CLOSED)
}

/// Future resolving to a raw [`Response`].
pub struct RawResponseFuture {
    rx: oneshot::Receiver<Response>,
}

impl Future for RawResponseFuture {
    type Output = Result<Response>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.rx)
            .poll(cx)
            .map(|r| r.map_err(|_| channel_closed()))
    }
}

/// Future resolving to a typed response body.
pub struct ResponseFuture<T> {
    command: &'static str,
    raw: RawResponseFuture,
    _marker: PhantomData<fn() -> T>,
}

impl<T: serde::de::DeserializeOwned> Future for ResponseFuture<T> {
    type Output = Result<T>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let command = self.command;
        Pin::new(&mut self.raw)
            .poll(cx)
            .map(|r| r.and_then(|resp| parse_response(command, resp)))
    }
}

fn parse_response<T: serde::de::DeserializeOwned>(command: &str, resp: Response) -> Result<T> {
    if !resp.success {
        let message = resp
            .body
            .as_ref()
            .and_then(|b| b.pointer("/error/format"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(resp.message)
            .unwrap_or_else(|| "unknown error".into());
        return Err(anyhow!("{command} failed: {message}"));
    }
    let body = resp
        .body
        .unwrap_or_else(|| Value::Object(Default::default()));
    serde_json::from_value(body).with_context(|| format!("unexpected response body for {command}"))
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut rx: mpsc::UnboundedReceiver<Message>,
) {
    while let Some(msg) = rx.recv().await {
        let bytes = match encode(&msg) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(target: "dap", "encode failed: {e}");
                continue;
            }
        };
        if tracing::enabled!(target: "dap", tracing::Level::TRACE) {
            tracing::trace!(target: "dap", "--> {}", serde_json::to_string(&msg).unwrap_or_default());
        }
        if let Err(e) = async {
            writer.write_all(&bytes).await?;
            writer.flush().await
        }
        .await
        {
            tracing::debug!(target: "dap", "write failed: {e}");
            break;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    pending: Pending,
    in_tx: mpsc::UnboundedSender<Incoming>,
) {
    let mut decoder = Decoder::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::debug!(target: "dap", "read failed: {e}");
                break;
            }
        };
        decoder.extend(&buf[..n]);
        loop {
            match decoder.decode() {
                Ok(Some(msg)) => dispatch(msg, &pending, &in_tx),
                Ok(None) => break,
                Err(e) => tracing::warn!(target: "dap", "discarding message: {e}"),
            }
        }
    }
    // Fail every outstanding request.
    pending.lock().unwrap().clear();
}

fn dispatch(msg: Message, pending: &Pending, in_tx: &mpsc::UnboundedSender<Incoming>) {
    if tracing::enabled!(target: "dap", tracing::Level::TRACE) {
        tracing::trace!(target: "dap", "<-- {}", serde_json::to_string(&msg).unwrap_or_default());
    }
    match msg {
        Message::Response(resp) => {
            let tx = pending.lock().unwrap().remove(&resp.request_seq);
            match tx {
                Some(tx) => {
                    let _ = tx.send(resp);
                }
                None => {
                    tracing::warn!(target: "dap", "unmatched response for seq {}", resp.request_seq)
                }
            }
        }
        Message::Event(event) => {
            let _ = in_tx.send(Incoming::Event(event.into()));
        }
        Message::Request(req) => {
            let _ = in_tx.send(Incoming::Request(req));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Decoder;
    use crate::protocol::{Capabilities, InitializeArguments, ThreadsArguments, ThreadsResponse};
    use serde_json::json;
    use tokio::io::{DuplexStream, duplex};

    /// A scripted fake adapter on the other end of an in-memory pipe.
    struct FakeAdapter {
        io: DuplexStream,
        decoder: Decoder,
        seq: i64,
    }

    impl FakeAdapter {
        async fn recv(&mut self) -> Request {
            let mut buf = [0u8; 4096];
            loop {
                if let Some(Message::Request(r)) = self.decoder.decode().unwrap() {
                    return r;
                }
                let n = self.io.read(&mut buf).await.unwrap();
                assert!(n > 0, "client closed");
                self.decoder.extend(&buf[..n]);
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

        async fn event(&mut self, name: &str, body: Value) {
            self.seq += 1;
            let msg = Message::Event(crate::protocol::Event {
                seq: self.seq,
                event: name.into(),
                body: Some(body),
            });
            self.send(msg).await;
        }
    }

    fn setup() -> (DapClient, mpsc::UnboundedReceiver<Incoming>, FakeAdapter) {
        let (a, b) = duplex(64 * 1024);
        let (r, w) = tokio::io::split(a);
        let (client, rx) = DapClient::connect(r, w);
        let fake = FakeAdapter {
            io: b,
            decoder: Decoder::new(),
            seq: 1000,
        };
        (client, rx, fake)
    }

    #[tokio::test]
    async fn correlates_out_of_order_responses() {
        let (client, mut rx, mut fake) = setup();

        let f1 = client.send(InitializeArguments::new("test")).unwrap();
        let f2 = client.send(ThreadsArguments {}).unwrap();

        let r1 = fake.recv().await;
        let r2 = fake.recv().await;
        assert_eq!(r1.command, "initialize");
        assert_eq!(r2.command, "threads");

        fake.respond(&r2, json!({"threads": [{"id": 1, "name": "main"}]}))
            .await;
        fake.event("output", json!({"output": "hi"})).await;
        fake.respond(&r1, json!({"supportsConfigurationDoneRequest": true}))
            .await;

        let threads: ThreadsResponse = f2.await.unwrap();
        let caps: Capabilities = f1.await.unwrap();
        assert_eq!(threads.threads[0].name, "main");
        assert!(caps.supports_configuration_done_request);

        let Some(Incoming::Event(DapEvent::Output(o))) = rx.recv().await else {
            panic!("expected output event")
        };
        assert_eq!(o.output, "hi");
    }

    #[tokio::test]
    async fn adapter_error_is_reported() {
        let (client, _rx, mut fake) = setup();
        let f = client.send(ThreadsArguments {}).unwrap();
        let req = fake.recv().await;
        fake.seq += 1;
        fake.send(Message::Response(Response {
            seq: fake.seq,
            request_seq: req.seq,
            success: false,
            command: req.command,
            message: Some("nope".into()),
            body: None,
        }))
        .await;
        let err = f.await.unwrap_err();
        assert_eq!(err.to_string(), "threads failed: nope");
    }

    #[tokio::test]
    async fn disconnect_fails_pending_requests() {
        let (client, mut rx, mut fake) = setup();
        let f = client.send(ThreadsArguments {}).unwrap();
        let _ = fake.recv().await;
        drop(fake);
        assert_eq!(f.await.unwrap_err().to_string(), CHANNEL_CLOSED);
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn reverse_requests_are_surfaced() {
        let (client, mut rx, mut fake) = setup();
        fake.send(Message::Request(Request {
            seq: 5,
            command: "runInTerminal".into(),
            arguments: Some(json!({"args": ["x"]})),
        }))
        .await;
        let Some(Incoming::Request(req)) = rx.recv().await else {
            panic!()
        };
        client
            .respond(&req, false, Some("unsupported".into()), None)
            .unwrap();
        let mut buf = [0u8; 1024];
        let n = fake.io.read(&mut buf).await.unwrap();
        fake.decoder.extend(&buf[..n]);
        let Some(Message::Response(r)) = fake.decoder.decode().unwrap() else {
            panic!()
        };
        assert_eq!(r.request_seq, 5);
        assert!(!r.success);
    }
}
