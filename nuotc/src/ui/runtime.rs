use std::any::Any;
use std::collections::HashMap;
use std::hash::Hash;

use crate::{Frame, Rect};

use super::scene::{Component, NodeId, NodeLayout, PointerPolicy, Scene, UiError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lifecycle<K> {
    Mounted {
        key: K,
        id: NodeId,
    },
    Unmounted {
        key: K,
        id: NodeId,
    },
    FocusChanged {
        previous: Option<NodeId>,
        current: Option<NodeId>,
    },
    CaptureReleased {
        id: NodeId,
    },
}

/// Owns mounted instances and frame transactions. Scene declarations carry no
/// application vocabulary and are never the storage for business models.
pub struct UiRuntime<K> {
    presented: Scene<K>,
    pending: Option<Scene<K>>,
    next_id: u64,
    focused: Option<NodeId>,
    capture: Option<NodeId>,
    focus_history: Vec<NodeId>,
    state: HashMap<NodeId, Box<dyn Any + Send>>,
    invalidated: Vec<K>,
}

impl<K> Default for UiRuntime<K> {
    fn default() -> Self {
        Self {
            presented: Scene::default(),
            pending: None,
            next_id: 0,
            focused: None,
            capture: None,
            focus_history: Vec::new(),
            state: HashMap::new(),
            invalidated: Vec::new(),
        }
    }
}

impl<K: Clone + Eq + Hash> UiRuntime<K> {
    pub fn presented(&self) -> &Scene<K> {
        &self.presented
    }

    pub fn pending(&self) -> Result<&Scene<K>, UiError> {
        self.pending.as_ref().ok_or(UiError::NoPendingFrame)
    }

    /// Replaces an uncommitted measurement pass. It cannot unmount instances
    /// or alter the interaction snapshot that the user is currently seeing.
    pub fn begin(&mut self, viewport: Rect) {
        self.pending = Some(Scene {
            viewport,
            ..Scene::default()
        });
    }

    pub fn mount(&mut self, component: Component<K>) -> Result<NodeLayout, UiError> {
        let pending = self.pending.as_mut().ok_or(UiError::NoPendingFrame)?;
        // A stable key denotes the instance, independent of sibling position.
        let id = if let Some(id) = self.presented.id(&component.key) {
            id
        } else {
            self.next_id = self
                .next_id
                .checked_add(1)
                .ok_or(UiError::IdentityExhausted)?;
            NodeId(self.next_id)
        };
        pending.push(id, component)
    }

    pub fn abort(&mut self) {
        self.pending = None;
    }

    /// Refine a measured allocation before presentation. Descendant clips are
    /// recomputed from declarations, never adjusted from stale screen boxes.
    pub fn place(&mut self, key: &K, layout: super::LayoutBox<K>) -> Result<NodeLayout, UiError> {
        let scene = self.pending.as_mut().ok_or(UiError::NoPendingFrame)?;
        let index = *scene.keys.get(key).ok_or(UiError::MissingNode)?;
        scene.nodes[index].component.layout = layout;
        let nodes = std::mem::take(&mut scene.nodes);
        scene.keys.clear();
        scene.order.clear();
        for node in nodes {
            scene.push(node.id, node.component)?;
        }
        scene.layout(key).ok_or(UiError::MissingNode)
    }

    /// Explicit invalidation is for mutable leaf models. Declarative leaves
    /// can instead change their revision. Invalidations survive failed writes.
    pub fn invalidate_paint(&mut self, key: K) {
        if !self.invalidated.contains(&key) {
            self.invalidated.push(key);
        }
    }

    /// Paint an explicitly updated component during composition. The pending
    /// tree supplies its clip; a painter cannot choose a second allocation.
    /// This eager entry is useful for streaming document leaves. Declarative
    /// revision-based components use `paint` to skip unchanged work.
    pub fn paint_node<R>(
        &self,
        key: &K,
        frame: &mut Frame<'_>,
        painter: impl FnOnce(&mut Frame<'_>) -> R,
    ) -> Result<R, UiError> {
        let layout = self.pending()?.layout(key).ok_or(UiError::MissingNode)?;
        Ok(frame.paint_clipped(layout.clip, painter))
    }

    /// Both the previous and new coverage are damaged. A node with unchanged
    /// text but a different clip or layer still needs composition again.
    pub fn damage(&self) -> Result<Vec<Rect>, UiError> {
        let pending = self.pending()?;
        if self.presented.viewport != pending.viewport {
            return Ok(vec![pending.viewport]);
        }
        let mut damage = Vec::new();
        for old in &self.presented.nodes {
            let changed = pending.node(&old.component.key).is_none_or(|new| {
                old.layout != new.layout
                    || old.component.layer != new.component.layer
                    || old.component.revision != new.component.revision
                    || self.invalidated.contains(&old.component.key)
            });
            if changed {
                damage.push(old.layout.clip);
            }
        }
        for new in &pending.nodes {
            let changed = self.presented.node(&new.component.key).is_none_or(|old| {
                old.layout != new.layout
                    || old.component.layer != new.component.layer
                    || old.component.revision != new.component.revision
                    || self.invalidated.contains(&new.component.key)
            });
            if changed {
                damage.push(new.layout.clip);
            }
        }
        // Reordering siblings can change coverage without changing any box.
        let old_order: Vec<_> = self.presented.paint_order().map(|(key, _)| key).collect();
        let new_order: Vec<_> = pending.paint_order().map(|(key, _)| key).collect();
        if old_order != new_order {
            damage.push(pending.viewport);
        }
        damage.retain(|rect| rect.area() > 0);
        Ok(damage)
    }

    /// Composes damaged nodes in paint order. Repaints overlapping unchanged
    /// nodes as well: a clean overlay still covers a changed node behind it.
    /// The caller supplies the root background painter to clear old coverage.
    pub fn paint(
        &self,
        frame: &mut Frame<'_>,
        mut painter: impl FnMut(&K, NodeLayout, &mut Frame<'_>),
    ) -> Result<(), UiError> {
        let damage = self.damage()?;
        let Some(damage) = damage.into_iter().reduce(|a, b| {
            let x = a.x.min(b.x);
            let y = a.y.min(b.y);
            Rect::new(
                x,
                y,
                a.right().max(b.right()).saturating_sub(x),
                a.bottom().max(b.bottom()).saturating_sub(y),
            )
        }) else {
            return Ok(());
        };
        for (key, layout) in self.pending()?.paint_order() {
            let clip = damage.intersection(layout.clip);
            if clip.area() > 0 {
                frame.paint_clipped(clip, |frame| painter(key, layout, frame));
            }
        }
        Ok(())
    }

    /// Call only after successful terminal presentation. This is the only
    /// operation that publishes geometry or disposes unmounted local state.
    pub fn commit(&mut self) -> Result<Vec<Lifecycle<K>>, UiError> {
        let next = self.pending.take().ok_or(UiError::NoPendingFrame)?;
        let mut events = Vec::new();
        // Children are torn down before their logical owners.
        for node in self.presented.nodes.iter().rev() {
            if next.id(&node.component.key) != Some(node.id) {
                self.state.remove(&node.id);
                events.push(Lifecycle::Unmounted {
                    key: node.component.key.clone(),
                    id: node.id,
                });
            }
        }
        for node in &next.nodes {
            if self.presented.id(&node.component.key) != Some(node.id) {
                events.push(Lifecycle::Mounted {
                    key: node.component.key.clone(),
                    id: node.id,
                });
            }
        }
        self.presented = next;
        self.invalidated.clear();
        let previous = self.focused;
        self.focus_history
            .retain(|&id| self.presented.key(id).is_some());
        if self.focused.is_some_and(|id| !self.focus_eligible(id)) {
            if let Some(id) = self.focused.filter(|id| self.presented.key(*id).is_some()) {
                self.focus_history.push(id);
            }
            self.focused = None;
        }
        if self.focused.is_none() {
            self.focused = self
                .focus_history
                .iter()
                .rev()
                .copied()
                .find(|&id| self.focus_eligible(id))
                .or_else(|| {
                    self.presented
                        .foreground()
                        .and_then(|key| self.presented.id(key))
                })
                .or_else(|| {
                    self.presented.order.iter().find_map(|&index| {
                        let node = &self.presented.nodes[index];
                        self.focus_eligible(node.id).then_some(node.id)
                    })
                });
        }
        if previous != self.focused {
            events.push(Lifecycle::FocusChanged {
                previous,
                current: self.focused,
            });
        }
        if let Some(id) = self.capture
            && !self.capture_eligible(id)
        {
            self.capture = None;
            events.push(Lifecycle::CaptureReleased { id });
        }
        Ok(events)
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    pub fn focus(&mut self, id: NodeId) -> Result<(), UiError> {
        if !self.focus_eligible(id) {
            return Err(UiError::InvalidFocus);
        }
        if self.focused != Some(id) {
            if let Some(old) = self.focused {
                self.focus_history.retain(|&saved| saved != old);
                self.focus_history.push(old);
            }
            self.focused = Some(id);
        }
        Ok(())
    }

    pub fn capture_pointer(&mut self, id: NodeId) -> Result<(), UiError> {
        if !self.capture_eligible(id) {
            return Err(UiError::InvalidCapture);
        }
        self.capture = Some(id);
        Ok(())
    }

    pub fn release_pointer(&mut self) {
        self.capture = None;
    }

    pub fn pointer_target(&self, x: u16, y: u16) -> Option<&K> {
        self.capture
            .and_then(|id| self.presented.key(id))
            .or_else(|| self.presented.hit_test(x, y))
    }

    pub fn state<T: Any + Send + Default>(&mut self, id: NodeId) -> Result<&mut T, UiError> {
        if self.presented.key(id).is_none() {
            return Err(UiError::MissingNode);
        }
        self.state
            .entry(id)
            .or_insert_with(|| Box::<T>::default())
            .downcast_mut::<T>()
            .ok_or(UiError::MissingNode)
    }

    fn focus_eligible(&self, id: NodeId) -> bool {
        self.presented.key(id).is_some_and(|key| {
            self.presented.component(key).is_some_and(|c| c.focusable)
                && self
                    .presented
                    .layout(key)
                    .is_some_and(|layout| layout.clip.area() > 0)
                && self
                    .presented
                    .focus_barrier()
                    .is_none_or(|scope| self.presented.is_descendant(key, scope))
        })
    }

    fn capture_eligible(&self, id: NodeId) -> bool {
        self.presented.key(id).is_some_and(|key| {
            self.presented
                .component(key)
                .is_some_and(|c| c.pointer != PointerPolicy::Transparent)
                && self
                    .presented
                    .layout(key)
                    .is_some_and(|layout| layout.clip.area() > 0)
                && self
                    .presented
                    .pointer_barrier()
                    .is_none_or(|scope| self.presented.is_descendant(key, scope))
        })
    }
}
