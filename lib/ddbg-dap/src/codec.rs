//! `Content-Length` framed message codec.
//!
//! DAP messages are UTF-8 JSON bodies preceded by HTTP-like headers:
//!
//! ```text
//! Content-Length: 119\r\n
//! \r\n
//! {"seq":1,...}
//! ```

use bytes::{Buf, BytesMut};

use crate::protocol::Message;
use anyhow::{Context, Result, anyhow, bail};

const HEADER_TERMINATOR: &[u8] = b"\r\n\r\n";
const MAX_HEADER_LEN: usize = 8 * 1024;

/// Incremental decoder. Feed bytes with [`Decoder::extend`] and pull
/// complete messages with [`Decoder::decode`].
#[derive(Debug, Default)]
pub struct Decoder {
    buf: BytesMut,
    /// Body length of the frame currently being read, once its header is parsed.
    pending_len: Option<usize>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extend(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Decode the next complete message, if one is buffered.
    ///
    /// Errors consume the offending frame, so decoding can continue afterwards.
    pub fn decode(&mut self) -> Result<Option<Message>> {
        let len = match self.pending_len {
            Some(len) => len,
            None => {
                let Some(end) = find(&self.buf, HEADER_TERMINATOR) else {
                    if self.buf.len() > MAX_HEADER_LEN {
                        self.buf.clear();
                        bail!("DAP header too long");
                    }
                    return Ok(None);
                };
                let header = self.buf.split_to(end + HEADER_TERMINATOR.len());
                let len = parse_header(&header[..end])?;
                self.pending_len = Some(len);
                len
            }
        };

        if self.buf.len() < len {
            return Ok(None);
        }
        self.pending_len = None;
        let body = self.buf.split_to(len);
        serde_json::from_slice(&body)
            .map(Some)
            .context("invalid DAP JSON")
    }

    /// Bytes buffered but not yet decoded.
    pub fn buffered(&self) -> usize {
        self.buf.remaining()
    }
}

fn parse_header(header: &[u8]) -> Result<usize> {
    let text = std::str::from_utf8(header).map_err(|_| anyhow!("invalid DAP header"))?;
    let mut len = None;
    for line in text.split("\r\n") {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid DAP header"))?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            len = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| anyhow!("invalid DAP header"))?,
            );
        }
    }
    len.ok_or_else(|| anyhow!("DAP header missing Content-Length"))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Encode a message into a framed byte buffer.
pub fn encode(message: &Message) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(message).context("failed to serialize DAP message")?;
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Event, Message};

    fn frame(body: &str) -> Vec<u8> {
        let mut v = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        v.extend_from_slice(body.as_bytes());
        v
    }

    const EVT: &str = r#"{"seq":1,"type":"event","event":"initialized"}"#;

    #[test]
    fn decodes_single_message() {
        let mut d = Decoder::new();
        d.extend(&frame(EVT));
        let msg = d.decode().unwrap().unwrap();
        assert!(matches!(msg, Message::Event(Event { ref event, .. }) if event == "initialized"));
        assert!(d.decode().unwrap().is_none());
    }

    #[test]
    fn handles_fragmented_reads() {
        let bytes = frame(EVT);
        let mut d = Decoder::new();
        let mut got = 0;
        for b in &bytes {
            d.extend(std::slice::from_ref(b));
            while d.decode().unwrap().is_some() {
                got += 1;
            }
        }
        assert_eq!(got, 1);
    }

    #[test]
    fn handles_multiple_messages_in_one_buffer() {
        let mut bytes = frame(EVT);
        bytes.extend(frame(r#"{"seq":2,"type":"event","event":"terminated"}"#));
        let mut d = Decoder::new();
        d.extend(&bytes);
        assert!(d.decode().unwrap().is_some());
        assert!(d.decode().unwrap().is_some());
        assert!(d.decode().unwrap().is_none());
    }

    #[test]
    fn rejects_malformed_header_and_recovers() {
        let mut bytes = b"Content-Length: abc\r\n\r\n".to_vec();
        bytes.extend(frame(EVT));
        let mut d = Decoder::new();
        d.extend(&bytes);
        assert!(matches!(d.decode(), Err(ref e) if e.to_string() == "invalid DAP header"));
        assert!(d.decode().unwrap().is_some());
    }

    #[test]
    fn rejects_missing_content_length() {
        let mut d = Decoder::new();
        d.extend(b"Content-Type: x\r\n\r\n");
        assert!(matches!(d.decode(), Err(ref e) if e.to_string().contains("Content-Length")));
    }

    #[test]
    fn rejects_malformed_json_and_recovers() {
        let mut bytes = frame("{not json}");
        bytes.extend(frame(EVT));
        let mut d = Decoder::new();
        d.extend(&bytes);
        assert!(matches!(d.decode(), Err(ref e) if e.to_string() == "invalid DAP JSON"));
        assert!(d.decode().unwrap().is_some());
    }

    #[test]
    fn content_length_counts_utf8_bytes() {
        let body = r#"{"seq":1,"type":"event","event":"output","body":{"output":"héllo → 🦀"}}"#;
        assert_ne!(body.len(), body.chars().count());
        let mut d = Decoder::new();
        d.extend(&frame(body));
        let Message::Event(e) = d.decode().unwrap().unwrap() else {
            panic!("expected event")
        };
        assert_eq!(e.body.unwrap()["output"], "héllo → 🦀");
    }

    #[test]
    fn encode_roundtrip() {
        let msg: Message = serde_json::from_str(EVT).unwrap();
        let bytes = encode(&msg).unwrap();
        let mut d = Decoder::new();
        d.extend(&bytes);
        assert!(d.decode().unwrap().is_some());
    }

    #[test]
    fn header_too_long() {
        let mut d = Decoder::new();
        d.extend(&vec![b'x'; MAX_HEADER_LEN + 1]);
        assert!(matches!(d.decode(), Err(ref e) if e.to_string().contains("too long")));
    }
}
