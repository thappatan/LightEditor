//! Subprocess wrapper around one language-server stdio pair.
//!
//! The client spawns the server with three background threads:
//! - a **reader** thread that frames incoming JSON-RPC messages from
//!   stdout onto an `mpsc::Receiver` the host drains at its leisure;
//! - a **writer** thread that consumes outgoing messages from an
//!   `mpsc::Sender` and writes them framed to stdin.
//!
//! Decoupling writes from the host's render thread is what keeps keystroke
//! latency low: a full-document `didChange` on a large buffer can exceed
//! the OS pipe buffer (~64KB on macOS) and block the writer until the
//! server drains it. Without the writer thread, that block runs on the
//! UI thread.

use std::ffi::OsString;
use std::io::{self, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use lsp_types::{
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
        Exit, Initialized, Notification,
    },
    request::{Completion, GotoDefinition, HoverRequest, Initialize, Request, Shutdown},
    ClientCapabilities, CompletionClientCapabilities, CompletionContext, CompletionParams,
    CompletionTriggerKind, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, GotoDefinitionParams,
    HoverClientCapabilities, HoverParams, InitializeParams, InitializedParams, MarkupKind,
    Position, TextDocumentClientCapabilities, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
    TextDocumentSyncClientCapabilities, Url, VersionedTextDocumentIdentifier,
    WorkDoneProgressParams,
};
use serde::Serialize;
use serde_json::Value;

use crate::jsonrpc::{self, Message};

/// One running language-server process plus its stdio plumbing.
///
/// Reads and writes run on dedicated background threads; the host thread
/// only pushes outgoing messages onto a channel (cheap and non-blocking)
/// and drains incoming messages via [`try_recv`](LspClient::try_recv).
/// Dropping the client kills the child — graceful shutdown should go
/// through [`shutdown`](LspClient::shutdown) and
/// [`exit_notification`](LspClient::exit_notification) first.
pub struct LspClient {
    child: Child,
    /// Outgoing-message channel. The writer thread on the other end frames
    /// each value and writes it to the server's stdin.
    outgoing: Sender<Value>,
    incoming: Receiver<Message>,
    /// JoinHandles are kept so the threads are dropped (and joined-best-
    /// effort via the channels' hang-up) on `LspClient::drop`.
    _reader: JoinHandle<()>,
    _writer: JoinHandle<()>,
    /// Monotonic request-id counter. JSON-RPC ids can be strings or numbers;
    /// we use numbers for simplicity.
    next_id: i64,
}

impl LspClient {
    /// Spawn `command` with `args` as a subprocess and start consuming its
    /// stdout. `stderr` is inherited (so server diagnostics show up in the
    /// host process's stderr / logs).
    pub fn spawn<S: Into<OsString>>(command: S, args: &[&str]) -> io::Result<Self> {
        let cmd_name = {
            let s: OsString = command.into();
            s
        };
        log::info!(
            "LSP: spawning {:?} with args {:?}",
            cmd_name.to_string_lossy(),
            args
        );
        let mut child = Command::new(&cmd_name)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        let (in_tx, in_rx) = mpsc::channel::<Message>();
        let (out_tx, out_rx) = mpsc::channel::<Value>();

        let reader = std::thread::Builder::new()
            .name("lsp-client-reader".into())
            .spawn(move || {
                let mut r = BufReader::new(stdout);
                loop {
                    match jsonrpc::read_message(&mut r) {
                        Ok(Some(msg)) => {
                            if in_tx.send(msg).is_err() {
                                break; // main side dropped the receiver
                            }
                        }
                        Ok(None) => break, // server closed stdout
                        Err(e) => {
                            log::warn!("LSP read error: {e}");
                            break;
                        }
                    }
                }
            })?;

        let writer = std::thread::Builder::new()
            .name("lsp-client-writer".into())
            .spawn(move || {
                while let Ok(body) = out_rx.recv() {
                    if let Err(e) = jsonrpc::write_message(&mut stdin, &body) {
                        log::warn!("LSP write error: {e}");
                        break;
                    }
                }
                // Best-effort: flush any pending bytes before the FD is dropped.
                let _ = stdin.flush();
            })?;

        Ok(Self {
            child,
            outgoing: out_tx,
            incoming: in_rx,
            _reader: reader,
            _writer: writer,
            next_id: 0,
        })
    }

    /// Drain one server message without blocking. Returns `None` when the
    /// queue is empty and `Err(Disconnected)` when the reader thread has
    /// exited (server crash / closed stdout).
    pub fn try_recv(&self) -> Result<Option<Message>, TryRecvError> {
        match self.incoming.try_recv() {
            Ok(msg) => Ok(Some(msg)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Send a JSON-RPC request and return its id, so the caller can match
    /// it against the eventual response.
    pub fn send_request<P: Serialize>(&mut self, method: &str, params: P) -> io::Result<i64> {
        self.next_id += 1;
        let id = self.next_id;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write(body)?;
        Ok(id)
    }

    /// Send a JSON-RPC notification — no id, no response expected.
    pub fn send_notification<P: Serialize>(&self, method: &str, params: P) -> io::Result<()> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write(body)
    }

    /// Send a response to a server-initiated request. The server may ask
    /// for e.g. workspace configuration; if we don't reply, it can stall.
    pub fn send_response<R: Serialize>(&self, id: Value, result: R) -> io::Result<()> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        self.write(body)
    }

    fn write(&self, body: Value) -> io::Result<()> {
        // Push to the writer thread's channel — returns immediately even
        // for a multi-megabyte payload, since the channel is unbounded.
        // Error means the writer thread has exited (server crash).
        self.outgoing.send(body).map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "LSP writer thread is gone (server probably crashed)",
            )
        })
    }

    // ── high-level LSP convenience wrappers ───────────────────────────────

    /// Send the `initialize` request with workspace folder `root_uri` and
    /// a minimal capability set (text sync + hover + definition). Returns
    /// the request id so the caller can wait for the response.
    pub fn initialize(&mut self, root_uri: Option<Url>) -> io::Result<i64> {
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            #[allow(deprecated)]
            root_uri,
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    synchronization: Some(TextDocumentSyncClientCapabilities {
                        dynamic_registration: Some(false),
                        will_save: Some(false),
                        will_save_wait_until: Some(false),
                        did_save: Some(true),
                    }),
                    hover: Some(HoverClientCapabilities {
                        dynamic_registration: Some(false),
                        content_format: Some(vec![MarkupKind::PlainText, MarkupKind::Markdown]),
                    }),
                    completion: Some(CompletionClientCapabilities {
                        dynamic_registration: Some(false),
                        // We don't implement snippets in v1; ask servers to
                        // give us plain text so they don't return `${...}`
                        // placeholders we'd insert verbatim.
                        completion_item: None,
                        completion_item_kind: None,
                        context_support: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        self.send_request(Initialize::METHOD, params)
    }

    /// Send the `initialized` notification — the server treats this as the
    /// signal that handshake is done and it's safe to send didOpen.
    pub fn initialized(&self) -> io::Result<()> {
        self.send_notification(Initialized::METHOD, InitializedParams {})
    }

    /// Send `textDocument/didOpen` with the document's full text.
    pub fn did_open(
        &self,
        uri: Url,
        language_id: &str,
        version: i32,
        text: String,
    ) -> io::Result<()> {
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri,
                language_id: language_id.to_string(),
                version,
                text,
            },
        };
        self.send_notification(DidOpenTextDocument::METHOD, params)
    }

    /// Send `textDocument/didChange` with a full-document replacement.
    /// Incremental sync is a follow-up.
    pub fn did_change_full(&self, uri: Url, version: i32, text: String) -> io::Result<()> {
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier { uri, version },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text,
            }],
        };
        self.send_notification(DidChangeTextDocument::METHOD, params)
    }

    pub fn did_save(&self, uri: Url, text: Option<String>) -> io::Result<()> {
        let params = DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text,
        };
        self.send_notification(DidSaveTextDocument::METHOD, params)
    }

    pub fn did_close(&self, uri: Url) -> io::Result<()> {
        let params = DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
        };
        self.send_notification(DidCloseTextDocument::METHOD, params)
    }

    /// Ask for hover info at a position. Returns the request id.
    pub fn request_hover(&mut self, uri: Url, position: Position) -> io::Result<i64> {
        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        };
        self.send_request(HoverRequest::METHOD, params)
    }

    /// Ask for the definition of the symbol at `position`. Returns the
    /// request id.
    pub fn request_definition(&mut self, uri: Url, position: Position) -> io::Result<i64> {
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
        };
        self.send_request(GotoDefinition::METHOD, params)
    }

    /// Ask for completion items at `position`. `trigger` carries the
    /// reason for the request — explicit (`Invoked`) when the user hit
    /// the completion shortcut, or `TriggerCharacter` when a server-
    /// declared trigger char like `.` or `::` was just typed. Returns
    /// the request id.
    pub fn request_completion(
        &mut self,
        uri: Url,
        position: Position,
        trigger: CompletionTriggerKind,
        trigger_character: Option<String>,
    ) -> io::Result<i64> {
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: Default::default(),
            context: Some(CompletionContext {
                trigger_kind: trigger,
                trigger_character,
            }),
        };
        self.send_request(Completion::METHOD, params)
    }

    /// Send the `shutdown` request. Returns the request id; the caller
    /// should wait for the response before sending [`exit_notification`].
    pub fn shutdown(&mut self) -> io::Result<i64> {
        self.send_request(Shutdown::METHOD, Value::Null)
    }

    pub fn exit_notification(&self) -> io::Result<()> {
        self.send_notification(Exit::METHOD, Value::Null)
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Best-effort kill — graceful shutdown happens via shutdown/exit
        // before drop. Ignoring errors because the child may already be
        // gone (clean exit) or unreachable (host crashing).
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Convert a filesystem path to a `file://` URL. Returns `None` if the
/// path isn't absolute or can't be UTF-8 encoded.
pub fn path_to_uri(path: &Path) -> Option<Url> {
    Url::from_file_path(path).ok()
}
