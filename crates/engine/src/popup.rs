//! Popups, tooltips and context menus (docs/ui-protocol.md §4, §11).
//!
//! A popup is a window with an `:owner` window and a `:position` in the
//! owner's coordinates (backends show it as a small borderless OS window,
//! so it can reach past the owner's edges). `:fit-content` sizes a window
//! to its content. Popups are dismissed lightly: a press in an owner (other
//! than on the popup's `:anchor` node) or Escape sends `{:event :dismiss}`
//! for each popup it owns, and Skein closes them.
//!
//! Tooltips: hovering a node with a `:tooltip` prop for a moment sends
//! `{:event :tooltip :window w :node n :text t :x :y}`; leaving it sends
//! `{:event :tooltip :window w :node nil}`. Skein shows them as popups.
//!
//! Hovering in an editor: an editor with a `:hover` prop says where the
//! pointer rests, once it has stayed over one character of its text for
//! the same moment: `{:event :editor-hover :window w :node n :line l
//! :col c :x :y}`; moving on, leaving, a press or a key sends
//! `{:event :editor-hover :window w :node n :line nil}`.

use crate::data::{Data, Event, NodeId, WindowId};
use crate::engine::{Engine, Measure};
use std::time::{Duration, Instant};

pub const TOOLTIP_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct Tooltip {
    pub window: WindowId,
    pub node: NodeId,
    pub x: f32,
    pub y: f32,
    pub due: Instant,
    pub shown: bool,
    /// For an editor's hover: the (line, column) the pointer is over.
    pub at: Option<(usize, usize)>,
}

impl Engine {
    /// Window w's owner, if it is a popup.
    pub fn owner_of(&self, w: WindowId) -> Option<WindowId> {
        let win = self.windows.get(&w)?;
        win.props.iter().find(|(k, _)| k == "owner").and_then(|(_, v)| v.as_num()).map(|n| n as WindowId)
    }

    /// Is `a` an owner of `w`, directly or through other popups?
    pub fn owned_by(&self, mut w: WindowId, a: WindowId) -> bool {
        for _ in 0..64 {
            match self.owner_of(w) {
                Some(o) if o == a => return true,
                Some(o) => w = o,
                None => return false,
            }
        }
        false
    }

    fn window_prop(&self, w: WindowId, k: &str) -> Option<&Data> {
        self.windows.get(&w)?.props.iter().find(|(key, _)| key == k).map(|(_, v)| v)
    }

    /// The popups w owns (at any depth).
    pub fn popups_of(&self, w: WindowId) -> Vec<WindowId> {
        let mut ps: Vec<WindowId> = self.windows.keys().copied().filter(|p| self.owned_by(*p, w)).collect();
        ps.sort_unstable();
        ps
    }

    /// A press in window w at node `over`: dismiss the popups w owns,
    /// except one whose `:anchor` was pressed (its anchor toggles it).
    pub(crate) fn dismiss_for_press(&mut self, w: WindowId, over: Option<NodeId>) {
        for p in self.popups_of(w) {
            let anchor = self.window_prop(p, "anchor").and_then(Data::as_num).map(|n| n as NodeId);
            if let (Some(a), Some(o)) = (anchor, over)
                && self.holds(a, o)
            {
                continue;
            }
            self.dismiss(p);
        }
    }

    fn dismiss(&mut self, p: WindowId) {
        let me = Data::Num(p as f64);
        if !self.events.iter().any(|e| e.kind == "dismiss" && e.get("window") == Some(&me)) {
            self.events.push(Event::new("dismiss", vec![("window", me)]));
        }
    }

    /// Escape in window w dismisses its popups: true if there were any.
    pub(crate) fn escape_popups(&mut self, w: WindowId) -> bool {
        let ps = self.popups_of(w);
        for p in &ps {
            self.dismiss(*p);
        }
        !ps.is_empty()
    }

    /// The nearest node at or above id with a :tooltip, and its text.
    fn tooltip_at(&self, mut id: NodeId) -> Option<(NodeId, String)> {
        loop {
            let n = self.nodes.get(&id)?;
            if let Some(t) = n.prop("tooltip").and_then(Data::as_str) {
                return Some((id, t.to_owned()));
            }
            id = n.parent?;
        }
    }

    /// The editor at or above id that asks for `:hover`, and the (line,
    /// column) of its text at window point (x, y): None in its gutter,
    /// past a line's end or below its last line.
    fn editor_hover_at(&self, mut id: NodeId, x: f32, y: f32, m: &mut dyn Measure) -> Option<(NodeId, (usize, usize))> {
        loop {
            let n = self.nodes.get(&id)?;
            if self.is_editor(id) {
                if !n.prop("hover").is_some_and(Data::truthy) {
                    return None;
                }
                let (e, r) = (self.editors.get(&id)?, self.rect(id)?);
                let row = (y - r.y + e.scroll_y) / e.line_h;
                if row < 0.0 || row >= e.line_count as f32 {
                    return None;
                }
                let line = row.floor() as usize;
                let len = e.lines.get(&line)?.text.chars().count();
                let dx = x - r.x - e.gutter_w - 4.0 + e.scroll_x;
                if dx < 0.0 || dx > self.editor_col_x(id, line, len, m) {
                    return None;
                }
                let (line, col) = self.editor_position(id, x, y, m);
                // (the character under the pointer, not the gap nearest it)
                let col = if col > 0 && self.editor_col_x(id, line, col, m) > dx { col - 1 } else { col };
                return Some((id, (line, col.min(len.saturating_sub(1)))));
            }
            id = n.parent?;
        }
    }

    /// The pointer moved to (x, y) over `over`: start, keep or end a
    /// tooltip, or an editor's hover.
    pub(crate) fn update_tooltip(&mut self, w: WindowId, x: f32, y: f32, over: Option<NodeId>, m: &mut dyn Measure) {
        let tip = over.and_then(|n| self.tooltip_at(n)).map(|(n, _)| n);
        let (target, at) = match tip {
            Some(n) => (Some(n), None),
            None => match over.and_then(|n| self.editor_hover_at(n, x, y, m)) {
                Some((n, pos)) => (Some(n), Some(pos)),
                None => (None, None),
            },
        };
        match &self.tooltip {
            Some(t) if Some(t.node) == target && t.window == w && t.at == at => {
                if !t.shown {
                    // Keep following the pointer until it shows.
                    let t = self.tooltip.as_mut().unwrap();
                    t.x = x;
                    t.y = y;
                }
            }
            _ => {
                self.hide_tooltip();
                if let Some(node) = target {
                    self.tooltip = Some(Tooltip { window: w, node, x, y, due: Instant::now() + TOOLTIP_DELAY, shown: false, at });
                }
            }
        }
    }

    /// Hide the tooltip (if shown) and forget a pending one.
    pub(crate) fn hide_tooltip(&mut self) {
        if let Some(t) = self.tooltip.take()
            && t.shown
        {
            if t.at.is_some() {
                self.events.push(Event::new(
                    "editor-hover",
                    vec![("window", Data::Num(t.window as f64)), ("node", Data::Num(t.node as f64)), ("line", Data::Nil)],
                ));
            } else {
                self.events.push(Event::new("tooltip", vec![("window", Data::Num(t.window as f64)), ("node", Data::Nil)]));
            }
        }
    }

    pub(crate) fn tooltip_wakeup(&self) -> Option<Instant> {
        self.tooltip.as_ref().filter(|t| !t.shown).map(|t| t.due)
    }

    pub(crate) fn tooltip_tick(&mut self, now: Instant) {
        let Some(t) = self.tooltip.clone() else { return };
        if t.shown || now < t.due {
            return;
        }
        if let Some((line, col)) = t.at {
            self.tooltip.as_mut().unwrap().shown = true;
            self.events.push(Event::new(
                "editor-hover",
                vec![
                    ("window", Data::Num(t.window as f64)),
                    ("node", Data::Num(t.node as f64)),
                    ("line", Data::Num(line as f64)),
                    ("col", Data::Num(col as f64)),
                    ("x", Data::Num(t.x as f64)),
                    ("y", Data::Num(t.y as f64)),
                ],
            ));
            return;
        }
        let Some((_, text)) = self.tooltip_at(t.node) else {
            self.tooltip = None;
            return;
        };
        self.tooltip.as_mut().unwrap().shown = true;
        self.events.push(Event::new(
            "tooltip",
            vec![
                ("window", Data::Num(t.window as f64)),
                ("node", Data::Num(t.node as f64)),
                ("text", Data::Str(text)),
                ("x", Data::Num(t.x as f64)),
                ("y", Data::Num(t.y as f64)),
            ],
        ));
    }

    /// A right click (or ctrl-click) released over `over`: a
    /// :context-menu event for the nearest node that asks for one.
    pub(crate) fn context_click(&mut self, w: WindowId, x: f32, y: f32, over: NodeId) -> bool {
        let Some(t) = self.target(over, "context-menu") else { return false };
        self.events.push(Event::new(
            "context-menu",
            vec![("window", Data::Num(w as f64)), ("node", Data::Num(t as f64)), ("x", Data::Num(x as f64)), ("y", Data::Num(y as f64))],
        ));
        true
    }
}
