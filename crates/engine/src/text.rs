//! Text: shaping and line layout with parley (font fallback, bidi, line
//! breaking), the bundled fonts, and glyph runs for the painter.
//!
//! The UI font is Inter and the code font JetBrains Mono, both bundled (SIL
//! Open Font License, fonts/), so text looks and measures the same on every
//! machine; system fonts are the fallback for scripts they don't cover.

use crate::data::{Data, Props};
use crate::engine::{Measure, Node};
use parley::{FontContext, FontFamily, FontStyle, FontWeight, Layout, LayoutContext, LineHeight, PositionedLayoutItem, StyleProperty};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

static INTER: &[u8] = include_bytes!("../../../fonts/Inter.ttf");
static INTER_ITALIC: &[u8] = include_bytes!("../../../fonts/Inter-Italic.ttf");
static MONO: &[u8] = include_bytes!("../../../fonts/JetBrainsMono.ttf");
static MONO_ITALIC: &[u8] = include_bytes!("../../../fonts/JetBrainsMono-Italic.ttf");

/// An RGBA color, the brush text is drawn with.
pub type Rgba = [u8; 4];

/// The text properties of a style map.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub family: String,
    pub size: f32,
    pub weight: f32,
    pub italic: bool,
    /// A multiple of the font size.
    pub line_height: f32,
    pub color: Rgba,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle { family: "ui".into(), size: 14.0, weight: 400.0, italic: false, line_height: 1.3, color: [0, 0, 0, 255] }
    }
}

/// A color from style data: "#rrggbb", "#rrggbbaa" or [r g b a].
pub fn color(d: &Data) -> Option<Rgba> {
    match d {
        Data::Str(s) if s.starts_with('#') && (s.len() == 7 || s.len() == 9) => {
            let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
            Some([byte(1)?, byte(3)?, byte(5)?, if s.len() == 9 { byte(7)? } else { 255 }])
        }
        Data::List(xs) if xs.len() >= 3 => {
            let c = |i: usize| xs.get(i).and_then(Data::as_num).map(|n| n.clamp(0.0, 255.0) as u8);
            Some([c(0)?, c(1)?, c(2)?, if xs.len() > 3 { c(3)? } else { 255 }])
        }
        _ => None,
    }
}

impl TextStyle {
    /// The text style of a resolved style map (theme tokens already looked
    /// up by `lookup`).
    pub fn from_props(style: &Props, lookup: &dyn Fn(&Data) -> Data) -> TextStyle {
        let mut t = TextStyle::default();
        for (k, v) in style {
            let v = lookup(v);
            match k.as_str() {
                "font-family" => {
                    if let Some(f) = v.as_str().or(v.as_key()) {
                        t.family = f.to_owned();
                    }
                }
                "font-size" => t.size = v.as_num().unwrap_or(14.0) as f32,
                "font-weight" => {
                    t.weight = match &v {
                        Data::Num(n) => *n as f32,
                        Data::Key(k) if k == "bold" => 700.0,
                        Data::Key(k) if k == "medium" => 500.0,
                        Data::Key(k) if k == "light" => 300.0,
                        _ => 400.0,
                    }
                }
                "font-style" => t.italic = v.as_key() == Some("italic"),
                "line-height" => t.line_height = v.as_num().unwrap_or(1.3) as f32,
                "color" => {
                    if let Some(c) = color(&v) {
                        t.color = c;
                    }
                }
                _ => {}
            }
        }
        t
    }
}

/// One positioned glyph run: everything the painter needs.
pub struct GlyphRun {
    pub font: parley::FontData,
    pub size: f32,
    pub coords: Vec<i16>,
    pub color: Rgba,
    /// (glyph id, x, y) relative to the text's top left; y is the baseline.
    pub glyphs: Vec<(u32, f32, f32)>,
    /// Underlines and strike-throughs: (x, y, w, h) relative to the text's
    /// top left, in the run's colour.
    pub lines: Vec<(f32, f32, f32, f32)>,
}

/// One span of rich text: its text and the style it changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Props,
}

impl Span {
    fn get(&self, k: &str) -> Option<&Data> {
        self.style.iter().find(|(x, _)| x == k).map(|(_, v)| v)
    }
    /// The link this span is part of, if any.
    pub fn link(&self) -> Option<&Data> {
        self.get("link").filter(|d| d.truthy())
    }
}

/// The spans of a rich-text node's :spans: each a string, or [text style].
pub fn spans_of(d: Option<&Data>) -> Vec<Span> {
    let Some(Data::List(xs)) = d else { return Vec::new() };
    xs.iter()
        .filter_map(|x| match x {
            Data::Str(t) => Some(Span { text: t.clone(), style: Vec::new() }),
            Data::List(p) => Some(Span {
                text: p.first().and_then(Data::as_str).unwrap_or("").to_owned(),
                style: p.get(1).and_then(|m| crate::style::props_of(m).ok()).unwrap_or_default(),
            }),
            _ => None,
        })
        .collect()
}

/// The whole text of some spans.
pub fn spans_text(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// The span holding byte `b` of the spans' text.
pub fn span_at(spans: &[Span], b: usize) -> Option<&Span> {
    let mut at = 0;
    for s in spans {
        if b < at + s.text.len() {
            return Some(s);
        }
        at += s.text.len();
    }
    None
}

pub struct Text {
    fonts: FontContext,
    layouts: LayoutContext<Rgba>,
    ui_family: String,
    mono_family: String,
    cache: HashMap<u64, (f32, f32)>,
    /// Shaped layouts for painting, by text, style and width: shaping is
    /// the costly part, and scrolling repaints the same text again and
    /// again.
    laid: HashMap<u64, Layout<Rgba>>,
}

impl Default for Text {
    fn default() -> Self {
        Text::new()
    }
}

impl Text {
    pub fn new() -> Text {
        let mut fonts = FontContext::new();
        let mut register = |bytes: &'static [u8]| -> Option<String> {
            let blob = parley::fontique::Blob::new(Arc::new(bytes));
            let families = fonts.collection.register_fonts(blob, None);
            let id = families.first()?.0;
            fonts.collection.family_name(id).map(str::to_owned)
        };
        let ui_family = register(INTER).unwrap_or_else(|| "Inter".into());
        register(INTER_ITALIC);
        let mono_family = register(MONO).unwrap_or_else(|| "JetBrains Mono".into());
        register(MONO_ITALIC);
        Text { fonts, layouts: LayoutContext::new(), ui_family, mono_family, cache: HashMap::new(), laid: HashMap::new() }
    }

    /// The family names of the bundled UI and code fonts.
    pub fn families(&self) -> (&str, &str) {
        (&self.ui_family, &self.mono_family)
    }

    /// A CSS family list for `family`: `ui` and `code` mean the bundled
    /// fonts; anything else is used as given, with the UI font after it.
    fn family_list(&self, family: &str) -> String {
        match family {
            "ui" | "sans-serif" | "system-ui" => format!("\"{}\", system-ui, sans-serif", self.ui_family),
            "code" | "mono" | "monospace" => format!("\"{}\", monospace", self.mono_family),
            other => format!("{other}, \"{}\", sans-serif", self.ui_family),
        }
    }

    /// A layout from the cache, or made by `make` and kept.
    fn cached(&mut self, key: u64, make: impl FnOnce(&mut Text) -> Layout<Rgba>) -> Layout<Rgba> {
        if let Some(l) = self.laid.get(&key) {
            return l.clone();
        }
        let l = make(self);
        if self.laid.len() > 4096 {
            self.laid.clear();
        }
        self.laid.insert(key, l.clone());
        l
    }

    /// Lay out `text` in `style`, wrapped at `max_width` if given.
    pub fn layout(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Layout<Rgba> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("plain", text, format!("{style:?}"), max_width.map(|w| (w * 4.0) as i64)).hash(&mut h);
        self.cached(h.finish(), |t| t.layout_now(text, style, max_width))
    }

    fn layout_now(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Layout<Rgba> {
        let families = self.family_list(&style.family);
        let mut b = self.layouts.ranged_builder(&mut self.fonts, text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::from(families.as_str())));
        b.push_default(StyleProperty::FontSize(style.size));
        b.push_default(StyleProperty::FontWeight(FontWeight::new(style.weight)));
        if style.italic {
            b.push_default(StyleProperty::FontStyle(FontStyle::Italic));
        }
        b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(style.line_height)));
        b.push_default(StyleProperty::Brush(style.color));
        let mut layout = b.build(text);
        layout.break_all_lines(max_width);
        layout
    }

    /// One line laid out with colored spans: (start, end, color) in
    /// characters, over the base style.
    pub fn layout_spans(&mut self, text: &str, style: &TextStyle, spans: &[(usize, usize, Rgba)]) -> Layout<Rgba> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("spans", text, format!("{style:?}"), spans).hash(&mut h);
        self.cached(h.finish(), |t| t.layout_spans_now(text, style, spans))
    }

    fn layout_spans_now(&mut self, text: &str, style: &TextStyle, spans: &[(usize, usize, Rgba)]) -> Layout<Rgba> {
        let families = self.family_list(&style.family);
        let byte = |i: usize| text.char_indices().nth(i).map_or(text.len(), |(b, _)| b);
        let mut b = self.layouts.ranged_builder(&mut self.fonts, text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::from(families.as_str())));
        b.push_default(StyleProperty::FontSize(style.size));
        b.push_default(StyleProperty::FontWeight(FontWeight::new(style.weight)));
        b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(style.line_height)));
        b.push_default(StyleProperty::Brush(style.color));
        for &(start, end, color) in spans {
            let (a, z) = (byte(start), byte(end));
            if a < z {
                b.push(StyleProperty::Brush(color), a..z);
            }
        }
        let mut layout = b.build(text);
        layout.break_all_lines(None);
        layout
    }

    /// Rich text laid out: spans over a base style, wrapped at max_width.
    /// A span's style may set font-family (ui or code), font-size,
    /// font-weight, font-style, color, underline, strike and link (links
    /// are underlined).
    pub fn layout_rich(&mut self, spans: &[Span], base: &TextStyle, max_width: Option<f32>, lookup: &dyn Fn(&Data) -> Data) -> Layout<Rgba> {
        // The key has the spans' style values as the theme resolves them,
        // so a new theme lays out afresh.
        let resolved: Vec<String> = spans.iter().flat_map(|s| s.style.iter().map(|(k, v)| format!("{k}={:?}", lookup(v)))).collect();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("rich", format!("{spans:?}"), resolved, format!("{base:?}"), max_width.map(|w| (w * 4.0) as i64)).hash(&mut h);
        self.cached(h.finish(), |t| t.layout_rich_now(spans, base, max_width, lookup))
    }

    fn layout_rich_now(&mut self, spans: &[Span], base: &TextStyle, max_width: Option<f32>, lookup: &dyn Fn(&Data) -> Data) -> Layout<Rgba> {
        let text = spans_text(spans);
        let families = self.family_list(&base.family);
        let span_families: Vec<Option<String>> = spans
            .iter()
            .map(|s| s.get("font-family").and_then(|f| f.as_str().or(f.as_key())).map(|f| self.family_list(f)))
            .collect();
        let mut b = self.layouts.ranged_builder(&mut self.fonts, &text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::from(families.as_str())));
        b.push_default(StyleProperty::FontSize(base.size));
        b.push_default(StyleProperty::FontWeight(FontWeight::new(base.weight)));
        if base.italic {
            b.push_default(StyleProperty::FontStyle(FontStyle::Italic));
        }
        b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(base.line_height)));
        b.push_default(StyleProperty::Brush(base.color));
        let mut at = 0;
        for (s, fam) in spans.iter().zip(&span_families) {
            let r = at..at + s.text.len();
            at = r.end;
            if r.is_empty() {
                continue;
            }
            if let Some(f) = fam {
                b.push(StyleProperty::FontFamily(FontFamily::from(f.as_str())), r.clone());
            }
            for (k, v) in &s.style {
                let v = lookup(v);
                match k.as_str() {
                    "font-size" => {
                        if let Some(n) = v.as_num() {
                            b.push(StyleProperty::FontSize(n as f32), r.clone());
                        }
                    }
                    "font-weight" => {
                        let w = match &v {
                            Data::Num(n) => *n as f32,
                            Data::Key(k) if k == "bold" => 700.0,
                            Data::Key(k) if k == "medium" => 500.0,
                            _ => 400.0,
                        };
                        b.push(StyleProperty::FontWeight(FontWeight::new(w)), r.clone());
                    }
                    "font-style" if v.as_key() == Some("italic") => b.push(StyleProperty::FontStyle(FontStyle::Italic), r.clone()),
                    "color" => {
                        if let Some(c) = color(&v) {
                            b.push(StyleProperty::Brush(c), r.clone());
                        }
                    }
                    "underline" | "link" if v.truthy() => b.push(StyleProperty::Underline(true), r.clone()),
                    "strike" if v.truthy() => b.push(StyleProperty::Strikethrough(true), r.clone()),
                    _ => {}
                }
            }
        }
        let mut layout = b.build(&text);
        layout.break_all_lines(max_width);
        layout
    }

    /// The byte index in rich text nearest to point (x, y).
    pub fn rich_index_at(&mut self, spans: &[Span], base: &TextStyle, max_width: Option<f32>, x: f32, y: f32) -> usize {
        let layout = self.layout_rich(spans, base, max_width, &|d| d.clone());
        parley::Cursor::from_point(&layout, x, y).index()
    }

    /// Rectangles (x, y, w, h, relative to the text) covering bytes a..b of
    /// rich text, one per line it touches.
    pub fn rich_rects(layout: &Layout<Rgba>, a: usize, b: usize) -> Vec<(f32, f32, f32, f32)> {
        if a >= b {
            return Vec::new();
        }
        let sel = parley::Selection::new(
            parley::Cursor::from_byte_index(layout, a, parley::Affinity::Downstream),
            parley::Cursor::from_byte_index(layout, b, parley::Affinity::Upstream),
        );
        sel.geometry(layout).into_iter().map(|(r, _)| (r.x0 as f32, r.y0 as f32, (r.x1 - r.x0) as f32, (r.y1 - r.y0) as f32)).collect()
    }

    /// The size text takes (rounded up to whole pixels).
    pub fn measure_text(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> (f32, f32) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        format!("{style:?}").hash(&mut h);
        max_width.map(|w| (w * 4.0) as i64).hash(&mut h);
        let key = h.finish();
        if let Some(s) = self.cache.get(&key) {
            return *s;
        }
        let l = self.layout(text, style, max_width);
        let size = (l.width().ceil(), l.height().ceil());
        if self.cache.len() > 8192 {
            self.cache.clear();
        }
        self.cache.insert(key, size);
        size
    }

    /// The x position of a caret before character `idx` of one-line text.
    pub fn caret_x(&mut self, text: &str, style: &TextStyle, idx: usize) -> f32 {
        if idx == 0 || text.is_empty() {
            return 0.0;
        }
        let layout = self.layout(text, style, None);
        let byte = text.char_indices().nth(idx).map_or(text.len(), |(b, _)| b);
        parley::Cursor::from_byte_index(&layout, byte, parley::Affinity::Downstream).geometry(&layout, 1.0).x0 as f32
    }

    /// The character index nearest to x in one-line text.
    pub fn index_at(&mut self, text: &str, style: &TextStyle, x: f32) -> usize {
        if text.is_empty() {
            return 0;
        }
        let layout = self.layout(text, style, None);
        let byte = parley::Cursor::from_point(&layout, x, 1.0).index();
        text[..byte.min(text.len())].chars().count()
    }

    /// The glyph runs of a laid-out text, for painting.
    pub fn glyph_runs(layout: &Layout<Rgba>) -> Vec<GlyphRun> {
        let mut out = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    let r = run.run();
                    let style = run.style();
                    let m = run.span_metrics();
                    let thick = (r.font_size() / 14.0).max(1.0);
                    let mut lines = Vec::new();
                    if style.underline.is_some() {
                        lines.push((run.offset(), run.baseline() + (m.descent * 0.35).max(1.5), run.advance(), thick));
                    }
                    if style.strikethrough.is_some() {
                        lines.push((run.offset(), run.baseline() - m.x_height * 0.5, run.advance(), thick));
                    }
                    out.push(GlyphRun {
                        font: r.font().clone(),
                        size: r.font_size(),
                        coords: r.normalized_coords().iter().map(|c| c.to_bits()).collect(),
                        color: style.brush,
                        glyphs: run.positioned_glyphs().map(|g| (g.id, g.x, g.y)).collect(),
                        lines,
                    });
                }
            }
        }
        out
    }
}

impl Measure for Text {
    fn measure(&mut self, node: &Node, style: &Props, max_width: Option<f32>) -> (f32, f32) {
        if node.kind == "icon" {
            let s = node.prop("size").and_then(Data::as_num).unwrap_or(16.0) as f32;
            return (s, s);
        }
        if node.kind == "rich-text" {
            let spans = spans_of(node.prop("spans"));
            if spans.is_empty() {
                return (0.0, 0.0);
            }
            let ts = TextStyle::from_props(style, &|d| d.clone());
            let l = self.layout_rich(&spans, &ts, max_width, &|d| d.clone());
            return (l.width().ceil(), l.height().ceil());
        }
        let text = node.text();
        if text.is_empty() {
            return (0.0, 0.0);
        }
        let ts = TextStyle::from_props(style, &|d| d.clone());
        self.measure_text(text, &ts, max_width)
    }
    fn caret_x(&mut self, value: &str, style: &Props, idx: usize) -> f32 {
        let ts = TextStyle::from_props(style, &|d| d.clone());
        Text::caret_x(self, value, &ts, idx)
    }
    fn index_at(&mut self, value: &str, style: &Props, x: f32) -> usize {
        let ts = TextStyle::from_props(style, &|d| d.clone());
        Text::index_at(self, value, &ts, x)
    }
    fn rich_index_at(&mut self, spans: &[Span], style: &Props, width: f32, x: f32, y: f32) -> usize {
        let ts = TextStyle::from_props(style, &|d| d.clone());
        Text::rich_index_at(self, spans, &ts, Some(width + 0.5), x, y)
    }
}
