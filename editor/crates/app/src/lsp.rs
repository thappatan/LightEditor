//! Per-app Language Server Protocol state (spec §3.5, §4.2).
//!
//! Owns one [`editor_lsp_client::LspClient`] per `ServerKind` (Rust,
//! TypeScript-family). Each client is spawned lazily — the first time a
//! document of a matching language opens — handshake runs in the
//! background, and didOpen/didChange/didClose flow through this state
//! machine.
//!
//! Incoming server messages land here too: [`drain`](LspState::drain)
//! pulls every queued notification/response, updates the diagnostics map
//! and pending-request table, and returns any handler hits the app should
//! act on (jump to a definition, show a hover popup).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use editor_lsp_client::lsp_types::{
    self, notification::Notification as _, Diagnostic, GotoDefinitionResponse, Hover, Location,
    PublishDiagnosticsParams, Url,
};
use editor_lsp_client::{path_to_uri, LspClient, Message};
use editor_syntax::Language;

/// Which language server backs a given language. TS, TSX, and JavaScript
/// share `typescript-language-server`; Rust uses rust-analyzer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerKind {
    Rust,
    TypeScript,
}

impl ServerKind {
    /// The server backing `lang`, if any. `None` for languages we haven't
    /// wired (Python, Go, Dart, …) — they fall back to tree-sitter only.
    pub fn for_language(lang: Language) -> Option<Self> {
        match lang {
            Language::Rust => Some(ServerKind::Rust),
            Language::TypeScript | Language::Tsx | Language::JavaScript => {
                Some(ServerKind::TypeScript)
            }
            _ => None,
        }
    }

    /// The executable + argv that spawns this server.
    fn command(self) -> (&'static str, Vec<&'static str>) {
        match self {
            ServerKind::Rust => ("rust-analyzer", vec![]),
            ServerKind::TypeScript => ("typescript-language-server", vec!["--stdio"]),
        }
    }
}

/// Which LSP `languageId` to use for a given editor language. Servers care
/// about this — e.g. `typescript-language-server` distinguishes `typescript`
/// from `typescriptreact`.
fn language_id(lang: Language) -> &'static str {
    match lang {
        Language::Rust => "rust",
        Language::TypeScript => "typescript",
        Language::Tsx => "typescriptreact",
        Language::JavaScript => "javascript",
        // The rest never hit the LSP layer (server_kind returns None) so the
        // exact label doesn't matter — kept for future expansion.
        Language::Python => "python",
        Language::Go => "go",
        Language::C => "c",
        Language::Dart => "dart",
        Language::Json => "json",
        Language::Markdown => "markdown",
        Language::Toml => "toml",
        Language::Yaml => "yaml",
        Language::Bash => "shellscript",
        Language::Lua => "lua",
        Language::Ruby => "ruby",
    }
}

/// One server, plus the bookkeeping needed to drive its lifecycle.
struct Slot {
    client: LspClient,
    /// `true` after we've received the response to `initialize` and sent
    /// `initialized`. Until then, document notifications get queued.
    initialized: bool,
    /// The id we used for our `initialize` request — matched against the
    /// incoming response.
    init_id: i64,
    /// Open document URIs the server has been told about. Used to gate
    /// didChange (only after didOpen) and didClose.
    open_uris: HashSet<Url>,
    /// didOpens received before the server finished initializing — flushed
    /// once it does.
    pending_opens: Vec<PendingOpen>,
}

struct PendingOpen {
    uri: Url,
    lang_id: &'static str,
    version: i32,
    text: String,
}

/// One outstanding request the app cares about — the id maps back to what
/// to do when the response arrives.
#[derive(Debug, Clone)]
pub enum PendingRequest {
    Hover {
        doc_path: PathBuf,
    },
    Definition {
        /// The path of the doc the request originated from, so we can decide
        /// whether the jump is in-file or cross-file.
        doc_path: PathBuf,
    },
}

/// One actionable result the app should react to after [`drain`].
#[derive(Debug)]
pub enum LspEvent {
    /// Server published new diagnostics for `path`. The field is logged by
    /// the receiver; the diagnostics themselves are already stored on the
    /// [`LspState`] (look them up with
    /// [`diagnostics_for`](LspState::diagnostics_for)).
    DiagnosticsUpdated {
        #[allow(dead_code)]
        path: PathBuf,
    },
    /// A hover response arrived for the matching request.
    Hover {
        doc_path: PathBuf,
        result: Option<Hover>,
    },
    /// A definition response arrived for the matching request.
    Definition {
        doc_path: PathBuf,
        locations: Vec<Location>,
    },
    /// The server's stderr / our reader thread died — the slot is gone.
    ServerExited { kind: ServerKind },
}

pub struct LspState {
    slots: HashMap<ServerKind, Slot>,
    pending: HashMap<i64, PendingRequest>,
    diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
    /// Initial workspace root URI, sent with every initialize. `None` when
    /// the app launched without a file (or with an unsaved path).
    workspace_root: Option<Url>,
}

impl LspState {
    pub fn new(workspace_root: Option<Url>) -> Self {
        Self {
            slots: HashMap::new(),
            pending: HashMap::new(),
            diagnostics: HashMap::new(),
            workspace_root,
        }
    }

    /// Diagnostics keyed by canonical document path. Empty when none are
    /// known.
    pub fn diagnostics_for(&self, path: &Path) -> Option<&[Diagnostic]> {
        self.diagnostics.get(path).map(|v| v.as_slice())
    }

    /// Total diagnostics across all open files, broken down by severity.
    pub fn diagnostic_counts(&self) -> DiagnosticCounts {
        let mut c = DiagnosticCounts::default();
        for diags in self.diagnostics.values() {
            for d in diags {
                match d.severity {
                    Some(lsp_types::DiagnosticSeverity::ERROR) => c.errors += 1,
                    Some(lsp_types::DiagnosticSeverity::WARNING) => c.warnings += 1,
                    Some(lsp_types::DiagnosticSeverity::INFORMATION) => c.info += 1,
                    Some(lsp_types::DiagnosticSeverity::HINT) => c.hints += 1,
                    _ => c.info += 1, // unspecified — treat as info
                }
            }
        }
        c
    }

    /// Spawn the server for `lang` if it isn't running yet. Returns `Ok`
    /// even when spawning fails (e.g. binary not on PATH) — in that case
    /// the server is silently absent and downstream operations no-op.
    pub fn ensure_server(&mut self, lang: Language) -> Option<ServerKind> {
        let kind = ServerKind::for_language(lang)?;
        if self.slots.contains_key(&kind) {
            return Some(kind);
        }
        let (cmd, args) = kind.command();
        let mut client = match LspClient::spawn(cmd, &args) {
            Ok(c) => c,
            Err(e) => {
                log::info!("LSP: could not spawn {cmd}: {e} — feature disabled for {kind:?}");
                return None;
            }
        };
        let init_id = match client.initialize(self.workspace_root.clone()) {
            Ok(id) => id,
            Err(e) => {
                log::warn!("LSP: initialize write failed for {kind:?}: {e}");
                return None;
            }
        };
        self.slots.insert(
            kind,
            Slot {
                client,
                initialized: false,
                init_id,
                open_uris: HashSet::new(),
                pending_opens: Vec::new(),
            },
        );
        Some(kind)
    }

    /// Notify the right server that `path` is now open. No-op when no
    /// server is wired for `lang`, when spawning failed, or when `path`
    /// isn't an absolute file path (untitled scratch docs).
    pub fn did_open(&mut self, path: &Path, lang: Language, version: i32, text: String) {
        let Some(uri) = path_to_uri(path) else {
            return;
        };
        let Some(kind) = self.ensure_server(lang) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(&kind) else {
            return;
        };
        let lang_id = language_id(lang);
        if !slot.initialized {
            slot.pending_opens.push(PendingOpen {
                uri,
                lang_id,
                version,
                text,
            });
            return;
        }
        if slot.open_uris.insert(uri.clone()) {
            if let Err(e) = slot.client.did_open(uri, lang_id, version, text) {
                log::warn!("LSP: didOpen failed for {kind:?}: {e}");
            }
        }
    }

    pub fn did_change(&mut self, path: &Path, lang: Language, version: i32, text: String) {
        let Some(uri) = path_to_uri(path) else { return };
        let Some(kind) = ServerKind::for_language(lang) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(&kind) else {
            return;
        };
        if !slot.open_uris.contains(&uri) {
            // didOpen first: catches the case where init was still
            // pending when the doc opened.
            slot.open_uris.insert(uri.clone());
            if let Err(e) =
                slot.client
                    .did_open(uri.clone(), language_id(lang), version, text.clone())
            {
                log::warn!("LSP: didOpen (deferred) failed for {kind:?}: {e}");
                return;
            }
        }
        if let Err(e) = slot.client.did_change_full(uri, version, text) {
            log::warn!("LSP: didChange failed for {kind:?}: {e}");
        }
    }

    pub fn did_save(&mut self, path: &Path, lang: Language, text: Option<String>) {
        let Some(uri) = path_to_uri(path) else { return };
        let Some(kind) = ServerKind::for_language(lang) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(&kind) else {
            return;
        };
        if slot.open_uris.contains(&uri) {
            if let Err(e) = slot.client.did_save(uri, text) {
                log::warn!("LSP: didSave failed for {kind:?}: {e}");
            }
        }
    }

    pub fn did_close(&mut self, path: &Path, lang: Language) {
        let Some(uri) = path_to_uri(path) else { return };
        let Some(kind) = ServerKind::for_language(lang) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(&kind) else {
            return;
        };
        if slot.open_uris.remove(&uri) {
            if let Err(e) = slot.client.did_close(uri) {
                log::warn!("LSP: didClose failed for {kind:?}: {e}");
            }
        }
        // Drop the diagnostics tied to this path — they'd be stale.
        self.diagnostics.remove(path);
    }

    /// Send a `textDocument/hover` request. Returns the request id or
    /// `None` when no server is available.
    pub fn request_hover(
        &mut self,
        path: &Path,
        lang: Language,
        position: lsp_types::Position,
    ) -> Option<i64> {
        let uri = path_to_uri(path)?;
        let kind = ServerKind::for_language(lang)?;
        let slot = self.slots.get_mut(&kind)?;
        if !slot.initialized || !slot.open_uris.contains(&uri) {
            return None;
        }
        match slot.client.request_hover(uri, position) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    PendingRequest::Hover {
                        doc_path: path.to_path_buf(),
                    },
                );
                Some(id)
            }
            Err(e) => {
                log::warn!("LSP: hover request failed: {e}");
                None
            }
        }
    }

    /// Send a `textDocument/definition` request. Returns the request id or
    /// `None` when no server is available.
    pub fn request_definition(
        &mut self,
        path: &Path,
        lang: Language,
        position: lsp_types::Position,
    ) -> Option<i64> {
        let uri = path_to_uri(path)?;
        let kind = ServerKind::for_language(lang)?;
        let slot = self.slots.get_mut(&kind)?;
        if !slot.initialized || !slot.open_uris.contains(&uri) {
            return None;
        }
        match slot.client.request_definition(uri, position) {
            Ok(id) => {
                self.pending.insert(
                    id,
                    PendingRequest::Definition {
                        doc_path: path.to_path_buf(),
                    },
                );
                Some(id)
            }
            Err(e) => {
                log::warn!("LSP: definition request failed: {e}");
                None
            }
        }
    }

    /// Drain every server's incoming queue. Returns the list of events the
    /// app should react to (diagnostics updates, response arrivals,
    /// server exits).
    pub fn drain(&mut self) -> Vec<LspEvent> {
        let mut events = Vec::new();
        let mut crashed: Vec<ServerKind> = Vec::new();
        for (&kind, slot) in self.slots.iter_mut() {
            loop {
                match slot.client.try_recv() {
                    Ok(Some(msg)) => Self::handle_message(
                        kind,
                        slot,
                        msg,
                        &mut self.pending,
                        &mut self.diagnostics,
                        &mut events,
                    ),
                    Ok(None) => break,
                    Err(_) => {
                        crashed.push(kind);
                        break;
                    }
                }
            }
        }
        for kind in crashed {
            self.slots.remove(&kind);
            events.push(LspEvent::ServerExited { kind });
        }
        events
    }

    fn handle_message(
        kind: ServerKind,
        slot: &mut Slot,
        msg: Message,
        pending: &mut HashMap<i64, PendingRequest>,
        diagnostics: &mut HashMap<PathBuf, Vec<Diagnostic>>,
        events: &mut Vec<LspEvent>,
    ) {
        match msg {
            Message::Notification(n) => match n.method.as_str() {
                m if m == lsp_types::notification::PublishDiagnostics::METHOD => {
                    if let Some(params) = n.params {
                        if let Ok(p) = serde_json::from_value::<PublishDiagnosticsParams>(params) {
                            if let Ok(path) = p.uri.to_file_path() {
                                diagnostics.insert(path.clone(), p.diagnostics);
                                events.push(LspEvent::DiagnosticsUpdated { path });
                            }
                        }
                    }
                }
                _ => {
                    log::debug!("LSP {kind:?} notification: {}", n.method);
                }
            },
            Message::Response(r) => {
                let Some(id) = r.id.as_i64() else { return };
                if id == slot.init_id {
                    slot.initialized = true;
                    let _ = slot.client.initialized();
                    // Flush any queued didOpens.
                    for op in std::mem::take(&mut slot.pending_opens) {
                        if slot.open_uris.insert(op.uri.clone()) {
                            let _ = slot
                                .client
                                .did_open(op.uri, op.lang_id, op.version, op.text);
                        }
                    }
                    return;
                }
                let Some(action) = pending.remove(&id) else {
                    return;
                };
                match action {
                    PendingRequest::Hover { doc_path } => {
                        let result = r.result.and_then(|v| serde_json::from_value(v).ok());
                        events.push(LspEvent::Hover { doc_path, result });
                    }
                    PendingRequest::Definition { doc_path } => {
                        let locations = r
                            .result
                            .and_then(|v| serde_json::from_value::<GotoDefinitionResponse>(v).ok())
                            .map(|r| match r {
                                GotoDefinitionResponse::Scalar(l) => vec![l],
                                GotoDefinitionResponse::Array(v) => v,
                                GotoDefinitionResponse::Link(v) => v
                                    .into_iter()
                                    .map(|l| Location {
                                        uri: l.target_uri,
                                        range: l.target_range,
                                    })
                                    .collect(),
                            })
                            .unwrap_or_default();
                        events.push(LspEvent::Definition {
                            doc_path,
                            locations,
                        });
                    }
                }
            }
            Message::Request(req) => {
                // Servers can send us requests too (workspace/configuration,
                // window/workDoneProgress/create, etc.). Reply with a null
                // result so the server doesn't stall waiting on us. The
                // null is correct for "we don't have a specific value" —
                // tells the server to fall back to its defaults.
                let _ = slot.client.send_response(req.id, serde_json::Value::Null);
            }
        }
    }

    /// Best-effort shutdown for graceful exit. Sends shutdown + exit to
    /// every server; drop() of LspClient also kills the process so even
    /// when this is skipped (panic etc.) we don't leak children.
    #[allow(dead_code)]
    pub fn shutdown_all(&mut self) {
        for slot in self.slots.values_mut() {
            let _ = slot.client.shutdown();
            let _ = slot.client.exit_notification();
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DiagnosticCounts {
    pub errors: u32,
    pub warnings: u32,
    pub info: u32,
    pub hints: u32,
}

impl DiagnosticCounts {
    pub fn total(self) -> u32 {
        self.errors + self.warnings + self.info + self.hints
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_kind_routes_typescript_family_together() {
        assert_eq!(
            ServerKind::for_language(Language::TypeScript),
            Some(ServerKind::TypeScript)
        );
        assert_eq!(
            ServerKind::for_language(Language::Tsx),
            Some(ServerKind::TypeScript)
        );
        assert_eq!(
            ServerKind::for_language(Language::JavaScript),
            Some(ServerKind::TypeScript)
        );
        assert_eq!(
            ServerKind::for_language(Language::Rust),
            Some(ServerKind::Rust)
        );
        // No server wired yet for these — falls through to None.
        assert_eq!(ServerKind::for_language(Language::Python), None);
        assert_eq!(ServerKind::for_language(Language::Markdown), None);
    }

    #[test]
    fn diagnostic_counts_sum_by_severity() {
        let mut state = LspState::new(None);
        let path = PathBuf::from("/tmp/x.rs");
        state.diagnostics.insert(
            path,
            vec![
                Diagnostic {
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    ..Default::default()
                },
                Diagnostic {
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    ..Default::default()
                },
                Diagnostic {
                    severity: Some(lsp_types::DiagnosticSeverity::WARNING),
                    ..Default::default()
                },
            ],
        );
        let c = state.diagnostic_counts();
        assert_eq!(c.errors, 2);
        assert_eq!(c.warnings, 1);
        assert_eq!(c.total(), 3);
    }

    #[test]
    fn missing_server_silently_skips() {
        // A pristine state with no spawned servers shouldn't panic when we
        // try to send didChange — it just no-ops.
        let mut state = LspState::new(None);
        state.did_change(
            Path::new("/tmp/x.rs"),
            Language::Rust,
            1,
            "fn main() {}".into(),
        );
        // No server slot was created (since ensure_server only runs in didOpen).
        assert!(state.slots.is_empty());
    }
}
