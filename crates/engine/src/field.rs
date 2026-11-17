//! Text fields, edited in the engine (docs/ui-protocol.md §3): typing,
//! selection, the caret and its blink never wait on Skein. Skein gets
//! `:input` with the new value (coalesced) and `:submit` on Enter, and can
//! set `:value` at any time (a controlled field).
//!
//! The caret blinks only while the field is focused, its window has focus,
//! and the user has been active in the last ten seconds; then it stays on.
//! A blink repaints only the caret's rectangle.

use crate::data::{Data, Event, NodeId, Rect};
use crate::engine::Engine;
use crate::engine::Measure;
use crate::text::TextStyle;
use std::time::{Duration, Instant};

pub const BLINK: Duration = Duration::from_millis(530);
pub const BLINK_FOR: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Field {
    pub value: String,
    /// Character indexes of the caret and the selection's other end.
    pub caret: usize,
    pub anchor: usize,
    pub scroll_x: f32,
    pub caret_on: bool,
    pub active_at: Instant,
    pub toggled_at: Instant,
    /// The caret's rectangle, in window coordinates, from the last update.
    pub caret_rect: Rect,
}

impl Field {
    pub fn new(value: String) -> Field {
        let n = value.chars().count();
        let now = Instant::now();
        Field { value, caret: n, anchor: n, scroll_x: 0.0, caret_on: true, active_at: now, toggled_at: now, caret_rect: Rect::default() }
    }
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }
    fn byte(&self, idx: usize) -> usize {
        self.value.char_indices().nth(idx).map_or(self.value.len(), |(b, _)| b)
    }
    pub fn selected_text(&self) -> String {
        let (a, b) = self.selection();
        self.value[self.byte(a)..self.byte(b)].to_owned()
    }
    /// Replace the selection with `t`.
    fn insert(&mut self, t: &str) {
        let (a, b) = self.selection();
        let (ba, bb) = (self.byte(a), self.byte(b));
        self.value.replace_range(ba..bb, t);
        self.caret = a + t.chars().count();
        self.anchor = self.caret;
    }
    fn len(&self) -> usize {
        self.value.chars().count()
    }
}

/// The text style, resolved style and content box (padding inside the
/// border) of a field (the painter uses this too).
pub fn content_box(engine: &Engine, id: NodeId) -> (TextStyle, crate::data::Props, Rect) {
    content(engine, id)
}

pub(crate) fn content(engine: &Engine, id: NodeId) -> (TextStyle, crate::data::Props, Rect) {
    let n = &engine.nodes[&id];
    let lookup = |d: &Data| engine.theme.token(d).clone();
    let ts = TextStyle::from_props(&n.resolved, &lookup);
    let r = engine.rect(id).unwrap_or_default();
    let pad = n.resolved.iter().find(|(k, _)| k == "padding").map(|(_, v)| v.clone()).unwrap_or(Data::Nil);
    let (pt, pl) = match &pad {
        Data::Num(p) => (*p as f32, *p as f32),
        Data::List(xs) if xs.len() == 2 => (xs[0].as_num().unwrap_or(0.0) as f32, xs[1].as_num().unwrap_or(0.0) as f32),
        Data::List(xs) if xs.len() == 4 => (xs[0].as_num().unwrap_or(0.0) as f32, xs[3].as_num().unwrap_or(0.0) as f32),
        _ => (0.0, 0.0),
    };
    (ts, n.resolved.clone(), Rect { x: r.x + pl, y: r.y + pt, w: (r.w - 2.0 * pl).max(0.0), h: (r.h - 2.0 * pt).max(0.0) })
}

impl Engine {
    pub fn is_field(&self, id: NodeId) -> bool {
        self.nodes.get(&id).is_some_and(|n| n.kind == "text-field")
    }

    /// The focused text field, if any.
    pub fn focused_field(&self) -> Option<NodeId> {
        self.focused.filter(|f| self.is_field(*f))
    }

    /// After an edit, a caret move or a layout: keep the caret in view,
    /// recompute its rectangle, and repaint the field.
    pub(crate) fn update_field(&mut self, id: NodeId, m: &mut dyn Measure) {
        if self.place_caret(id, m).is_some() {
            self.repaint_node(id);
        }
    }

    /// After a layout: a field repaints only if its caret or scroll moved
    /// (if the field itself moved, the layout's damage has it already).
    pub(crate) fn update_field_after_layout(&mut self, id: NodeId, m: &mut dyn Measure) {
        if self.place_caret(id, m) == Some(true) {
            self.repaint_node(id);
        }
    }

    /// Keep field id's caret in its content box: whether that moved the
    /// caret or the scroll (None if it isn't a field).
    fn place_caret(&mut self, id: NodeId, m: &mut dyn Measure) -> Option<bool> {
        if !self.fields.contains_key(&id) {
            return None;
        }
        let (ts, props, cb) = content(self, id);
        let f = self.fields.get_mut(&id).unwrap();
        let before = (f.scroll_x, f.caret_rect);
        let x = m.caret_x(&f.value, &props, f.caret);
        if x - f.scroll_x > cb.w - 2.0 {
            f.scroll_x = x - cb.w + 2.0;
        } else if x - f.scroll_x < 0.0 {
            f.scroll_x = x;
        }
        let line_h = (ts.size * ts.line_height).ceil();
        f.caret_rect = Rect { x: (cb.x + x - f.scroll_x).floor(), y: cb.y + ((cb.h - line_h) / 2.0).max(0.0), w: 2.0, h: line_h };
        Some((f.scroll_x, f.caret_rect) != before)
    }

    fn activity(&mut self, id: NodeId) {
        if let Some(f) = self.fields.get_mut(&id) {
            let now = Instant::now();
            f.active_at = now;
            f.toggled_at = now;
            f.caret_on = true;
        }
    }

    fn changed(&mut self, id: NodeId) {
        let value = self.fields[&id].value.clone();
        if let Some(n) = self.nodes.get_mut(&id) {
            crate::style::merge(&mut n.props, &vec![("value".into(), Data::Str(value.clone()))]);
        }
        if self.nodes[&id].wants("input") {
            let me = Data::Num(id as f64);
            self.events.retain(|e| !(e.kind == "input" && e.get("node") == Some(&me)));
            self.events.push(Event::new("input", vec![("node", me), ("value", Data::Str(value))]));
        }
    }

    /// Typed or committed text: into the focused field, else an event.
    pub fn text_input(&mut self, w: u64, t: &str, m: &mut dyn Measure) {
        if let Some(id) = self.focused_field() {
            self.fields.get_mut(&id).unwrap().insert(t);
            self.activity(id);
            self.changed(id);
            self.update_field(id, m);
            return;
        }
        let mut fields = vec![("window", Data::Num(w as f64)), ("text", Data::Str(t.to_owned()))];
        if let Some(f) = self.focused {
            fields.push(("node", Data::Num(f as f64)));
        }
        self.events.push(Event::new("text", fields));
    }

    /// An editing key for the focused field: true if it was used.
    pub fn field_key(&mut self, key: &Data, mods: &[&str], m: &mut dyn Measure) -> bool {
        let Some(id) = self.focused_field() else { return false };
        let shift = mods.contains(&"shift");
        let word = mods.contains(&"cmd") || mods.contains(&"ctrl");
        let f = self.fields.get_mut(&id).unwrap();
        let n = f.len();
        let mut edited = false;
        match (key.as_key(), key.as_str()) {
            (Some("backspace"), _) => {
                if f.caret == f.anchor && f.caret > 0 {
                    f.anchor = if word { 0 } else { f.caret - 1 };
                }
                f.insert("");
                edited = true;
            }
            (Some("delete"), _) => {
                if f.caret == f.anchor && f.caret < n {
                    f.anchor = if word { n } else { f.caret + 1 };
                }
                f.insert("");
                edited = true;
            }
            (Some(k @ ("arrow-left" | "arrow-right" | "home" | "end")), _) => {
                let (a, b) = f.selection();
                f.caret = match k {
                    "arrow-left" if word => 0,
                    "arrow-right" if word => n,
                    "arrow-left" if !shift && a != b => a,
                    "arrow-right" if !shift && a != b => b,
                    "arrow-left" => f.caret.saturating_sub(1),
                    "arrow-right" => (f.caret + 1).min(n),
                    "home" => 0,
                    _ => n,
                };
                if !shift {
                    f.anchor = f.caret;
                }
            }
            (None, Some("a")) if word => {
                f.anchor = 0;
                f.caret = n;
            }
            (Some("enter"), _) => {
                let v = f.value.clone();
                if self.nodes[&id].wants("submit") {
                    self.events.push(Event::new("submit", vec![("node", Data::Num(id as f64)), ("value", Data::Str(v))]));
                }
                return true;
            }
            _ => return false,
        }
        self.activity(id);
        if edited {
            self.changed(id);
        }
        self.update_field(id, m);
        true
    }

    /// The selected text of the focused field (copy).
    /// The selected text to copy: the focused field's, else rich text's.
    pub fn copy_selection(&self) -> Option<String> {
        let Some(id) = self.focused_field() else { return self.rich_selection_text() };
        let s = self.fields.get(&id)?.selected_text();
        (!s.is_empty()).then_some(s)
    }

    /// Cut: the selected text, removed from the field.
    pub fn cut_selection(&mut self, m: &mut dyn Measure) -> Option<String> {
        let s = self.copy_selection()?;
        let id = self.focused_field()?;
        self.fields.get_mut(&id).unwrap().insert("");
        self.activity(id);
        self.changed(id);
        self.update_field(id, m);
        Some(s)
    }

    /// Paste (one line: newlines become spaces).
    pub fn paste(&mut self, s: &str, w: u64, m: &mut dyn Measure) {
        let one_line = s.replace(['\r', '\n'], " ");
        self.text_input(w, &one_line, m);
    }

    /// A press on a field focuses it and puts the caret under the pointer
    /// (shift extends the selection); a drag selects.
    pub(crate) fn field_pointer(&mut self, id: NodeId, x: f32, extend: bool, m: &mut dyn Measure) {
        if self.focused != Some(id) {
            self.set_focus(id);
        }
        let (_, props, cb) = content(self, id);
        let f = self.fields.get_mut(&id).unwrap();
        let idx = m.index_at(&f.value, &props, x - cb.x + f.scroll_x);
        f.caret = idx;
        if !extend {
            f.anchor = idx;
        }
        self.activity(id);
        self.update_field(id, m);
    }

    /// The next time the engine has something to do on a timer (a caret
    /// blink, a tooltip), if any.
    pub fn next_wakeup(&self) -> Option<Instant> {
        [self.blink_wakeup(), self.tooltip_wakeup(), self.anim_wakeup()].into_iter().flatten().min()
    }

    /// The next time the caret should blink, if it should.
    fn blink_wakeup(&self) -> Option<Instant> {
        let id = self.focused_field()?;
        if !self.window_focused(id) {
            return None;
        }
        let f = &self.fields[&id];
        let now = Instant::now();
        (now.duration_since(f.active_at) < BLINK_FOR).then(|| f.toggled_at + BLINK)
    }

    /// Advance timers to `now`: the caret blink (repainting only the
    /// caret) and a pending tooltip.
    pub fn tick(&mut self, now: Instant) {
        self.tooltip_tick(now);
        self.anim_tick(now);
        let Some(id) = self.focused_field() else { return };
        let focused = self.window_focused(id);
        let w = self.window_of(id);
        let f = self.fields.get_mut(&id).unwrap();
        let blinking = focused && now.duration_since(f.active_at) < BLINK_FOR;
        let want = if blinking { if now.duration_since(f.toggled_at) >= BLINK { !f.caret_on } else { f.caret_on } } else { true };
        if want != f.caret_on {
            f.caret_on = want;
            f.toggled_at = now;
            let r = f.caret_rect;
            if let Some(w) = w {
                self.windows.get_mut(&w).unwrap().damage.push(r);
            }
        }
    }
}
