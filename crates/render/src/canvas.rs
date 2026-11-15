//! Canvas nodes (docs/ui-protocol.md §8): a node's `:commands` drawn in its
//! own coordinates. Rectangles and circles are boxes (antialiased by the
//! box shader); lines, polylines and SVG paths are tessellated by lyon into
//! triangles; text is laid out like any other.
//!
//!   [:rect [x y w h] paint]       [:rounded-rect [x y w h] r paint]
//!   [:circle [cx cy] r paint]     [:line [x1 y1] [x2 y2] stroke]
//!   [:polyline [[x y] …] stroke]  [:path "M0 0 L10 10 Z" paint]
//!   [:text [x y] "…" style]       [:clip [x y w h] [cmds…]]
//!   [:translate [dx dy] [cmds…]]
//!
//! A paint is `{:fill color :stroke color :width n}` (either or both).

use crate::display::Item;
use lyon::math::point;
use lyon::path::Path;
use lyon::tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, StrokeOptions, StrokeTessellator, StrokeVertex, VertexBuffers};
use norn_ui_engine::text::{Rgba, TextStyle, color};
use norn_ui_engine::{Data, Rect, Text};

struct Ctx<'a> {
    text: &'a mut Text,
    lookup: &'a dyn Fn(&Data) -> Data,
    out: &'a mut Vec<Item>,
}

fn n(d: Option<&Data>) -> f32 {
    d.and_then(Data::as_num).unwrap_or(0.0) as f32
}

fn pt(d: Option<&Data>) -> (f32, f32) {
    let xs = d.and_then(Data::as_list).unwrap_or(&[]);
    (n(xs.first()), n(xs.get(1)))
}

fn rect4(d: Option<&Data>) -> Rect {
    let xs = d.and_then(Data::as_list).unwrap_or(&[]);
    Rect { x: n(xs.first()), y: n(xs.get(1)), w: n(xs.get(2)), h: n(xs.get(3)) }
}

impl Ctx<'_> {
    fn color(&self, paint: Option<&Data>, k: &str) -> Option<Rgba> {
        color(&(self.lookup)(paint?.get(k)?))
    }

    fn tris(&mut self, path: &Path, paint: Option<&Data>, clip: Rect, fill: bool) {
        let tolerance = 0.05;
        if fill && let Some(c) = self.color(paint, "fill") {
            let mut g: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
            let ok = FillTessellator::new()
                .tessellate_path(path, &FillOptions::tolerance(tolerance), &mut BuffersBuilder::new(&mut g, |v: FillVertex| v.position().to_array()))
                .is_ok();
            if ok {
                self.out.push(Item::Tris { verts: g.indices.iter().map(|&i| g.vertices[i as usize]).collect(), color: c, clip });
            }
        }
        if let Some(c) = self.color(paint, "stroke") {
            let width = paint.and_then(|p| p.get("width")).and_then(Data::as_num).unwrap_or(1.0) as f32;
            let mut g: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
            let ok = StrokeTessellator::new()
                .tessellate_path(
                    path,
                    &StrokeOptions::tolerance(tolerance).with_line_width(width),
                    &mut BuffersBuilder::new(&mut g, |v: StrokeVertex| v.position().to_array()),
                )
                .is_ok();
            if ok {
                self.out.push(Item::Tris { verts: g.indices.iter().map(|&i| g.vertices[i as usize]).collect(), color: c, clip });
            }
        }
    }

    fn boxed(&mut self, r: Rect, radius: f32, paint: Option<&Data>, clip: Rect) {
        let fill = self.color(paint, "fill").unwrap_or([0, 0, 0, 0]);
        let border = self.color(paint, "stroke").map(|c| (paint.and_then(|p| p.get("width")).and_then(Data::as_num).unwrap_or(1.0) as f32, c));
        self.out.push(Item::Box { rect: r, color: fill, radius, border, clip });
    }

    fn run(&mut self, cmds: &[Data], dx: f32, dy: f32, clip: Rect) {
        for c in cmds {
            let Some(xs) = c.as_list() else { continue };
            let op = xs.first().and_then(Data::as_key).unwrap_or("");
            let at = |r: Rect| Rect { x: r.x + dx, y: r.y + dy, ..r };
            match op {
                "rect" => self.boxed(at(rect4(xs.get(1))), 0.0, xs.get(2), clip),
                "rounded-rect" => self.boxed(at(rect4(xs.get(1))), n(xs.get(2)), xs.get(3), clip),
                "circle" => {
                    let ((cx, cy), r) = (pt(xs.get(1)), n(xs.get(2)));
                    self.boxed(Rect { x: cx - r + dx, y: cy - r + dy, w: 2.0 * r, h: 2.0 * r }, r, xs.get(3), clip)
                }
                "line" | "polyline" => {
                    let pts: Vec<(f32, f32)> = if op == "line" {
                        vec![pt(xs.get(1)), pt(xs.get(2))]
                    } else {
                        xs.get(1).and_then(Data::as_list).unwrap_or(&[]).iter().map(|p| pt(Some(p))).collect()
                    };
                    if pts.len() < 2 {
                        continue;
                    }
                    let mut b = Path::builder();
                    b.begin(point(pts[0].0 + dx, pts[0].1 + dy));
                    for p in &pts[1..] {
                        b.line_to(point(p.0 + dx, p.1 + dy));
                    }
                    b.end(false);
                    let stroke = xs.get(if op == "line" { 3 } else { 2 });
                    self.tris(&b.build(), stroke, clip, false);
                }
                "path" => {
                    let Some(svg) = xs.get(1).and_then(Data::as_str) else { continue };
                    let mut b = Path::builder_with_attributes(0);
                    let mut src = lyon::extra::parser::Source::new(svg.chars());
                    if lyon::extra::parser::PathParser::new().parse(&lyon::extra::parser::ParserOptions::DEFAULT, &mut src, &mut b).is_err() {
                        continue;
                    }
                    let path = b.build().transformed(&lyon::math::Translation::new(dx, dy));
                    self.tris(&path, xs.get(2), clip, true);
                }
                "text" => {
                    let (x, y) = pt(xs.get(1));
                    let Some(s) = xs.get(2).and_then(Data::as_str) else { continue };
                    let props = xs.get(3).map(|d| norn_ui_engine::style::props_of(d).unwrap_or_default()).unwrap_or_default();
                    let ts = TextStyle::from_props(&props, self.lookup);
                    let layout = self.text.layout(s, &ts, None);
                    self.out.push(Item::Text { x: x + dx, y: y + dy, runs: Text::glyph_runs(&layout), clip });
                }
                "clip" => {
                    let inner = clip.intersect(&at(rect4(xs.get(1))));
                    self.run(xs.get(2).and_then(Data::as_list).unwrap_or(&[]), dx, dy, inner);
                }
                "translate" => {
                    let (tx, ty) = pt(xs.get(1));
                    self.run(xs.get(2).and_then(Data::as_list).unwrap_or(&[]), dx + tx, dy + ty, clip);
                }
                _ => {}
            }
        }
    }
}

/// The items a canvas node's commands draw, at `r` (window coordinates).
pub fn canvas(commands: &[Data], r: Rect, clip: Rect, text: &mut Text, lookup: &dyn Fn(&Data) -> Data, out: &mut Vec<Item>) {
    let clip = clip.intersect(&r);
    Ctx { text, lookup, out }.run(commands, r.x, r.y, clip);
}
