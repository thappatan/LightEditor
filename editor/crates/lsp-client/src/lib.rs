//! Minimal Language Server Protocol client (spec §3.5, §4.2, ADR-007).
//!
//! Spawn a server with [`LspClient::spawn`], drive the handshake via
//! [`initialize`](LspClient::initialize) + [`initialized`](LspClient::initialized),
//! then exchange notifications and requests. Incoming server messages
//! arrive on a non-blocking channel — drain them from the host's event
//! loop with [`try_recv`](LspClient::try_recv).
//!
//! `lsp-types` provides the protocol's strongly-typed params/results, so
//! callers can build `lsp_types::HoverParams` / consume `lsp_types::Hover`
//! directly from this crate's `Value`-shaped messages.

pub mod client;
pub mod jsonrpc;

pub use client::{path_to_uri, LspClient};
pub use jsonrpc::{Message, Notification, Request, Response, ResponseError};

// Re-export the upstream protocol types so app code can stay one crate
// away from `lsp-types` version drift.
pub use lsp_types;
