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
use std::time::{Duration, Instant};

/// Minimum gap between consecutive `textDocument/didChange` sends per
/// document. Without this, every keystroke triggers a fresh
/// rust-analyzer re-analysis and the resulting CPU contention spikes
/// keystroke latency well past the 33 ms hard limit (spec §8). 100 ms
/// is short enough that the user doesn't notice the lag in diagnostics
/// and long enough to coalesce a typing burst into one analysis pass.
const DIDCHANGE_DEBOUNCE: Duration = Duration::from_millis(100);

use editor_lsp_client::lsp_types::{
    self, notification::Notification as _, CompletionItem, CompletionResponse,
    CompletionTriggerKind, Diagnostic, GotoDefinitionResponse, Hover, Location,
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

/// Walk up from `start` looking for a directory that marks a project
/// root. Returns the **topmost** marker location bounded by a git repo
/// (or the filesystem root if there's no `.git` ancestor).
///
/// "Topmost wins" is what rust-analyzer needs for Cargo workspaces:
/// `editor/crates/app/Cargo.toml` is a crate manifest, but
/// `editor/Cargo.toml` is the workspace manifest, and rust-analyzer
/// loads cross-crate information only when pointed at the latter. The
/// `.git` ceiling stops us from accidentally jumping out of one project
/// into a parent monorepo.
pub(crate) fn find_project_root(start: &Path) -> Option<PathBuf> {
    const MARKERS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "tsconfig.json",
        "jsconfig.json",
        "pyproject.toml",
        "go.mod",
        "pubspec.yaml",
    ];
    let mut current = start.parent()?;
    let mut highest: Option<PathBuf> = None;
    loop {
        for marker in MARKERS {
            if current.join(marker).exists() {
                highest = Some(current.to_path_buf());
                break;
            }
        }
        // `.git` is the project-tree ceiling: don't jump out of one
        // repo into its parent.
        if current.join(".git").exists() {
            return highest.or_else(|| Some(current.to_path_buf()));
        }
        current = match current.parent() {
            Some(p) => p,
            None => return highest,
        };
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
        Language::Html => "html",
        Language::Css => "css",
        Language::Java => "java",
        Language::Swift => "swift",
        Language::Cpp => "cpp",
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
    /// When we last shipped a `textDocument/didChange` for each open URI.
    /// Used by the debouncer to skip a send when the previous one is
    /// still fresh.
    last_didchange_at: HashMap<Url, Instant>,
    /// Most recent didChange that the debouncer postponed. Flushed by
    /// [`drain`](LspState::drain) once the debounce window has passed.
    pending_change: Option<PendingChange>,
}

struct PendingChange {
    uri: Url,
    version: i32,
    text: String,
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
    Completion {
        doc_path: PathBuf,
        /// The caret's char index when the request fired — the receiver
        /// uses this as the anchor for positioning the popup and as the
        /// start of the prefix the user has typed so far.
        anchor_char: usize,
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
    /// A completion response arrived for the matching request.
    Completion {
        doc_path: PathBuf,
        anchor_char: usize,
        items: Vec<CompletionItem>,
    },
    /// The server's stderr / our reader thread died — the slot is gone.
    ServerExited { kind: ServerKind },
}

pub struct LspState {
    slots: HashMap<ServerKind, Slot>,
    pending: HashMap<i64, PendingRequest>,
    diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
}

impl LspState {
    pub fn new() -> Self {
        Self {
            slots: HashMap::new(),
            pending: HashMap::new(),
            diagnostics: HashMap::new(),
        }
    }

    /// Diagnostics keyed by canonical document path. Empty when none are
    /// known.
    pub fn diagnostics_for(&self, path: &Path) -> Option<&[Diagnostic]> {
        self.diagnostics.get(path).map(|v| v.as_slice())
    }

    /// Whether a server slot has been spawned for `lang`. Callers use this
    /// to avoid materialising the buffer text when no server would
    /// consume it (e.g. opening a Python file before that language has
    /// any LSP wiring).
    pub fn has_server(&self, lang: Language) -> bool {
        ServerKind::for_language(lang)
            .map(|k| self.slots.contains_key(&k))
            .unwrap_or(false)
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

    /// Spawn the server for `lang` if it isn't running yet, rooted at the
    /// project containing `hint_path` (walks up looking for Cargo.toml /
    /// package.json / tsconfig.json / etc.). Returns the server kind on
    /// success, `None` when no server is wired or when spawning the binary
    /// failed (e.g. not on PATH — feature silently disables).
    pub fn ensure_server(&mut self, lang: Language, hint_path: &Path) -> Option<ServerKind> {
        let kind = ServerKind::for_language(lang)?;
        if self.slots.contains_key(&kind) {
            return Some(kind);
        }
        let root = find_project_root(hint_path);
        let root_uri = root.as_deref().and_then(|p| Url::from_file_path(p).ok());
        log::info!(
            "LSP: starting {kind:?} for {} with root {}",
            hint_path.display(),
            root.as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<none>".into()),
        );
        let (cmd, args) = kind.command();
        let mut client = match LspClient::spawn(cmd, &args) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("LSP: could not spawn {cmd}: {e} — feature disabled for {kind:?}");
                return None;
            }
        };
        let init_id = match client.initialize(root_uri) {
            Ok(id) => id,
            Err(e) => {
                log::warn!("LSP: initialize write failed for {kind:?}: {e}");
                return None;
            }
        };
        log::info!("LSP: {kind:?} initialize sent (id={init_id})");
        self.slots.insert(
            kind,
            Slot {
                client,
                initialized: false,
                init_id,
                open_uris: HashSet::new(),
                pending_opens: Vec::new(),
                last_didchange_at: HashMap::new(),
                pending_change: None,
            },
        );
        Some(kind)
    }

    /// Notify the right server that `path` is now open. No-op when no
    /// server is wired for `lang`, when spawning failed, or when `path`
    /// isn't an absolute file path (untitled scratch docs).
    pub fn did_open(&mut self, path: &Path, lang: Language, version: i32, text: String) {
        let Some(uri) = path_to_uri(path) else {
            log::warn!(
                "LSP: skipping didOpen — path {:?} cannot be turned into a file:// URI \
                 (relative path? canonicalize before passing in)",
                path,
            );
            return;
        };
        let Some(kind) = self.ensure_server(lang, path) else {
            return;
        };
        let Some(slot) = self.slots.get_mut(&kind) else {
            return;
        };
        let lang_id = language_id(lang);
        if !slot.initialized {
            // Queue the open — actual didOpen flush happens once we receive
            // the initialize response. Replace any earlier queued entry for
            // the same URI so we don't ship stale text.
            slot.pending_opens.retain(|o| o.uri != uri);
            slot.pending_opens.push(PendingOpen {
                uri,
                lang_id,
                version,
                text,
            });
            return;
        }
        if slot.open_uris.insert(uri.clone()) {
            log::info!("LSP: didOpen {kind:?} {} (v{version})", path.display());
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
        if !slot.initialized {
            // Server is still handshaking. Refresh the queued didOpen with
            // the latest text so the post-init flush ships fresh content
            // instead of whatever the doc looked like at first didOpen.
            if let Some(po) = slot.pending_opens.iter_mut().find(|o| o.uri == uri) {
                po.version = version;
                po.text = text;
            }
            return;
        }
        if !slot.open_uris.contains(&uri) {
            // didOpen never reached this server for this URI — likely a
            // language we don't sync. Skip silently.
            return;
        }
        // Debounce: if we sent a didChange for this URI very recently,
        // park the new one. The `drain` loop will flush it once the
        // debounce window has passed — coalescing a typing burst into
        // one analysis instead of N.
        let too_soon = slot
            .last_didchange_at
            .get(&uri)
            .is_some_and(|t| t.elapsed() < DIDCHANGE_DEBOUNCE);
        if too_soon {
            slot.pending_change = Some(PendingChange { uri, version, text });
            return;
        }
        if let Err(e) = slot.client.did_change_full(uri.clone(), version, text) {
            log::warn!("LSP: didChange failed for {kind:?}: {e}");
            return;
        }
        slot.last_didchange_at.insert(uri, Instant::now());
        slot.pending_change = None;
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

    /// Send a `textDocument/completion` request. `anchor_char` is the
    /// caret's char index when the request fired — the response carries
    /// it back so the popup anchors to the right place even after the
    /// user has typed more characters in the meantime. Returns the
    /// request id, or `None` when no server is wired.
    pub fn request_completion(
        &mut self,
        path: &Path,
        lang: Language,
        position: lsp_types::Position,
        anchor_char: usize,
        trigger: CompletionTriggerKind,
        trigger_character: Option<String>,
    ) -> Option<i64> {
        let uri = path_to_uri(path)?;
        let kind = ServerKind::for_language(lang)?;
        let slot = self.slots.get_mut(&kind)?;
        if !slot.initialized || !slot.open_uris.contains(&uri) {
            return None;
        }
        match slot
            .client
            .request_completion(uri, position, trigger, trigger_character)
        {
            Ok(id) => {
                self.pending.insert(
                    id,
                    PendingRequest::Completion {
                        doc_path: path.to_path_buf(),
                        anchor_char,
                    },
                );
                Some(id)
            }
            Err(e) => {
                log::warn!("LSP: completion request failed: {e}");
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
        // Flush any debounced didChange that has aged past the window.
        for (kind, slot) in self.slots.iter_mut() {
            let due = match slot.pending_change.as_ref() {
                Some(p) => slot
                    .last_didchange_at
                    .get(&p.uri)
                    .map_or(true, |t| t.elapsed() >= DIDCHANGE_DEBOUNCE),
                None => false,
            };
            if due {
                let pc = slot.pending_change.take().expect("checked above");
                let uri = pc.uri.clone();
                if let Err(e) = slot.client.did_change_full(pc.uri, pc.version, pc.text) {
                    log::warn!("LSP: deferred didChange failed for {kind:?}: {e}");
                } else {
                    slot.last_didchange_at.insert(uri, Instant::now());
                }
            }
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
                                log::info!(
                                    "LSP {kind:?} publishDiagnostics {} ({} item(s))",
                                    path.display(),
                                    p.diagnostics.len(),
                                );
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
                    log::info!("LSP {kind:?} initialize response received");
                    slot.initialized = true;
                    let _ = slot.client.initialized();
                    let queued = std::mem::take(&mut slot.pending_opens);
                    log::info!("LSP {kind:?} flushing {} queued didOpen(s)", queued.len());
                    for op in queued {
                        if slot.open_uris.insert(op.uri.clone()) {
                            let path_str = op.uri.path().to_string();
                            log::info!(
                                "LSP {kind:?} didOpen (post-init) {path_str} v{}",
                                op.version
                            );
                            if let Err(e) = slot
                                .client
                                .did_open(op.uri, op.lang_id, op.version, op.text)
                            {
                                log::warn!("LSP {kind:?} didOpen flush failed: {e}");
                            }
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
                    PendingRequest::Completion {
                        doc_path,
                        anchor_char,
                    } => {
                        let items = r
                            .result
                            .and_then(|v| serde_json::from_value::<CompletionResponse>(v).ok())
                            .map(|r| match r {
                                CompletionResponse::Array(items) => items,
                                CompletionResponse::List(list) => list.items,
                            })
                            .unwrap_or_default();
                        events.push(LspEvent::Completion {
                            doc_path,
                            anchor_char,
                            items,
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
        let mut state = LspState::new();
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
    fn find_project_root_prefers_workspace_over_crate() {
        // CARGO_MANIFEST_DIR == .../editor/crates/app. Walking up from a
        // source file inside this crate should reach the workspace root
        // at .../editor (which contains a `[workspace]` Cargo.toml), not
        // stop at the inner crate Cargo.toml.
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lsp.rs");
        let root = find_project_root(&here).expect("Cargo.toml exists upward");
        assert_eq!(
            root.file_name().and_then(|n| n.to_str()),
            Some("editor"),
            "expected workspace root, got {root:?}"
        );
    }

    #[test]
    fn missing_server_silently_skips() {
        // A pristine state with no spawned servers shouldn't panic when we
        // try to send didChange — it just no-ops.
        let mut state = LspState::new();
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
