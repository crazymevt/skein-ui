//! The display list: what a window shows, from the engine's tree, in paint
//! order (parents before children, later siblings on top), limited to the
//! nodes that touch the damage.

use norn_ui_engine::text::{GlyphRun, Rgba, TextStyle, color};
use norn_ui_engine::{Data, Engine, NodeId, Rect, Text, WindowId};

pub enum Item {
    Box { rect: Rect, color: Rgba, radius: f32, border: Option<(f32, Rgba)>, clip: Rect },
    Text { x: f32, y: f32, runs: Vec<GlyphRun>, clip: Rect },
    /// Triangles (a canvas's lines and paths), three points each, in
    /// window coordinates.
    Tris { verts: Vec<[f32; 2]>, color: Rgba, clip: Rect },
    /// An icon: SVG path data in a view-by-view box, stroked (width in view
    /// units) or filled, drawn into rect in color.
    Icon { rect: Rect, path: String, view: f32, stroke: Option<f32>, color: Rgba, clip: Rect },
}

impl Item {
    pub fn clip(&self) -> Rect {
        match self {
            Item::Box { clip, .. } | Item::Text { clip, .. } | Item::Tris { clip, .. } | Item::Icon { clip, .. } => *clip,
        }
    }
}

fn touches(r: &Rect, damage: &[Rect]) -> bool {
    damage.iter().any(|d| !r.intersect(d).is_empty())
}

pub fn display_list(engine: &Engine, w: WindowId, text: &mut Text, damage: &[Rect]) -> Vec<Item> {
    let mut out = Vec::new();
    let Some(win) = engine.windows.get(&w) else { return out };
    let lookup = |d: &Data| engine.theme.token(d).clone();
    let bg = win
        .props
        .iter()
        .find(|(k, _)| k == "background")
        .and_then(|(_, v)| color(&lookup(v)))
        .or_else(|| color(&lookup(&Data::key("bg"))))
        .unwrap_or([255, 255, 255, 255]);
    let full = Rect { x: 0.0, y: 0.0, w: win.size.0, h: win.size.1 };
    out.push(Item::Box { rect: full, color: bg, radius: 0.0, border: None, clip: full });
    if let Some(root) = win.root {
        walk(engine, win, root, text, damage, &lookup, &mut out);
    }
    out
}

fn walk(
    engine: &Engine,
    win: &norn_ui_engine::engine::Window,
    id: NodeId,
    text: &mut Text,
    damage: &[Rect],
    lookup: &dyn Fn(&Data) -> Data,
    out: &mut Vec<Item>,
) {
    let Some(r) = win.rects.get(&id) else { return };
    let clip = win.clips.get(&id).copied().unwrap_or(*r);
    let node = &engine.nodes[&id];
    if clip.intersect(r).is_empty() {
        return;
    }
    if touches(&r.intersect(&clip), damage) {
        let style = &node.resolved;
        let get = |k: &str| style.iter().find(|(x, _)| x == k).map(|(_, v)| lookup(v));
        let fill = get("background").and_then(|v| color(&v));
        let radius = get("border-radius").and_then(|v| v.as_num()).unwrap_or(0.0) as f32;
        let border = get("border").and_then(|v| match &v {
            Data::List(xs) if xs.len() == 2 => Some((xs[0].as_num()? as f32, color(&lookup(&xs[1]))?)),
            _ => None,
        });
        if fill.is_some() || border.is_some() {
            out.push(Item::Box { rect: *r, color: fill.unwrap_or([0, 0, 0, 0]), radius, border, clip });
        }
        if let Some(f) = engine.fields.get(&id) {
            field(engine, id, f, style, clip, text, lookup, out);
        } else if let Some(e) = engine.editors.get(&id) {
            editor(engine, id, e, *r, style, clip, text, damage, lookup, out);
        } else if node.kind == "icon" {
            if let Some(d) = node.prop("d").and_then(Data::as_str) {
                let size = node.prop("size").and_then(Data::as_num).unwrap_or(16.0) as f32;
                let view = node.prop("view").and_then(Data::as_num).unwrap_or(24.0) as f32;
                let stroke = node.prop("stroke").and_then(Data::as_num).map(|n| n as f32);
                let c = get("color").and_then(|v| color(&v)).unwrap_or([0, 0, 0, 255]);
                let rect = Rect { x: r.x + ((r.w - size) / 2.0).max(0.0), y: r.y + ((r.h - size) / 2.0).max(0.0), w: size, h: size };
                out.push(Item::Icon { rect, path: d.to_owned(), view, stroke, color: c, clip });
            }
        } else if node.kind == "rich-text" {
            rich(engine, id, *r, style, clip, text, lookup, out);
        } else if node.kind == "canvas" {
            let cmds = node.prop("commands").and_then(Data::as_list).unwrap_or(&[]);
            crate::canvas::canvas(cmds, *r, clip, text, lookup, out);
        } else if !node.text().is_empty() {
            let content = node.text();
            let ts = TextStyle::from_props(style, lookup);
            let wrap = if norn_ui_engine::style::no_wrap(style) { None } else { Some(r.w + 0.5) };
            let layout = text.layout(content, &ts, wrap);
            out.push(Item::Text { x: r.x, y: r.y, runs: Text::glyph_runs(&layout), clip });
        }
    }
    for c in &node.children {
        walk(engine, win, *c, text, damage, lookup, out);
    }
    // Scroll bars, over the content.
    if touches(&r.intersect(&clip), damage) {
        let thumb = named_color("muted", lookup).map(|[r, g, b, _]| [r, g, b, 110]).unwrap_or([140, 140, 150, 110]);
        let clip = clip.intersect(r);
        for t in engine.scroll_thumbs(win, id) {
            out.push(Item::Box { rect: t, color: thumb, radius: 3.0, border: None, clip });
        }
    }
}

/// A text field: its value (or placeholder) scrolled to the caret, the
/// selection, and the caret while it is on.
#[allow(clippy::too_many_arguments)]
fn field(
    engine: &Engine,
    id: NodeId,
    f: &norn_ui_engine::field::Field,
    style: &norn_ui_engine::Props,
    clip: Rect,
    text: &mut Text,
    lookup: &dyn Fn(&Data) -> Data,
    out: &mut Vec<Item>,
) {
    let (ts, _, cb) = norn_ui_engine::field::content_box(engine, id);
    let clip = clip.intersect(&cb);
    let focused = engine.focused == Some(id);
    let line_h = (ts.size * ts.line_height).ceil();
    let y = cb.y + ((cb.h - line_h) / 2.0).max(0.0);
    let (a, b) = f.selection();
    if focused && a != b {
        let x0 = text.caret_x(&f.value, &ts, a);
        let x1 = text.caret_x(&f.value, &ts, b);
        let sel = style.iter().find(|(k, _)| k == "selection-color").and_then(|(_, v)| color(&lookup(v))).unwrap_or([180, 210, 255, 255]);
        out.push(Item::Box { rect: Rect { x: cb.x + x0 - f.scroll_x, y, w: x1 - x0, h: line_h }, color: sel, radius: 0.0, border: None, clip });
    }
    if let Some(pre) = engine.preedit_in(id) {
        // Composing: the preedit at the caret, underlined, and the caret
        // where the input method puts it.
        let at = f.value.char_indices().nth(f.caret).map_or(f.value.len(), |(b, _)| b);
        let shown = format!("{}{}{}", &f.value[..at], pre.text, &f.value[at..]);
        let n = pre.text.chars().count();
        let layout = text.layout(&shown, &ts, None);
        out.push(Item::Text { x: cb.x - f.scroll_x, y, runs: Text::glyph_runs(&layout), clip });
        let x0 = text.caret_x(&shown, &ts, f.caret);
        let x1 = text.caret_x(&shown, &ts, f.caret + n);
        out.push(Item::Box { rect: Rect { x: cb.x + x0 - f.scroll_x, y: y + line_h - 1.5, w: x1 - x0, h: 1.0 }, color: ts.color, radius: 0.0, border: None, clip });
        if focused {
            let x = text.caret_x(&shown, &ts, f.caret + pre.cursor.unwrap_or(n));
            out.push(Item::Box { rect: Rect { x: (cb.x + x - f.scroll_x).floor(), y, w: 2.0, h: line_h }, color: ts.color, radius: 0.0, border: None, clip });
        }
        return;
    }
    if f.value.is_empty() {
        if let Some(p) = engine.nodes[&id].prop("placeholder").and_then(Data::as_str) {
            let mut pts = ts.clone();
            pts.color[3] = 110;
            let layout = text.layout(p, &pts, None);
            out.push(Item::Text { x: cb.x, y, runs: Text::glyph_runs(&layout), clip });
        }
    } else {
        let layout = text.layout(&f.value, &ts, None);
        out.push(Item::Text { x: cb.x - f.scroll_x, y, runs: Text::glyph_runs(&layout), clip });
    }
    if focused && f.caret_on {
        let x = text.caret_x(&f.value, &ts, f.caret);
        let caret = style.iter().find(|(k, _)| k == "caret-color").and_then(|(_, v)| color(&lookup(v))).unwrap_or(ts.color);
        out.push(Item::Box { rect: Rect { x: (cb.x + x - f.scroll_x).floor(), y, w: 2.0, h: line_h }, color: caret, radius: 0.0, border: None, clip });
    }
}

/// A rich-text node: span backgrounds, the selection, the text, and its
/// underlines and strike-throughs.
#[allow(clippy::too_many_arguments)]
fn rich(
    engine: &Engine,
    id: NodeId,
    r: Rect,
    style: &norn_ui_engine::Props,
    clip: Rect,
    text: &mut Text,
    lookup: &dyn Fn(&Data) -> Data,
    out: &mut Vec<Item>,
) {
    let spans = norn_ui_engine::text::spans_of(engine.nodes[&id].prop("spans"));
    if spans.is_empty() {
        return;
    }
    let ts = TextStyle::from_props(style, lookup);
    let layout = text.layout_rich(&spans, &ts, Some(r.w + 0.5), lookup);
    let boxes = |a: usize, b: usize, color, radius: f32, out: &mut Vec<Item>| {
        for (x, y, w, h) in Text::rich_rects(&layout, a, b) {
            out.push(Item::Box { rect: Rect { x: r.x + x, y: r.y + y, w, h }, color, radius, border: None, clip });
        }
    };
    let mut at = 0;
    for s in &spans {
        let bg = s.style.iter().find(|(k, _)| k == "background").and_then(|(_, v)| color(&lookup(v)));
        if let Some(c) = bg {
            boxes(at, at + s.text.len(), c, 3.0, out);
        }
        at += s.text.len();
    }
    if let Some((a, b)) = engine.rich_selection_in(id) {
        let sel = named_color("selection", lookup).unwrap_or([200, 220, 255, 255]);
        boxes(a, b, sel, 0.0, out);
    }
    let runs = Text::glyph_runs(&layout);
    let lines: Vec<Item> = runs
        .iter()
        .flat_map(|run| {
            run.lines.iter().map(move |&(x, y, w, h)| Item::Box {
                rect: Rect { x: r.x + x, y: r.y + y, w, h },
                color: run.color,
                radius: 0.0,
                border: None,
                clip,
            })
        })
        .collect();
    out.push(Item::Text { x: r.x, y: r.y, runs, clip });
    out.extend(lines);
}

/// A color by name: a theme token, a color value, or None.
fn named_color(name: &str, lookup: &dyn Fn(&Data) -> Data) -> Option<norn_ui_engine::text::Rgba> {
    color(&lookup(&Data::key(name))).or_else(|| color(&Data::str(name)))
}

/// An editor surface: gutter and line numbers, the current line, selected
/// text, highlighted lines (span styles are theme tokens), decorations and
/// carets. Only lines that touch the damage are built.
#[allow(clippy::too_many_arguments)]
fn editor(
    engine: &Engine,
    id: NodeId,
    e: &norn_ui_engine::editor::Editor,
    r: Rect,
    style: &norn_ui_engine::Props,
    clip: Rect,
    text: &mut Text,
    damage: &[Rect],
    lookup: &dyn Fn(&Data) -> Data,
    out: &mut Vec<Item>,
) {
    let clip = clip.intersect(&r);
    let ts = TextStyle::from_props(style, lookup);
    let muted = named_color("muted", lookup).unwrap_or([140, 140, 150, 255]);
    let gutter_bg = named_color("gutter", lookup).unwrap_or([246, 246, 248, 255]);
    let current = named_color("current-line", lookup).unwrap_or([242, 245, 252, 255]);
    let selection = named_color("selection", lookup).unwrap_or([200, 220, 255, 255]);
    let caret_color = named_color("caret", lookup).unwrap_or(ts.color);
    let focused = engine.focused == Some(id);
    let (first, last) = e.visible(r.h);
    // The text scrolls sideways under its own clip; the gutter stays.
    let text_x = r.x + e.gutter_w + 4.0 - e.scroll_x;
    let gclip = clip;
    let clip = clip.intersect(&Rect { x: r.x + e.gutter_w, y: r.y, w: (r.w - e.gutter_w).max(0.0), h: r.h });
    out.push(Item::Box { rect: Rect { x: r.x, y: r.y, w: e.gutter_w, h: r.h }, color: gutter_bg, radius: 0.0, border: None, clip: gclip });
    for line in first..last {
        let y = r.y + line as f32 * e.line_h - e.scroll_y;
        let row = Rect { x: r.x, y, w: r.w, h: e.line_h };
        if !touches(&row.intersect(&gclip), damage) {
            continue;
        }
        if focused && e.carets.iter().any(|c| c.line == line && c.anchor.is_none()) {
            out.push(Item::Box { rect: Rect { x: r.x + e.gutter_w, y, w: r.w - e.gutter_w, h: e.line_h }, color: current, radius: 0.0, border: None, clip });
        }
        let Some(l) = e.lines.get(&line) else { continue };
        let len = l.text.chars().count();
        for c in &e.carets {
            if let Some((al, ac)) = c.anchor {
                let (s, t) = if (al, ac) <= (c.line, c.col) { ((al, ac), (c.line, c.col)) } else { ((c.line, c.col), (al, ac)) };
                if line < s.0 || line > t.0 {
                    continue;
                }
                let a = if line == s.0 { s.1 } else { 0 };
                let (xa, xb) = (col_x(text, &ts, e.col_w, &l.text, a.min(len)), if line == t.0 { col_x(text, &ts, e.col_w, &l.text, t.1.min(len)) } else { col_x(text, &ts, e.col_w, &l.text, len) + e.col_w });
                out.push(Item::Box {
                    rect: Rect { x: text_x + xa, y, w: (xb - xa).max(0.0), h: e.line_h },
                    color: selection,
                    radius: 0.0,
                    border: None,
                    clip,
                });
            }
        }
        let pad = ((e.line_h - (ts.size * ts.line_height)) / 2.0).max(0.0);
        if e.gutter_w > 0.0 {
            let num_text = (line + 1).to_string();
            let mut nts = ts.clone();
            nts.color = muted;
            let nl = text.layout(&num_text, &nts, None);
            let nx = r.x + e.gutter_w - e.col_w - num_text.len() as f32 * e.col_w;
            out.push(Item::Text { x: nx, y: y + pad, runs: Text::glyph_runs(&nl), clip: gclip });
        }
        let mut spans: Vec<(usize, usize, norn_ui_engine::text::Rgba)> =
            l.spans.iter().filter_map(|(a, b, k)| named_color(k, lookup).map(|c| (*a, *b, c))).collect();
        // An input method composing at the primary caret: its text goes in
        // the line, underlined, pushing the rest along.
        let primary = e.carets.last().filter(|c| c.line == line);
        let pre = engine.preedit_in(id).zip(primary);
        let line_text = match pre {
            Some((p, c)) => {
                let col = c.col.min(len);
                let n = p.text.chars().count();
                let at = l.text.char_indices().nth(col).map_or(l.text.len(), |(b, _)| b);
                for s in spans.iter_mut() {
                    if s.0 >= col {
                        s.0 += n;
                    }
                    if s.1 > col {
                        s.1 += n;
                    }
                }
                let shown = format!("{}{}{}", &l.text[..at], p.text, &l.text[at..]);
                let (x0, x1) = (col_x(text, &ts, e.col_w, &shown, col), col_x(text, &ts, e.col_w, &shown, col + n));
                out.push(Item::Box {
                    rect: Rect { x: text_x + x0, y: y + e.line_h - 2.0, w: x1 - x0, h: 1.0 },
                    color: ts.color,
                    radius: 0.0,
                    border: None,
                    clip,
                });
                shown
            }
            None => l.text.clone(),
        };
        let layout = text.layout_spans(&line_text, &ts, &spans);
        out.push(Item::Text { x: text_x, y: y + pad, runs: Text::glyph_runs(&layout), clip });
        for d in e.decorations.iter().filter(|d| d.line == line) {
            let c = named_color(&d.style, lookup).unwrap_or(match d.style.as_str() {
                "warning" => [210, 150, 30, 255],
                "info" => [60, 120, 220, 255],
                _ => [220, 50, 50, 255],
            });
            let (a, b) = (d.from.min(len), d.to.min(len).max(d.from.min(len) + 1));
            let xa = col_x(text, &ts, e.col_w, &l.text, a);
            let xb = if b > len { col_x(text, &ts, e.col_w, &l.text, len) + e.col_w } else { col_x(text, &ts, e.col_w, &l.text, b) };
            match d.kind.as_str() {
                "inline" => {
                    if let Some(t) = &d.text {
                        let mut its = ts.clone();
                        its.color = c;
                        let il = text.layout(t, &its, None);
                        let end = col_x(text, &ts, e.col_w, &l.text, len);
                        out.push(Item::Text { x: text_x + end + 2.0 * e.col_w, y: y + pad, runs: Text::glyph_runs(&il), clip });
                    }
                }
                "highlight" => out.push(Item::Box {
                    rect: Rect { x: text_x + xa, y, w: xb - xa, h: e.line_h },
                    color: [c[0], c[1], c[2], 60],
                    radius: 2.0,
                    border: None,
                    clip,
                }),
                _ => out.push(Item::Box {
                    rect: Rect { x: text_x + xa, y: y + e.line_h - 3.0, w: xb - xa, h: 2.0 },
                    color: c,
                    radius: 1.0,
                    border: None,
                    clip,
                }),
            }
        }
        if focused {
            for c in e.carets.iter().filter(|c| c.line == line) {
                let shift = match pre {
                    Some((p, pc)) if std::ptr::eq(pc, c) => p.cursor.unwrap_or(p.text.chars().count()),
                    _ => 0,
                };
                out.push(Item::Box {
                    rect: Rect { x: (text_x + col_x(text, &ts, e.col_w, &line_text, c.col + shift)).floor(), y, w: 2.0, h: e.line_h },
                    color: caret_color,
                    radius: 0.0,
                    border: None,
                    clip,
                });
            }
        }
    }
    // A scroll bar, when the text is taller than the view.
    let total = e.line_count as f32 * e.line_h;
    if total > r.h + 1.0 && r.h > 20.0 {
        let th = (r.h * r.h / total).clamp(24.0, r.h);
        let at = (e.scroll_y / (total - r.h)).clamp(0.0, 1.0);
        let thumb = Rect { x: r.x + r.w - 8.0, y: r.y + at * (r.h - th), w: 6.0, h: th };
        if touches(&thumb, damage) || touches(&r, damage) {
            let c = named_color("muted", lookup).map(|[r, g, b, _]| [r, g, b, 110]).unwrap_or([140, 140, 150, 110]);
            out.push(Item::Box { rect: thumb, color: c, radius: 3.0, border: None, clip: gclip });
        }
    }
}

/// The x of column c in one line of editor text: c cells for ASCII, else
/// measured (wide characters take more than a cell).
fn col_x(text: &mut Text, ts: &TextStyle, col_w: f32, line: &str, c: usize) -> f32 {
    if line.is_ascii() { c as f32 * col_w } else { text.caret_x(line, ts, c) }
}
