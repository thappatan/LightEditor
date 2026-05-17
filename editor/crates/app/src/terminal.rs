//! Embedded PTY-backed terminal (spec §4.1.4, M3 "Developable").
//!
//! Wraps `alacritty_terminal` so the rest of the app can speak to a
//! real shell without touching the underlying PTY plumbing. The
//! crate spawns three threads internally — read pump, write pump,
//! and signal pump — and exposes them via:
//!
//! - [`TerminalPane::spawn`] — fork the user's `$SHELL`, build a
//!   `Term`, hand it to alacritty's `EventLoop`, and return a handle.
//! - [`TerminalPane::write`] — push keystrokes to the PTY.
//! - [`TerminalPane::resize`] — propagate window-size changes.
//!
//! Incoming events (grid changes, title updates, child exit) reach
//! the host via a `winit` `EventLoopProxy<AppEvent>`, so the host's
//! event loop drives redraws without us spinning another timer.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use alacritty_terminal::event::{
    Event as TermEvent, EventListener as TermEventListener, Notify, OnResize, WindowSize,
};
use alacritty_terminal::event_loop::{EventLoop as TermEventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use winit::event_loop::EventLoopProxy;

use crate::AppEvent;

/// Adapter from alacritty's [`TermEventListener`] callback to our
/// app's event-loop proxy. Cloned per worker thread (`Send + Clone`).
#[derive(Clone)]
pub struct AppTermProxy {
    proxy: EventLoopProxy<AppEvent>,
}

impl TermEventListener for AppTermProxy {
    fn send_event(&self, event: TermEvent) {
        // Every alacritty event maps to a single `TerminalWakeup` —
        // the renderer locks the term, walks the grid, and repaints.
        // The specific event variant (Title / Bell / ChildExit) is
        // currently unused; reserved for status-bar polish later.
        let _ = event; // explicitly discard; field-by-field handling is M3 polish
        let _ = self.proxy.send_event(AppEvent::TerminalWakeup);
    }
}

/// One running terminal. Owns the shared `Term` and the notifier
/// that talks to the PTY's writer thread.
pub struct TerminalPane {
    /// Whether the pane is currently visible. Hiding doesn't kill
    /// the shell — `Cmd-J` to hide, `Cmd-J` to bring it back with
    /// the same scrollback intact.
    pub visible: bool,
    /// `true` when the terminal owns the keyboard; `false` while
    /// focus is on the editor. The pane stays visible either way.
    pub focused: bool,
    /// Pane height in physical pixels. Fixed for v1; drag-resize is
    /// a polish item.
    pub height_px: f32,
    /// Cell metrics in physical pixels — the renderer divides the
    /// pane's rect by these to lay out the grid. Kept up to date by
    /// [`resize`](TerminalPane::resize); not yet read because v1
    /// renders text via cosmic-text rather than per-cell glyphs.
    #[allow(dead_code)]
    pub cell_width: f32,
    #[allow(dead_code)]
    pub cell_height: f32,
    /// Current grid dimensions in cells.
    pub cols: usize,
    pub rows: usize,
    /// The terminal state — locked while reading the grid for paint
    /// or writing changes from the read pump. `Arc<FairMutex>` so
    /// the worker threads can also hold it briefly.
    pub term: Arc<FairMutex<Term<AppTermProxy>>>,
    /// Sender into alacritty's worker thread (writes to the PTY).
    notifier: Notifier,
    /// Kept alive to keep the worker thread running. Dropping kills
    /// the shell — see [`Drop`]. Type-erased to keep this struct
    /// `where T: ?Sized`-free and avoid leaking alacritty's
    /// internal `EventLoop<Pty, _>` return type.
    _join: Option<std::thread::JoinHandle<()>>,
}

/// Spawned worker returns `(EventLoop, State)` — we only care that
/// it stays alive, so wrap it in a thread that owns the result.
fn forget_loop_return(
    handle: std::thread::JoinHandle<(
        TermEventLoop<tty::Pty, AppTermProxy>,
        alacritty_terminal::event_loop::State,
    )>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let _ = handle.join();
    })
}

impl TerminalPane {
    /// Spawn the shell defined by `$SHELL` (falling back to
    /// `/bin/bash`) into a PTY sized to `(cols, rows)` cells, each
    /// `cell_width × cell_height` physical pixels.
    pub fn spawn(
        proxy: EventLoopProxy<AppEvent>,
        cwd: Option<PathBuf>,
        cols: u16,
        rows: u16,
        cell_width: f32,
        cell_height: f32,
        pane_height_px: f32,
    ) -> Result<Self, String> {
        let event_proxy = AppTermProxy { proxy };

        // alacritty wants WindowSize in cells + cell pixels (so the
        // PTY can report a sensible TIOCSWINSZ to programs that ask).
        let window_size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cell_width.round().max(1.0) as u16,
            cell_height: cell_height.round().max(1.0) as u16,
        };

        let pty_options = tty::Options {
            shell: None, // None → alacritty picks $SHELL or /bin/sh
            working_directory: cwd,
            drain_on_exit: true,
            env: Default::default(),
            #[cfg(target_os = "windows")]
            escape_args: true,
        };

        let pty = tty::new(&pty_options, window_size, /* window_id */ 0)
            .map_err(|e| format!("failed to spawn PTY: {e}"))?;

        let size = TermDims {
            columns: cols as usize,
            screen_lines: rows as usize,
        };
        let term = Term::new(Config::default(), &size, event_proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let event_loop = TermEventLoop::new(
            term.clone(),
            event_proxy,
            pty,
            /* drain_on_exit */ false,
            /* ref_test */ false,
        )
        .map_err(|e| format!("failed to start terminal event loop: {e}"))?;

        let notifier = Notifier(event_loop.channel());
        let join = forget_loop_return(event_loop.spawn());

        Ok(TerminalPane {
            visible: true,
            focused: true,
            height_px: pane_height_px,
            cell_width,
            cell_height,
            cols: cols as usize,
            rows: rows as usize,
            term,
            notifier,
            _join: Some(join),
        })
    }

    /// Push bytes to the PTY (typed input, paste, etc.). Returns
    /// silently if the writer thread has gone away (shell exited).
    pub fn write<B: Into<Cow<'static, [u8]>>>(&self, bytes: B) {
        self.notifier.notify(bytes);
    }

    /// Tell the terminal + PTY about a new size. `cols`/`rows` are
    /// in cells; `cell_width`/`cell_height` are physical pixels.
    /// Wired in once window-resize is plumbed through to the
    /// terminal (M3 polish).
    #[allow(dead_code)]
    pub fn resize(&mut self, cols: u16, rows: u16, cell_width: f32, cell_height: f32) {
        if cols as usize == self.cols
            && rows as usize == self.rows
            && (cell_width - self.cell_width).abs() < 0.5
            && (cell_height - self.cell_height).abs() < 0.5
        {
            return;
        }
        self.cols = cols as usize;
        self.rows = rows as usize;
        self.cell_width = cell_width;
        self.cell_height = cell_height;

        let size = WindowSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cell_width.round().max(1.0) as u16,
            cell_height: cell_height.round().max(1.0) as u16,
        };
        // alacritty's event loop owns the PTY now — send a Resize
        // message so the worker thread propagates it.
        let _ = self.notifier.0.send(Msg::Resize(size));
        // Also resize the Term directly so the grid matches.
        let mut term = self.term.lock();
        term.resize(TermDims {
            columns: cols as usize,
            screen_lines: rows as usize,
        });
    }
}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        // Ask the worker thread to wind down cleanly. The
        // `JoinHandle` is dropped after, which forks the thread —
        // the worker exits when the PTY's read returns EOF (shell
        // gone) or this Shutdown message lands.
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

/// Minimal [`Dimensions`] impl alacritty needs to size the grid.
/// Scrollback is set by `Term::new` (default 10 000 lines) — we
/// only expose what the trait requires.
#[derive(Clone, Copy)]
struct TermDims {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for TermDims {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

impl OnResize for TermDims {
    fn on_resize(&mut self, ws: WindowSize) {
        self.screen_lines = ws.num_lines as usize;
        self.columns = ws.num_cols as usize;
    }
}
