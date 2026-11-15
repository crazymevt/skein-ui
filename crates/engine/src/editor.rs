//! The code-editor surface (docs/ui-protocol.md §6). The text lives in Norn;
//! the engine shows a viewport onto it:
//!
//! - it caches the lines it has been sent, and asks for the ones scrolling
//!   brings into view (`:need-lines`), once per gap;
//! - it scrolls itself (no Norn on the frame path), draws the gutter, the
//!   lines with their colored spans, selections, decorations and carets;
//! - `[:lines id from [...]]` repaints exactly those lines, so a keystroke
//!   repaints one line.
//!
//! Lines are monospaced (the code font): a column is a fixed advance.

use crate::data::{Data, Event, NodeId, Rect};
use crate::engine::{Engine, Measure};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    /// (start, end, style key) in characters.
    pub spans: Vec<(usize, usize, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Caret {
    pub line: usize,
    pub col: usize,
    pub anchor: Option<(usize, usize)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Decoration {
    pub line: usize,
    pub from: usize,
    pub to: usize,
    /// :squiggle, :highlight or :inline (with text after the line).
    pub kind: String,
    pub style: String,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Editor {
    pub line_count: usize,
    pub lines: HashMap<usize, Line>,
    pub carets: Vec<Caret>,
    pub decorations: Vec<Decoration>,
    pub scroll_y: f32,
    /// Horizontal scroll of the text (the gutter stays put).
    pub scroll_x: f32,
    /// The range asked for and not yet answered.
    pub requested: Option<(usize, usize)>,
    /// Metrics from the last layout: line height, column width, gutter width.
    pub line_h: f32,
    pub col_w: f32,
    pub gutter_w: f32,
    /// A reveal asked for before the editor had a size: done after layout.
    pub pending_reveal: Option<(usize, Option<usize>)>,
}

fn num(d: Option<&Data>) -> usize {
    d.and_then(Data::as_num).unwrap_or(0.0).max(0.0) as usize
}

impl Editor {
    pub fn new() -> Editor {
        Editor { line_count: 1, line_h: 18.0, col_w: 8.0, gutter_w: 40.0, ..Default::default() }
    }

    /// Lines [first, last) in view for a viewport of height h.
    pub fn visible(&self, h: f32) -> (usize, usize) {
        let first = (self.scroll_y / self.line_h).floor().max(0.0) as usize;
        let last = (((self.scroll_y + h) / self.line_h).ceil() as usize + 1).min(self.line_count);
        (first.min(self.line_count), last)
    }
}

impl Engine {
    pub fn is_editor(&self, id: NodeId) -> bool {
        self.nodes.get(&id).is_some_and(|n| n.kind == "editor")
    }

    /// The rectangle of line `line` of editor `id`, in window coordinates.
    pub fn editor_line_rect(&self, id: NodeId, line: usize) -> Option<Rect> {
        let e = self.editors.get(&id)?;
        let r = self.rect(id)?;
        let y = r.y + line as f32 * e.line_h - e.scroll_y;
        Some(Rect { x: r.x, y, w: r.w, h: e.line_h }.intersect(&r))
    }

    /// The widest cached line's width, for the horizontal scroll range.
    fn editor_max_x(&self, id: NodeId) -> f32 {
        let (Some(e), Some(r)) = (self.editors.get(&id), self.rect(id)) else { return 0.0 };
        let widest = e.lines.values().map(|l| l.text.chars().count()).max().unwrap_or(0) as f32 * e.col_w;
        (widest + 4.0 * e.col_w - (r.w - e.gutter_w - 4.0)).max(0.0)
    }

    fn damage_lines(&mut self, id: NodeId, lines: impl IntoIterator<Item = usize>) {
        let Some(w) = self.window_of(id) else { return };
        let rects: Vec<Rect> = lines.into_iter().filter_map(|l| self.editor_line_rect(id, l)).filter(|r| !r.is_empty()).collect();
        self.windows.get_mut(&w).unwrap().damage.extend(rects);
    }

    pub(crate) fn damage_editor(&mut self, id: NodeId) {
        if let (Some(w), Some(r)) = (self.window_of(id), self.rect(id)) {
            self.windows.get_mut(&w).unwrap().damage.push(r);
        }
    }

    pub(crate) fn editor_op(&mut self, id: NodeId, op: &str, args: &[Data]) {
        let Some(e) = self.editors.get_mut(&id) else { return };
        match op {
            "line-count" => {
                let n = num(args.first()).max(1);
                let old = e.line_count;
                e.line_count = n;
                e.lines.retain(|l, _| *l < n);
                if n != old {
                    // Lines past the shorter end changed (appeared or went).
                    let from = old.min(n);
                    let to = old.max(n);
                    self.damage_lines(id, from..to);
                }
            }
            "lines" => {
                let from = num(args.first());
                let mut changed = Vec::new();
                if let Some(Data::List(ls)) = args.get(1) {
                    for (i, l) in ls.iter().enumerate() {
                        let line = parse_line(l);
                        let n = from + i;
                        if e.lines.get(&n) != Some(&line) {
                            e.lines.insert(n, line);
                            changed.push(n);
                        }
                    }
                }
                if e.requested.is_some_and(|(a, b)| from <= a && from + changed.len().max(1) >= b.min(from + 1)) {
                    e.requested = None;
                }
                e.requested = None;
                self.damage_lines(id, changed);
            }
            "carets" => {
                let new: Vec<Caret> = match args.first() {
                    Some(Data::List(cs)) => cs.iter().map(parse_caret).collect(),
                    _ => Vec::new(),
                };
                let touched: Vec<usize> = e.carets.iter().chain(new.iter()).flat_map(caret_lines).collect();
                e.carets = new;
                self.damage_lines(id, touched);
            }
            "decorations" => {
                let new: Vec<Decoration> = match args.first() {
                    Some(Data::List(ds)) => ds.iter().map(parse_decoration).collect(),
                    _ => Vec::new(),
                };
                let touched: Vec<usize> = e.decorations.iter().chain(new.iter()).map(|d| d.line).collect();
                e.decorations = new;
                self.damage_lines(id, touched);
            }
            "reveal" => {
                // [:reveal id line col?]: scroll so the line (and column)
                // is in view.
                let line = num(args.first());
                let col = args.get(1).and_then(Data::as_num).map(|c| c.max(0.0) as usize);
                self.reveal(id, line, col);
            }
            "invalidate" => {
                e.lines.clear();
                e.requested = None;
                self.damage_editor(id);
            }
            _ => {}
        }
    }

    /// Scroll editor id so line (and col) is in view. Before the editor
    /// has a size there's no view to scroll it into: that waits for the
    /// layout (else it would scroll as if the view were a line short).
    fn reveal(&mut self, id: NodeId, line: usize, col: Option<usize>) {
        let (h, w) = self.rect(id).map_or((0.0, 0.0), |r| (r.h, r.w));
        let e = self.editors.get_mut(&id).unwrap();
        if h <= 0.0 {
            e.pending_reveal = Some((line, col));
            return;
        }
        let top = line as f32 * e.line_h;
        let before = (e.scroll_y, e.scroll_x);
        if top < e.scroll_y {
            e.scroll_y = top;
        } else if top + e.line_h > e.scroll_y + h {
            e.scroll_y = top + e.line_h - h;
        }
        if let Some(col) = col {
            let view = (w - e.gutter_w - 4.0).max(e.col_w);
            let x = col as f32 * e.col_w;
            if x < e.scroll_x {
                e.scroll_x = (x - 4.0 * e.col_w).max(0.0);
            } else if x + 2.0 * e.col_w > e.scroll_x + view {
                e.scroll_x = x + 4.0 * e.col_w - view;
            }
        }
        if (e.scroll_y, e.scroll_x) != before {
            self.damage_editor(id);
        }
    }

    /// After layout, scrolling or an answer: metrics, and a :need-lines
    /// request for lines in view that aren't cached (one at a time).
    pub(crate) fn update_editor(&mut self, id: NodeId, m: &mut dyn Measure) {
        let Some(r) = self.rect(id) else { return };
        let props = self.nodes[&id].resolved.clone();
        let size = props.iter().find(|(k, _)| k == "font-size").and_then(|(_, v)| v.as_num()).unwrap_or(13.0) as f32;
        let lh = props.iter().find(|(k, _)| k == "line-height").and_then(|(_, v)| v.as_num()).unwrap_or(1.45) as f32;
        let col_w = m.caret_x("0000000000", &props, 10) / 10.0;
        let e = self.editors.get_mut(&id).unwrap();
        e.line_h = (size * lh).round().max(1.0);
        e.col_w = if col_w > 0.0 { col_w } else { size * 0.6 };
        let digits = e.line_count.max(1).to_string().len().max(2) as f32;
        // :gutter false: no line numbers (a REPL's input)
        let gutter = !self.nodes[&id].props.iter().any(|(k, v)| k == "gutter" && matches!(v, Data::Bool(false)));
        e.gutter_w = if gutter { (digits * e.col_w + 2.0 * e.col_w).ceil() } else { 0.0 };
        if r.h > 0.0
            && let Some((line, col)) = e.pending_reveal.take()
        {
            self.reveal(id, line, col);
        }
        let e = self.editors.get_mut(&id).unwrap();
        let (first, last) = e.visible(r.h);
        if e.requested.is_some() {
            return;
        }
        let missing: Vec<usize> = (first..last).filter(|l| !e.lines.contains_key(l)).collect();
        if let (Some(&a), Some(&b)) = (missing.first(), missing.last()) {
            e.requested = Some((a, b + 1));
            // Keep the cache near the view.
            let keep = (first.saturating_sub(200), last + 200);
            e.lines.retain(|l, _| *l >= keep.0 && *l < keep.1);
            self.events.push(Event::new(
                "need-lines",
                vec![("node", Data::Num(id as f64)), ("from", Data::Num(a as f64)), ("to", Data::Num((b + 1) as f64))],
            ));
        }
    }

    /// The wheel over an editor scrolls it (no layout, no Norn): true if
    /// it moved.
    pub(crate) fn editor_wheel(&mut self, id: NodeId, dx: f32, dy: f32, m: &mut dyn Measure) -> bool {
        let h = self.rect(id).map_or(0.0, |r| r.h);
        let max_x = self.editor_max_x(id);
        let e = self.editors.get_mut(&id).unwrap();
        let max = (e.line_count as f32 * e.line_h - h + e.line_h * 2.0).max(0.0);
        let y = (e.scroll_y + dy).clamp(0.0, max);
        let x = (e.scroll_x + dx).clamp(0.0, max_x.max(e.scroll_x));
        if y == e.scroll_y && x == e.scroll_x {
            return false;
        }
        e.scroll_y = y;
        e.scroll_x = x;
        self.damage_editor(id);
        self.update_editor(id, m);
        true
    }

    /// (line, column) at window point (x, y) in editor id. Columns are
    /// characters, placed by measuring the line (wide characters such as
    /// CJK take more than one cell).
    pub fn editor_position(&self, id: NodeId, x: f32, y: f32, m: &mut dyn Measure) -> (usize, usize) {
        let (Some(e), Some(r)) = (self.editors.get(&id), self.rect(id)) else { return (0, 0) };
        let line = (((y - r.y + e.scroll_y) / e.line_h).floor().max(0.0) as usize).min(e.line_count.saturating_sub(1));
        let dx = x - r.x - e.gutter_w - 4.0 + e.scroll_x;
        match e.lines.get(&line) {
            Some(l) => (line, m.index_at(&l.text, &self.nodes[&id].resolved, dx.max(0.0)).min(l.text.chars().count())),
            None => (line, (dx / e.col_w).round().max(0.0) as usize),
        }
    }

    /// The x of column `col` of `line` from the text's left edge (before
    /// horizontal scrolling).
    pub fn editor_col_x(&self, id: NodeId, line: usize, col: usize, m: &mut dyn Measure) -> f32 {
        let Some(e) = self.editors.get(&id) else { return 0.0 };
        match e.lines.get(&line) {
            Some(l) if !l.text.is_ascii() => m.caret_x(&l.text, &self.nodes[&id].resolved, col),
            _ => col as f32 * e.col_w,
        }
    }
}

fn caret_lines(c: &Caret) -> Vec<usize> {
    match c.anchor {
        Some((al, _)) => (c.line.min(al)..=c.line.max(al)).collect(),
        None => vec![c.line],
    }
}

fn parse_line(d: &Data) -> Line {
    match d {
        Data::Str(s) => Line { text: s.clone(), spans: Vec::new() },
        Data::List(xs) => {
            let text = xs.first().and_then(Data::as_str).unwrap_or("").to_owned();
            let spans = match xs.get(1) {
                Some(Data::List(ss)) => ss
                    .iter()
                    .filter_map(|s| {
                        let p = s.as_list()?;
                        Some((num(p.first()), num(p.get(1)), p.get(2).and_then(|k| k.as_key().or(k.as_str())).unwrap_or("").to_owned()))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Line { text, spans }
        }
        _ => Line { text: String::new(), spans: Vec::new() },
    }
}

fn parse_caret(d: &Data) -> Caret {
    let line = num(d.get("line"));
    let col = num(d.get("col"));
    let anchor = d.get("anchor").and_then(Data::as_list).map(|a| (num(a.first()), num(a.get(1))));
    Caret { line, col, anchor }
}

fn parse_decoration(d: &Data) -> Decoration {
    Decoration {
        line: num(d.get("line")),
        from: num(d.get("from")),
        to: num(d.get("to")),
        kind: d.get("kind").and_then(Data::as_key).unwrap_or("squiggle").to_owned(),
        style: d.get("style").and_then(Data::as_key).unwrap_or("error").to_owned(),
        text: d.get("text").and_then(Data::as_str).map(str::to_owned),
    }
}
