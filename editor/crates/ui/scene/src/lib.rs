//! Retained-mode scene graph for the editor's UI framework (spec §3.2).
//!
//! The scene graph is the description layer between editor logic and the GPU
//! renderer. Callers build a [`Scene`] of [`SceneNode`]s once, mutate it in
//! place across frames, and the renderer redraws only the [`Rect`]s reported
//! by [`Scene::collect_damage`].
//!
//! This crate is pure logic — no wgpu, no winit — so it is exhaustively
//! unit-tested. The renderer (`editor-ui-render`) consumes it; the layout
//! engine produces the node bounds.

mod geometry;
mod scene;

pub use geometry::{Color, Point, Rect, Size};
pub use scene::{Primitive, Scene, SceneNode};
