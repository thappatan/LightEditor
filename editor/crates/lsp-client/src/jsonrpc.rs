//! JSON-RPC 2.0 message types and Content-Length framing.
//!
//! The Language Server Protocol layers JSON-RPC 2.0 over a stream framed by
//! a single `Content-Length: <bytes>\r\n\r\n` header. Everything in this
//! module is symmetric — clients can read the messages servers send and
//! vice versa.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC request: has an id, a method, and parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A JSON-RPC response: id (matching the request) and either a result or
/// an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A JSON-RPC notification: no id, fire-and-forget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// One framed message. Discriminating by presence of `id` and `method` is
/// the JSON-RPC convention — there is no shared tag.
#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

impl Message {
    /// Classify a freshly-decoded JSON value into one of the three shapes.
    /// Returns `Err` on a value that fits none of them.
    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        let has_id = v.get("id").is_some();
        let has_method = v.get("method").is_some();
        match (has_id, has_method) {
            (true, true) => Ok(Message::Request(serde_json::from_value(v)?)),
            (true, false) => Ok(Message::Response(serde_json::from_value(v)?)),
            (false, true) => Ok(Message::Notification(serde_json::from_value(v)?)),
            (false, false) => Err(serde::de::Error::custom(
                "JSON-RPC message has neither id nor method",
            )),
        }
    }
}

/// Write `msg` framed with a `Content-Length` header. Flushes after.
pub fn write_message<W: Write>(w: &mut W, msg: &Value) -> io::Result<()> {
    let body =
        serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(body.as_bytes())?;
    w.flush()
}

/// Read one framed message. Returns `Ok(None)` on EOF before any data —
/// the server closed its stdout cleanly. Returns `Err` on malformed
/// framing or partial reads.
pub fn read_message<R: BufRead>(r: &mut R) -> io::Result<Option<Message>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    let mut saw_header = false;
    loop {
        line.clear();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            return if saw_header {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF inside JSON-RPC header block",
                ))
            } else {
                Ok(None)
            };
        }
        saw_header = true;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of header block
        }
        // Headers are case-insensitive per LSP spec; lower-case for the
        // common `Content-Length:` field.
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let v: Value =
        serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Message::from_value(v)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trips_a_notification() {
        let original = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": { "textDocument": { "uri": "file:///x.rs" } }
        });
        let mut buf = Vec::new();
        write_message(&mut buf, &original).unwrap();
        let mut cur = Cursor::new(buf);
        let msg = read_message(&mut cur).unwrap().unwrap();
        match msg {
            Message::Notification(n) => {
                assert_eq!(n.method, "textDocument/didOpen");
                assert!(n.params.is_some());
            }
            _ => panic!("expected notification, got {msg:?}"),
        }
    }

    #[test]
    fn round_trips_a_request() {
        let original = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "initialize",
            "params": { "rootUri": null }
        });
        let mut buf = Vec::new();
        write_message(&mut buf, &original).unwrap();
        let mut cur = Cursor::new(buf);
        match read_message(&mut cur).unwrap().unwrap() {
            Message::Request(r) => {
                assert_eq!(r.method, "initialize");
                assert_eq!(r.id, serde_json::json!(42));
            }
            other => panic!("expected request, got {other:?}"),
        }
    }

    #[test]
    fn round_trips_a_response_with_error() {
        let original = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "error": { "code": -32601, "message": "method not found" }
        });
        let mut buf = Vec::new();
        write_message(&mut buf, &original).unwrap();
        let mut cur = Cursor::new(buf);
        match read_message(&mut cur).unwrap().unwrap() {
            Message::Response(r) => {
                assert!(r.result.is_none());
                assert_eq!(r.error.unwrap().code, -32601);
            }
            other => panic!("expected response, got {other:?}"),
        }
    }

    #[test]
    fn eof_before_any_header_is_not_an_error() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        assert!(read_message(&mut cur).unwrap().is_none());
    }

    #[test]
    fn missing_content_length_is_an_error() {
        // Headers but no Content-Length.
        let buf = b"X-Other: foo\r\n\r\n{}".to_vec();
        let mut cur = Cursor::new(buf);
        assert!(read_message(&mut cur).is_err());
    }

    #[test]
    fn frame_uses_byte_length_not_char_length() {
        // "สวัสดี🚀" — many bytes, few chars.
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "test",
            "params": { "greeting": "สวัสดี🚀" }
        });
        let mut buf = Vec::new();
        write_message(&mut buf, &body).unwrap();
        let header = std::str::from_utf8(&buf[..40]).unwrap();
        let len: usize = header
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Content-Length:")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The body must be exactly `len` bytes.
        let body_start = header.find("\r\n\r\n").unwrap() + 4;
        assert_eq!(buf.len() - body_start, len);
    }

    #[test]
    fn rejects_message_with_neither_id_nor_method() {
        let v = serde_json::json!({ "jsonrpc": "2.0", "result": 1 });
        assert!(Message::from_value(v).is_err());
    }

    #[test]
    fn read_uses_a_real_bufread_path() {
        // Multiple messages back-to-back in one stream — common when a
        // server replies immediately after a notification.
        let mut buf = Vec::new();
        write_message(&mut buf, &serde_json::json!({"jsonrpc":"2.0","method":"a"})).unwrap();
        write_message(&mut buf, &serde_json::json!({"jsonrpc":"2.0","method":"b"})).unwrap();
        let mut cur = Cursor::new(buf);
        let a = read_message(&mut cur).unwrap().unwrap();
        let b = read_message(&mut cur).unwrap().unwrap();
        match (a, b) {
            (Message::Notification(a), Message::Notification(b)) => {
                assert_eq!(a.method, "a");
                assert_eq!(b.method, "b");
            }
            other => panic!("expected two notifications, got {other:?}"),
        }
    }
}
