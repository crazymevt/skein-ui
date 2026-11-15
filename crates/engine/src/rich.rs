//! Rich-text nodes: wrapped, styled spans (`:spans`, see text.rs). The
//! engine selects their text by dragging (one node at a time), copies it,
//! and turns a click on a link span into `{:event :link :node n :href h}`.
//! Text starts at the node's top left; give a wrapping box the padding.

use crate::data::{Data, Event, NodeId, WindowId};
use crate::engine::{Engine, Measure};
use crate::text::{span_at, spans_of, spans_text};

/// A selection in a rich-text node: byte indexes into its text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RichSel {
    pub node: NodeId,
    pub anchor: usize,
    pub focus: usize,
    /// The pointer is down in it.
    pub dragging: bool,
    /// The pointer moved since it went down (so it's not a click).
    pub moved: bool,
}

impl RichSel {
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }
}

impl Engine {
    pub fn is_rich(&self, id: NodeId) -> bool {
        self.nodes.get(&id).is_some_and(|n| n.kind == "rich-text")
    }

    fn rich_index(&self, id: NodeId, x: f32, y: f32, m: &mut dyn Measure) -> usize {
        let (Some(n), Some(r)) = (self.nodes.get(&id), self.rect(id)) else { return 0 };
        let spans = spans_of(n.prop("spans"));
        m.rich_index_at(&spans, &n.resolved, r.w, x - r.x, y - r.y)
    }

    fn damage_node(&mut self, id: NodeId) {
        if let (Some(w), Some(r)) = (self.window_of(id), self.rect(id)) {
            self.windows.get_mut(&w).unwrap().damage.push(r);
        }
    }

    /// The pointer went down at (x, y) over `over`: start a selection in a
    /// rich-text node, or drop the one there was.
    pub(crate) fn rich_down(&mut self, over: Option<NodeId>, x: f32, y: f32, shift: bool, m: &mut dyn Measure) {
        let old = self.rich_sel;
        match over.filter(|id| self.is_rich(*id)) {
            Some(id) => {
                let i = self.rich_index(id, x, y, m);
                let anchor = match old {
                    Some(s) if shift && s.node == id => s.anchor,
                    _ => i,
                };
                self.rich_sel = Some(RichSel { node: id, anchor, focus: i, dragging: true, moved: false });
            }
            None => self.rich_sel = None,
        }
        if let Some(s) = old
            && old.map(|o| o.range()) != self.rich_sel.map(|n| n.range())
        {
            self.damage_node(s.node);
        }
        if let Some(s) = self.rich_sel {
            self.damage_node(s.node);
        }
    }

    /// The pointer moved while down in rich text: extend the selection.
    /// True if a rich-text drag took the motion.
    pub(crate) fn rich_move(&mut self, x: f32, y: f32, m: &mut dyn Measure) -> bool {
        let Some(s) = self.rich_sel.filter(|s| s.dragging) else { return false };
        let i = self.rich_index(s.node, x, y, m);
        if i != s.focus {
            self.rich_sel = Some(RichSel { focus: i, moved: true, ..s });
            self.damage_node(s.node);
        }
        true
    }

    /// The pointer came up: a click (no drag) on a link sends :link.
    pub(crate) fn rich_up(&mut self, w: WindowId) {
        let Some(s) = self.rich_sel else { return };
        self.rich_sel = Some(RichSel { dragging: false, ..s });
        if s.moved || s.anchor != s.focus {
            return;
        }
        let Some(n) = self.nodes.get(&s.node) else { return };
        let spans = spans_of(n.prop("spans"));
        if let Some(href) = span_at(&spans, s.anchor).and_then(|sp| sp.link()).cloned() {
            self.events.push(Event::new(
                "link",
                vec![("window", Data::Num(w as f64)), ("node", Data::Num(s.node as f64)), ("href", href)],
            ));
        }
    }

    /// The selected rich text, if any.
    pub fn rich_selection_text(&self) -> Option<String> {
        let s = self.rich_sel?;
        let (a, b) = s.range();
        if a == b {
            return None;
        }
        let text = spans_text(&spans_of(self.nodes.get(&s.node)?.prop("spans")));
        text.get(a..b.min(text.len())).map(str::to_owned)
    }

    /// The selection in rich-text node id, as byte indexes.
    pub fn rich_selection_in(&self, id: NodeId) -> Option<(usize, usize)> {
        self.rich_sel.filter(|s| s.node == id).map(|s| s.range()).filter(|(a, b)| a < b)
    }
}
