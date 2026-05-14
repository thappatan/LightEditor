//! The retained-mode scene graph (spec §3.2).
//!
//! The scene is a tree of [`SceneNode`]s that persists across frames. Callers
//! mutate it in place; each mutation marks the touched node dirty. Once per
//! frame the renderer calls [`Scene::collect_damage`] to get the absolute
//! rectangles that changed, redraws only those, then calls
//! [`Scene::clear_dirty`].
//!
//! Dirty state propagates *downward only*: a node is "damaged" if it is dirty
//! or any descendant is. That is computed by walking the tree at collection
//! time, so mutating a node never has to reach back up to its parent — which
//! keeps the tree a plain owned structure with no parent pointers.
//!
//! This crate is the scene *description*. Turning it into draw calls is the
//! renderer's job; laying out node bounds is the layout engine's job.

use crate::geometry::{Color, Rect};

/// What a [`SceneNode`] draws.
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    /// Draws nothing itself — a pure container for layout and grouping.
    Group,
    /// A solid-color rectangle filling the node's bounds.
    Quad { color: Color },
}

/// A node in the retained scene graph.
///
/// `bounds` is relative to the parent's origin. Use [`Scene::collect_damage`]
/// (or walk with an accumulated offset) to get absolute coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneNode {
    bounds: Rect,
    primitive: Primitive,
    children: Vec<SceneNode>,
    dirty: bool,
}

impl SceneNode {
    /// A new node. It starts dirty — it has never been drawn.
    pub fn new(bounds: Rect, primitive: Primitive) -> Self {
        Self {
            bounds,
            primitive,
            children: Vec::new(),
            dirty: true,
        }
    }

    /// A pure container node with no primitive of its own.
    pub fn group(bounds: Rect) -> Self {
        Self::new(bounds, Primitive::Group)
    }

    /// A solid-color rectangle.
    pub fn quad(bounds: Rect, color: Color) -> Self {
        Self::new(bounds, Primitive::Quad { color })
    }

    /// The node's parent-relative bounds.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// The node's primitive.
    pub fn primitive(&self) -> &Primitive {
        &self.primitive
    }

    /// The node's children, in draw order (earlier = behind).
    pub fn children(&self) -> &[SceneNode] {
        &self.children
    }

    /// Whether this node was mutated since the last [`clear_dirty`].
    ///
    /// This is the node's *own* flag — a clean node may still have dirty
    /// descendants. [`clear_dirty`]: SceneNode::clear_dirty
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Set the bounds, marking the node dirty if they actually changed.
    pub fn set_bounds(&mut self, bounds: Rect) {
        if self.bounds != bounds {
            self.bounds = bounds;
            self.dirty = true;
        }
    }

    /// Set the primitive, marking the node dirty if it actually changed.
    pub fn set_primitive(&mut self, primitive: Primitive) {
        if self.primitive != primitive {
            self.primitive = primitive;
            self.dirty = true;
        }
    }

    /// Append a child. Marks this node dirty — its subtree changed shape.
    pub fn push_child(&mut self, child: SceneNode) {
        self.children.push(child);
        self.dirty = true;
    }

    /// Mutable access to a child by index, for in-place updates. The child's
    /// own mutators mark *it* dirty; damage collection finds it from there, so
    /// nothing needs to propagate up to this node.
    pub fn child_mut(&mut self, index: usize) -> Option<&mut SceneNode> {
        self.children.get_mut(index)
    }

    /// Remove all children. Marks this node dirty if it had any.
    pub fn clear_children(&mut self) {
        if !self.children.is_empty() {
            self.children.clear();
            self.dirty = true;
        }
    }

    /// Force the node dirty — e.g. when something it depends on changed
    /// without its own fields changing.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Recursively gather the absolute rects of every dirty node, offsetting
    /// each subtree by `parent_origin`. A dirty node contributes its whole
    /// absolute rect; children are still visited so a clean parent with a
    /// dirty child still reports the child.
    fn collect_damage_into(&self, parent_origin: (f32, f32), out: &mut Vec<Rect>) {
        let abs = self.bounds.translated(parent_origin.0, parent_origin.1);
        if self.dirty {
            out.push(abs);
        }
        for child in &self.children {
            child.collect_damage_into((abs.min_x(), abs.min_y()), out);
        }
    }

    /// Recursively clear the dirty flag on this node and all descendants.
    fn clear_dirty_recursive(&mut self) {
        self.dirty = false;
        for child in &mut self.children {
            child.clear_dirty_recursive();
        }
    }
}

/// The scene graph for one render surface — a single root [`SceneNode`].
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    root: SceneNode,
}

impl Scene {
    /// A scene with the given root node.
    pub fn new(root: SceneNode) -> Self {
        Self { root }
    }

    /// A scene with an empty group root covering `bounds`.
    pub fn with_root_bounds(bounds: Rect) -> Self {
        Self::new(SceneNode::group(bounds))
    }

    /// The root node.
    pub fn root(&self) -> &SceneNode {
        &self.root
    }

    /// Mutable access to the root node.
    pub fn root_mut(&mut self) -> &mut SceneNode {
        &mut self.root
    }

    /// The absolute rects of every dirty node, in tree (draw) order.
    ///
    /// The renderer can union these into a damage region and redraw only
    /// what it covers. An all-clean scene returns an empty vec.
    pub fn collect_damage(&self) -> Vec<Rect> {
        let mut out = Vec::new();
        self.root.collect_damage_into((0.0, 0.0), &mut out);
        out
    }

    /// Whether any node in the scene is dirty.
    pub fn is_dirty(&self) -> bool {
        fn any_dirty(node: &SceneNode) -> bool {
            node.is_dirty() || node.children().iter().any(any_dirty)
        }
        any_dirty(&self.root)
    }

    /// Clear the dirty flag on every node — call after a frame is rendered.
    pub fn clear_dirty(&mut self) {
        self.root.clear_dirty_recursive();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Color;

    fn quad(x: f32, y: f32, w: f32, h: f32) -> SceneNode {
        SceneNode::quad(Rect::new(x, y, w, h), Color::WHITE)
    }

    #[test]
    fn new_nodes_start_dirty() {
        let node = quad(0.0, 0.0, 10.0, 10.0);
        assert!(node.is_dirty());
        let scene = Scene::new(node);
        assert!(scene.is_dirty());
    }

    #[test]
    fn clear_dirty_makes_the_whole_tree_clean() {
        let mut root = SceneNode::group(Rect::new(0.0, 0.0, 100.0, 100.0));
        root.push_child(quad(10.0, 10.0, 20.0, 20.0));
        let mut scene = Scene::new(root);

        assert!(scene.is_dirty());
        scene.clear_dirty();
        assert!(!scene.is_dirty());
        assert!(scene.collect_damage().is_empty());
    }

    #[test]
    fn setting_same_value_does_not_redirty() {
        let mut node = quad(0.0, 0.0, 10.0, 10.0);
        node.clear_dirty_recursive();

        node.set_bounds(Rect::new(0.0, 0.0, 10.0, 10.0)); // unchanged
        assert!(!node.is_dirty());
        node.set_primitive(Primitive::Quad {
            color: Color::WHITE,
        }); // unchanged
        assert!(!node.is_dirty());

        node.set_bounds(Rect::new(1.0, 0.0, 10.0, 10.0)); // changed
        assert!(node.is_dirty());
    }

    #[test]
    fn mutating_a_child_is_found_by_damage_collection() {
        let mut root = SceneNode::group(Rect::new(0.0, 0.0, 200.0, 200.0));
        root.push_child(quad(10.0, 10.0, 20.0, 20.0));
        root.push_child(quad(50.0, 50.0, 20.0, 20.0));
        let mut scene = Scene::new(root);
        scene.clear_dirty();

        // mutate only the second child
        scene
            .root_mut()
            .child_mut(1)
            .unwrap()
            .set_primitive(Primitive::Quad {
                color: Color::BLACK,
            });

        let damage = scene.collect_damage();
        // only the second child's absolute rect is damaged
        assert_eq!(damage, vec![Rect::new(50.0, 50.0, 20.0, 20.0)]);
    }

    #[test]
    fn damage_rects_are_absolute() {
        // nested groups offset their children
        let mut root = SceneNode::group(Rect::new(0.0, 0.0, 300.0, 300.0));
        let mut panel = SceneNode::group(Rect::new(100.0, 50.0, 200.0, 200.0));
        panel.push_child(quad(10.0, 20.0, 30.0, 30.0));
        root.push_child(panel);
        let scene = Scene::new(root);

        let damage = scene.collect_damage();
        // root (0,0,300,300), panel (100,50,200,200), quad absolute = (110,70,30,30)
        assert!(damage.contains(&Rect::new(0.0, 0.0, 300.0, 300.0)));
        assert!(damage.contains(&Rect::new(100.0, 50.0, 200.0, 200.0)));
        assert!(damage.contains(&Rect::new(110.0, 70.0, 30.0, 30.0)));
        assert_eq!(damage.len(), 3);
    }

    #[test]
    fn push_child_dirties_the_parent() {
        let mut root = SceneNode::group(Rect::new(0.0, 0.0, 100.0, 100.0));
        let mut scene = Scene::new(root.clone());
        scene.clear_dirty();
        assert!(!scene.is_dirty());

        scene.root_mut().push_child(quad(0.0, 0.0, 5.0, 5.0));
        // parent is dirty (shape changed) and so is the freshly added child
        let damage = scene.collect_damage();
        assert_eq!(damage.len(), 2);

        // (silence unused warning on the standalone `root`)
        root.push_child(quad(0.0, 0.0, 1.0, 1.0));
        assert!(root.is_dirty());
    }

    #[test]
    fn clear_children_dirties_only_when_nonempty() {
        let mut node = SceneNode::group(Rect::new(0.0, 0.0, 10.0, 10.0));
        node.clear_dirty_recursive();
        node.clear_children(); // already empty
        assert!(!node.is_dirty());

        node.push_child(quad(0.0, 0.0, 2.0, 2.0));
        node.clear_dirty_recursive();
        node.clear_children(); // had a child
        assert!(node.is_dirty());
        assert!(node.children().is_empty());
    }
}
