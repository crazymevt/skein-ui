//! The engine: the retained node tree that patch batches change, layout per
//! window (taffy), hit-testing, the engine-side states (hover, pressed,
//! focus), damage tracking and the counters (docs/ui-protocol.md).
//!
//! Painting (2.4) and text (2.3) build on this: `frame` returns the damaged
//! rectangles to repaint, and nodes carry their resolved styles.

use crate::data::{Data, Event, NodeId, Op, Props, Rect, WindowId};
use crate::raster::{DecodeRequest, RasterImage};
use crate::style::{self, Theme};
use std::collections::{HashMap, HashSet};
use taffy::prelude::*;

pub const PROTOCOL_VERSION: u32 = 1;

const KINDS: &[&str] = &[
    "box", "text", "rich-text", "icon", "image", "svg", "button", "text-field", "scroll", "list", "grid", "editor", "terminal", "canvas",
    "native-view", "popup",
];

#[derive(Debug)]
pub struct Node {
    pub kind: String,
    pub props: Props,
    /// The node's own style (overrides the theme).
    pub style: Props,
    pub children: Vec<NodeId>,
    pub parent: Option<NodeId>,
    layout: taffy::NodeId,
    /// The resolved style as of the last restyle (what is painted).
    pub resolved: Props,
    pub raster: Option<RasterImage>,
}

impl Node {
    pub fn prop(&self, k: &str) -> Option<&Data> {
        self.props.iter().find(|(x, _)| x == k).map(|(_, v)| v)
    }
    pub fn classes(&self) -> Vec<String> {
        match self.prop("class") {
            Some(Data::Key(k)) => vec![k.clone()],
            Some(Data::List(xs)) | Some(Data::Set(xs)) => xs.iter().filter_map(|x| x.as_key().map(str::to_owned)).collect(),
            _ => Vec::new(),
        }
    }
    /// Does Norn want event `e` from this node?
    pub fn wants(&self, e: &str) -> bool {
        self.prop("on").is_some_and(|on| on.has_key(e))
    }
    /// The text a node shows: :text, or a field's :value (its :placeholder
    /// when empty, for measuring).
    pub fn text(&self) -> &str {
        if self.kind == "text-field" {
            let v = self.prop("value").and_then(Data::as_str).unwrap_or("");
            return if v.is_empty() { self.prop("placeholder").and_then(Data::as_str).unwrap_or(" ") } else { v };
        }
        self.prop("text").and_then(Data::as_str).unwrap_or("")
    }
}

/// A scroll bar's thumb being dragged: the node it scrolls, which way, the
/// thumb's length, and where on the thumb the pointer took it.
#[derive(Clone, Copy, Debug)]
struct ThumbDrag {
    window: WindowId,
    node: NodeId,
    vertical: bool,
    thumb: f32,
    grab: f32,
}

#[derive(Debug, Default)]
pub struct Window {
    pub props: Props,
    pub root: Option<NodeId>,
    pub size: (f32, f32),
    pub layout_dirty: bool,
    /// Absolute rectangles from the last layout (scrolling applied).
    pub rects: HashMap<NodeId, Rect>,
    /// Each node's clip: the intersection of its scroll containers.
    pub clips: HashMap<NodeId, Rect>,
    pub damage: Vec<Rect>,
    pub visible: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Stats {
    pub frames: u64,
    pub area: f64,
    pub layouts: u64,
}

/// Measures text for layout and for text-field carets.
pub trait Measure {
    /// The size of node content given a width limit (None: unlimited).
    fn measure(&mut self, node: &Node, style: &Props, max_width: Option<f32>) -> (f32, f32);
    /// The x of a caret before character idx of one-line `value`.
    fn caret_x(&mut self, value: &str, style: &Props, idx: usize) -> f32;
    /// The character index nearest to x in one-line `value`.
    fn index_at(&mut self, value: &str, style: &Props, x: f32) -> usize;
    /// The byte index in rich text (wrapped at width) nearest to (x, y).
    fn rich_index_at(&mut self, spans: &[crate::text::Span], style: &Props, width: f32, x: f32, y: f32) -> usize {
        let _ = (style, width, y);
        let text = crate::text::spans_text(spans);
        let i = ((x / (font_size(style) * 0.6)).round().max(0.0) as usize).min(text.chars().count());
        text.char_indices().nth(i).map_or(text.len(), |(b, _)| b)
    }
}

fn font_size(style: &Props) -> f32 {
    style.iter().find(|(k, _)| k == "font-size").and_then(|(_, v)| v.as_num()).unwrap_or(14.0) as f32
}

/// A rough measure for tests and headless runs without text: a monospace
/// estimate (0.6 em per character, 1.25 em lines).
pub struct RoughMeasure;

impl Measure for RoughMeasure {
    fn measure(&mut self, node: &Node, style: &Props, max_width: Option<f32>) -> (f32, f32) {
        if node.kind == "icon" {
            let s = node.prop("size").and_then(Data::as_num).unwrap_or(16.0) as f32;
            return (s, s);
        }
        let size = font_size(style);
        let text = node.text();
        if text.is_empty() {
            return (0.0, 0.0);
        }
        let cw = size * 0.6;
        let line_h = size * 1.25;
        let widest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32 * cw;
        let lines = text.lines().count().max(1) as f32;
        match max_width {
            Some(w) if widest > w && w > cw => {
                let per_line = (w / cw).floor().max(1.0);
                let wrapped: f32 = text.lines().map(|l| (l.chars().count() as f32 / per_line).ceil().max(1.0)).sum();
                (w, wrapped * line_h)
            }
            _ => (widest, lines * line_h),
        }
    }
    fn caret_x(&mut self, _: &str, style: &Props, idx: usize) -> f32 {
        idx as f32 * font_size(style) * 0.6
    }
    fn index_at(&mut self, value: &str, style: &Props, x: f32) -> usize {
        ((x / (font_size(style) * 0.6)).round().max(0.0) as usize).min(value.chars().count())
    }
}

/// A press in an editor: (editor, where it began, where it is now, moved?).
type EditorDrag = (NodeId, (usize, usize), (usize, usize), bool);

pub struct Engine {
    pub nodes: HashMap<NodeId, Node>,
    pub windows: HashMap<WindowId, Window>,
    pub theme: Theme,
    layout: TaffyTree<NodeId>,
    stats: Stats,
    pub(crate) events: Vec<Event>,
    /// Text fields' editing state.
    pub fields: HashMap<NodeId, crate::field::Field>,
    /// Editor surfaces' viewport state.
    pub editors: HashMap<NodeId, crate::editor::Editor>,
    /// Platform requests for the backend (menus, dialogs, clipboard).
    pub platform: Vec<(String, Vec<Data>)>,
    /// Windows without keyboard focus (blinking stops there).
    unfocused: HashSet<WindowId>,
    /// The text field being drag-selected.
    dragging: Option<NodeId>,
    /// A press in an editor: (editor, where it began, where it is now,
    /// moved?). Moving sends :drag-select; a drag isn't also a click.
    editor_drag: Option<EditorDrag>,
    /// A drag on a node that wants :drag: (window, node, last x, last y,
    /// started).
    drag: Option<(WindowId, NodeId, f32, f32, bool)>,
    /// A scroll bar's thumb being dragged.
    thumb_drag: Option<ThumbDrag>,
    batches: u64,
    pub hovered: Option<(WindowId, NodeId)>,
    pub pressed: Option<(WindowId, NodeId)>,
    pub focused: Option<NodeId>,
    /// Nodes whose paint changed since the last frame.
    repaint: HashSet<NodeId>,
    /// Scroll offsets of scroll containers.
    pub scroll: HashMap<NodeId, (f32, f32)>,
    /// The tooltip pending or shown.
    pub(crate) tooltip: Option<crate::popup::Tooltip>,
    /// Windows the engine sized to their content (`:fit-content`) since
    /// the backend last looked: (window, width, height).
    pub fitted: Vec<(WindowId, f32, f32)>,
    /// The input method's composition in progress.
    pub preedit: Option<crate::ime::Preedit>,
    /// Style transitions running.
    pub(crate) anims: Vec<crate::anim::Anim>,
    /// When the transitions last advanced.
    pub(crate) anim_at: Option<std::time::Instant>,
    /// The selection in a rich-text node.
    pub(crate) rich_sel: Option<crate::rich::RichSel>,
    /// Scroll positions Norn set, for the next frame.
    pending_scroll: HashMap<NodeId, (Option<f32>, Option<f32>)>,
    /// Nodes given :reveal true: scrolled into view after the next layout.
    pending_reveal: HashSet<NodeId>,
    /// Scroll positions recently reported to Norn, per node: Norn setting
    /// one of these back is an echo, not a request (it would undo a fast
    /// wheel).
    reported_scroll: HashMap<NodeId, std::collections::VecDeque<(f32, f32)>>,
    /// Decode images off the engine's thread (`take_decode_requests`)
    /// instead of while applying a batch.
    async_decode: bool,
    decodes: Vec<DecodeRequest>,
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}

impl Engine {
    pub fn new() -> Engine {
        Engine {
            nodes: HashMap::new(),
            windows: HashMap::new(),
            theme: Theme::default(),
            layout: TaffyTree::new(),
            stats: Stats::default(),
            events: Vec::new(),
            batches: 0,
            hovered: None,
            pressed: None,
            focused: None,
            repaint: HashSet::new(),
            scroll: HashMap::new(),
            fields: HashMap::new(),
            editors: HashMap::new(),
            platform: Vec::new(),
            unfocused: HashSet::new(),
            dragging: None,
            editor_drag: None,
            thumb_drag: None,
            tooltip: None,
            fitted: Vec::new(),
            preedit: None,
            anims: Vec::new(),
            anim_at: None,
            rich_sel: None,
            pending_scroll: HashMap::new(),
            pending_reveal: HashSet::new(),
            reported_scroll: HashMap::new(),
            drag: None,
            async_decode: false,
            decodes: Vec::new(),
        }
    }

    /// With on, image decoding waits in `take_decode_requests` for the
    /// backend to run elsewhere, and its results come back through
    /// `decoded`. Otherwise (the default, and the headless backend) images
    /// decode while the batch that needs them is applied.
    pub fn set_async_decode(&mut self, on: bool) {
        self.async_decode = on;
    }

    /// The images waiting to be decoded, for a backend decoding them off
    /// this thread.
    pub fn take_decode_requests(&mut self) -> Vec<DecodeRequest> {
        std::mem::take(&mut self.decodes)
    }

    fn request_decode(&mut self, request: DecodeRequest) {
        if self.async_decode {
            self.decodes.push(request);
        } else {
            let result = request.decode();
            self.decoded(request, result);
        }
    }

    /// A decode's result. An image node takes it only if its `:src` is
    /// still the one decoded (a later one may have replaced it meanwhile).
    pub fn decoded(&mut self, request: DecodeRequest, result: Result<RasterImage, String>) {
        match request {
            DecodeRequest::Node { id, src } => {
                let Some(n) = self.nodes.get_mut(&id) else { return };
                // (a nil :src is no prop at all once merged)
                if n.kind != "image" || n.prop("src").unwrap_or(&Data::Nil) != &src {
                    return;
                }
                let old_size = n.raster.as_ref().map(|image| image.size);
                n.raster = result.ok();
                if old_size != n.raster.as_ref().map(|image| image.size) {
                    self.dirty_layout(id);
                }
                self.repaint.insert(id);
            }
            DecodeRequest::Event { req, .. } => {
                let mut fields = vec![("req", Data::Num(req as f64))];
                match result {
                    Ok(image) => {
                        fields.push(("size", Data::List(image.size.into_iter().map(|n| Data::Num(n.into())).collect())));
                        fields.push(("rgba", Data::Bytes(image.rgba)));
                    }
                    Err(error) => fields.push(("error", Data::Str(error))),
                }
                self.events.push(Event::new("image-decoded", fields));
            }
        }
    }

    /// Events produced since the last call, for Norn.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Files dropped from the OS on window w at (x, y): a :drop event (for
    /// the node there that :accept's files, else the window). Several files
    /// dropped together are one event.
    pub fn files_dropped(&mut self, w: WindowId, x: f32, y: f32, path: &str) {
        let node = self.hit(w, x, y).and_then(|n| self.target(n, "drop"));
        if let Some(last) = self.events.last_mut()
            && last.kind == "drop"
            && last.get("window") == Some(&Data::Num(w as f64))
            && let Some((_, Data::List(ps))) = last.fields.iter_mut().find(|(k, _)| *k == "paths")
        {
            ps.push(Data::Str(path.to_owned()));
            return;
        }
        let mut fields = vec![("window", Data::Num(w as f64)), ("kind", Data::key("files")), ("paths", Data::List(vec![Data::Str(path.to_owned())]))];
        if let Some(n) = node {
            fields.push(("node", Data::Num(n as f64)));
        }
        self.events.push(Event::new("drop", fields));
    }

    /// A command (from a menu or a backend): it goes to the focused node
    /// if that asks for `:command` (an editor's copy and paste), and to the
    /// app's command handlers otherwise.
    pub fn command(&mut self, cmd: Data) {
        let mut fields = vec![("command", cmd)];
        if let Some(n) = self.focused.and_then(|f| self.target(f, "command")) {
            fields.push(("node", Data::Num(n as f64)));
        }
        self.events.push(Event::new("command", fields));
    }

    /// Queue an event for Norn (backends use this for their own answers).
    pub fn push_event(&mut self, e: Event) {
        self.events.push(e);
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    // ---- applying batches ---------------------------------------------------------------------

    /// Apply a batch atomically: either every op applies, or none does and
    /// an `:error` event says why.
    pub fn apply(&mut self, batch: Vec<Op>) -> bool {
        self.batches += 1;
        if let Err((i, msg)) = self.check(&batch) {
            let op = format!("{:?}", batch[i]);
            self.events.push(Event::new(
                "error",
                vec![("batch", Data::Num(self.batches as f64)), ("op", Data::Str(op)), ("message", Data::Str(msg))],
            ));
            return false;
        }
        for op in batch {
            self.apply_op(op);
        }
        true
    }

    /// Check a batch against the tree as it would be after each op.
    fn check(&self, batch: &[Op]) -> Result<(), (usize, String)> {
        let mut exists: HashSet<NodeId> = HashSet::new();
        let mut gone: HashSet<NodeId> = HashSet::new();
        let has = |id: NodeId, exists: &HashSet<NodeId>, gone: &HashSet<NodeId>| {
            !gone.contains(&id) && (exists.contains(&id) || self.nodes.contains_key(&id))
        };
        // Children counts and parents as the batch changes them: inserting a
        // node takes it out of where it was, as removing one does.
        let mut child_counts: HashMap<NodeId, usize> = HashMap::new();
        let mut parents: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let count_of = |counts: &mut HashMap<NodeId, usize>, p: NodeId| -> usize {
            *counts.entry(p).or_insert_with(|| self.nodes.get(&p).map_or(0, |n| n.children.len()))
        };
        let parent_of = |parents: &HashMap<NodeId, Option<NodeId>>, id: NodeId| -> Option<NodeId> {
            match parents.get(&id) {
                Some(p) => *p,
                None => self.nodes.get(&id).and_then(|n| n.parent),
            }
        };
        for (i, op) in batch.iter().enumerate() {
            let fail = |m: String| Err((i, m));
            match op {
                Op::Create { id, kind, props } => {
                    if has(*id, &exists, &gone) {
                        return fail(format!("node {id} already exists"));
                    }
                    if !KINDS.contains(&kind.as_str()) {
                        return fail(format!("unknown node kind :{kind}"));
                    }
                    let _ = props;
                    exists.insert(*id);
                    gone.remove(id);
                    child_counts.insert(*id, 0);
                    parents.insert(*id, None);
                }
                Op::Set { id, .. } | Op::Text { id, .. } | Op::Focus(id) => {
                    if !has(*id, &exists, &gone) {
                        return fail(format!("no node {id}"));
                    }
                }
                Op::Style { id, style } => {
                    if !has(*id, &exists, &gone) {
                        return fail(format!("no node {id}"));
                    }
                    // nil removes a property: nothing to check
                    let set: Props = style.iter().filter(|(_, v)| *v != Data::Nil).cloned().collect();
                    style::layout_style(&set).map_err(|m| (i, m))?;
                }
                Op::Insert { parent, index, id } => {
                    if !has(*parent, &exists, &gone) || !has(*id, &exists, &gone) {
                        return fail(format!("no node {} to insert into {}", id, parent));
                    }
                    if parent == id {
                        return fail("a node can't be its own child".into());
                    }
                    if let Some(q) = parent_of(&parents, *id) {
                        let c = count_of(&mut child_counts, q);
                        child_counts.insert(q, c.saturating_sub(1));
                    }
                    let n = count_of(&mut child_counts, *parent);
                    if *index > n {
                        return fail(format!("index {index} is past node {parent}'s {n} children"));
                    }
                    child_counts.insert(*parent, n + 1);
                    parents.insert(*id, Some(*parent));
                }
                Op::Move { parent, from, to } => {
                    if !has(*parent, &exists, &gone) {
                        return fail(format!("no node {parent}"));
                    }
                    let n = count_of(&mut child_counts, *parent);
                    if *from >= n || *to >= n {
                        return fail(format!("move {from} → {to} is outside node {parent}'s {n} children"));
                    }
                }
                Op::Remove(id) => {
                    if !has(*id, &exists, &gone) {
                        return fail(format!("no node {id}"));
                    }
                    if let Some(q) = parent_of(&parents, *id) {
                        let c = count_of(&mut child_counts, q);
                        child_counts.insert(q, c.saturating_sub(1));
                    }
                    parents.insert(*id, None);
                    gone.insert(*id);
                }
                Op::Window { props, .. } => {
                    if let Some(Data::Num(root)) = props.iter().find(|(k, _)| k == "root").map(|(_, v)| v)
                        && !has(*root as NodeId, &exists, &gone)
                    {
                        return fail(format!("no node {root} for the window's root"));
                    }
                }
                Op::Theme(d) => {
                    Theme::from_data(d).map_err(|m| (i, m))?;
                }
                Op::Editor { id, .. } => {
                    if !has(*id, &exists, &gone) {
                        return fail(format!("no node {id}"));
                    }
                }
                Op::Hello(_) | Op::CloseWindow(_) | Op::Stats(_) | Op::DecodeImage { .. } | Op::Platform { .. } => {}
            }
        }
        Ok(())
    }

    fn apply_op(&mut self, op: Op) {
        // A scroll container's :scroll-x/:scroll-y set by Norn: applied at
        // the next frame, once the layout is current.
        if let Op::Create { id, props, .. } | Op::Set { id, props } = &op {
            let get = |k: &str| props.iter().find(|(x, _)| x == k).and_then(|(_, v)| v.as_num()).map(|n| n as f32);
            let (x, y) = (get("scroll-x"), get("scroll-y"));
            if x.is_some() || y.is_some() {
                self.pending_scroll.insert(*id, (x, y));
            }
            if props.iter().any(|(k, v)| k == "reveal" && v.truthy()) {
                self.pending_reveal.insert(*id);
            }
        }
        match op {
            Op::Hello(v) => self.events.push(Event::new(
                "hello",
                vec![
                    ("version", Data::Num(PROTOCOL_VERSION.min(v) as f64)),
                    ("features", Data::Set(vec![Data::key("editor"), Data::key("canvas"), Data::key("lists")])),
                ],
            )),
            Op::Window { id, props } => {
                let w = self.windows.entry(id).or_default();
                style::merge(&mut w.props, &props);
                if let Some(Data::Num(r)) = props.iter().find(|(k, _)| k == "root").map(|(_, v)| v) {
                    w.root = Some(*r as NodeId);
                }
                if let Some(Data::List(s)) = props.iter().find(|(k, _)| k == "size").map(|(_, v)| v)
                    && let (Some(a), Some(b)) = (s.first().and_then(Data::as_num), s.get(1).and_then(Data::as_num))
                {
                    w.size = (a as f32, b as f32);
                }
                w.visible = true;
                w.layout_dirty = true;
            }
            Op::CloseWindow(id) => {
                self.windows.remove(&id);
            }
            Op::Create { id, kind, props } => {
                let layout = self.layout.new_leaf_with_context(Style::default(), id).expect("taffy leaf");
                if kind == "editor" {
                    self.editors.insert(id, crate::editor::Editor::new());
                }
                if kind == "text-field" {
                    let v = props.iter().find(|(k, _)| k == "value").and_then(|(_, v)| v.as_str()).unwrap_or("").to_owned();
                    self.fields.insert(id, crate::field::Field::new(v));
                }
                let image_src = if kind == "image" { props.iter().find(|(name, _)| name == "src").map(|(_, src)| src.clone()) } else { None };
                self.nodes.insert(id, Node { kind, props, style: Vec::new(), children: Vec::new(), parent: None, layout, resolved: Vec::new(), raster: None });
                let _ = self.restyle(id);
                if let Some(src) = image_src {
                    self.request_decode(DecodeRequest::Node { id, src });
                }
            }
            Op::Set { id, props } => {
                // Norn setting a field's value replaces what was typed.
                if let (Some(f), Some(Data::Str(v))) = (self.fields.get_mut(&id), props.iter().find(|(k, _)| k == "value").map(|(_, v)| v))
                    && f.value != *v
                {
                    // Replaced by the program: the caret goes to the end.
                    let n = v.chars().count();
                    f.value = v.clone();
                    f.caret = n;
                    f.anchor = n;
                }
                // Text and visibility change the layout; any prop can change
                // the style (:class, and the :selected and :disabled states),
                // so restyle — which repaints only what really changed.
                // (rich text's content is its :spans; an icon's size is its :size)
                let relayout = props.iter().any(|(k, _)| matches!(k.as_str(), "text" | "spans" | "hidden" | "value" | "size"));
                let mut new_src = None;
                if let Some(n) = self.nodes.get_mut(&id) {
                    if n.kind == "image"
                        && let Some((_, src)) = props.iter().find(|(k, _)| k == "src")
                        && n.prop("src") != Some(src)
                    {
                        new_src = Some(src.clone());
                    }
                    style::merge(&mut n.props, &props);
                }
                // (the node shows its old image until the new one is ready)
                if let Some(src) = new_src {
                    self.request_decode(DecodeRequest::Node { id, src });
                }
                self.restyle_down(id);
                if relayout {
                    self.dirty_layout(id);
                }
                self.repaint.insert(id);
            }
            Op::Style { id, style } => {
                let layout = style.iter().any(|(k, _)| style::affects_layout(k));
                let passes_down = style.iter().any(|(k, _)| style::inherited(k));
                if let Some(n) = self.nodes.get_mut(&id) {
                    style::merge(&mut n.style, &style);
                }
                if passes_down {
                    self.restyle_down(id);
                } else if self.restyle(id) {
                    self.repaint.insert(id);
                }
                if layout {
                    self.dirty_layout(id);
                }
            }
            Op::Text { id, text } => {
                if let Some(n) = self.nodes.get_mut(&id) {
                    style::merge(&mut n.props, &vec![("text".into(), Data::Str(text))]);
                    let l = n.layout;
                    let _ = self.layout.mark_dirty(l);
                }
                self.dirty_layout(id);
                self.repaint.insert(id);
            }
            Op::Insert { parent, index, id } => {
                self.detach(id);
                let (pl, cl) = (self.nodes[&parent].layout, self.nodes[&id].layout);
                let _ = self.layout.insert_child_at_index(pl, index, cl);
                self.nodes.get_mut(&parent).unwrap().children.insert(index, id);
                self.nodes.get_mut(&id).unwrap().parent = Some(parent);
                // It now inherits from its new parent.
                self.restyle_subtree(id);
                self.dirty_layout(parent);
            }
            Op::Move { parent, from, to } => {
                let p = self.nodes.get_mut(&parent).unwrap();
                let c = p.children.remove(from);
                p.children.insert(to, c);
                let kids = p.children.clone();
                let ids: Vec<taffy::NodeId> = kids.iter().map(|c| self.nodes[c].layout).collect();
                let pl = self.nodes[&parent].layout;
                let _ = self.layout.set_children(pl, &ids);
                self.dirty_layout(parent);
            }
            Op::Remove(id) => {
                if let Some(p) = self.nodes.get(&id).and_then(|n| n.parent) {
                    self.dirty_layout(p);
                }
                self.detach(id);
                self.destroy(id);
            }
            Op::Theme(d) => {
                self.theme = Theme::from_data(&d).unwrap_or_default();
                let ids: Vec<NodeId> = self.nodes.keys().copied().collect();
                for id in ids {
                    if self.restyle(id) {
                        self.repaint.insert(id);
                    }
                }
                // Styles name colours by token, and a token's colour may
                // have changed with no style changing: repaint everything.
                for w in self.windows.values_mut() {
                    w.layout_dirty = true;
                    w.damage.push(Rect { x: 0.0, y: 0.0, w: w.size.0, h: w.size.1 });
                }
            }
            Op::Focus(id) => self.set_focus(id),
            Op::Editor { id, op, args } => self.editor_op(id, &op, &args),
            Op::Platform { op, args } => self.platform.push((op, args)),
            Op::DecodeImage { req, src, size } => self.request_decode(DecodeRequest::Event { req, src, size }),
            Op::Stats(req) => {
                let s = std::mem::take(&mut self.stats);
                self.events.push(Event::new(
                    "stats",
                    vec![
                        ("req", Data::Num(req as f64)),
                        ("frames", Data::Num(s.frames as f64)),
                        ("area", Data::Num(s.area)),
                        ("layouts", Data::Num(s.layouts as f64)),
                    ],
                ));
            }
        }
    }

    fn detach(&mut self, id: NodeId) {
        if let Some(p) = self.nodes.get(&id).and_then(|n| n.parent) {
            let cl = self.nodes[&id].layout;
            let pl = self.nodes[&p].layout;
            let _ = self.layout.remove_child(pl, cl);
            self.nodes.get_mut(&p).unwrap().children.retain(|c| *c != id);
            self.nodes.get_mut(&id).unwrap().parent = None;
        }
    }

    /// Move keyboard focus to `id` (restyling both ends for :focused).
    pub fn set_focus(&mut self, id: NodeId) {
        let old = self.focused.replace(id);
        for n in [old, Some(id)].into_iter().flatten() {
            self.restyle_subtree(n);
            self.repaint.insert(n);
        }
    }

    pub(crate) fn repaint_node(&mut self, id: NodeId) {
        self.repaint.insert(id);
    }

    /// Whether node id's window has keyboard focus (true until told).
    pub fn window_focused(&self, id: NodeId) -> bool {
        self.window_of(id).is_none_or(|w| !self.unfocused.contains(&w))
    }

    /// A window gained or lost keyboard focus.
    pub fn set_window_focus(&mut self, w: WindowId, focused: bool) {
        if focused {
            self.unfocused.remove(&w);
        } else {
            self.unfocused.insert(w);
        }
        if let Some(id) = self.focused_field() {
            if let Some(f) = self.fields.get_mut(&id) {
                f.active_at = std::time::Instant::now();
                f.caret_on = true;
            }
            self.repaint.insert(id);
        }
        self.events.push(Event::new("window-focus", vec![("window", Data::Num(w as f64)), ("focused", Data::Bool(focused))]));
    }

    fn destroy(&mut self, id: NodeId) {
        self.fields.remove(&id);
        self.editors.remove(&id);
        if let Some(n) = self.nodes.remove(&id) {
            for c in n.children {
                if let Some(cn) = self.nodes.get_mut(&c) {
                    cn.parent = None;
                }
                self.destroy(c);
            }
            let _ = self.layout.remove(n.layout);
            if self.focused == Some(id) {
                self.focused = None;
            }
        }
    }

    /// The window holding node `id`, if it is shown.
    pub fn window_of(&self, mut id: NodeId) -> Option<WindowId> {
        loop {
            match self.nodes.get(&id)?.parent {
                Some(p) => id = p,
                None => return self.windows.iter().find(|(_, w)| w.root == Some(id)).map(|(k, _)| *k),
            }
        }
    }

    fn dirty_layout(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.get(&id) {
            let _ = self.layout.mark_dirty(n.layout);
        }
        if let Some(w) = self.window_of(id) {
            self.windows.get_mut(&w).unwrap().layout_dirty = true;
        }
    }

    /// Is `id` the node `n` or one of its ancestors?
    pub(crate) fn holds(&self, id: NodeId, mut n: NodeId) -> bool {
        loop {
            if n == id {
                return true;
            }
            match self.nodes.get(&n).and_then(|x| x.parent) {
                Some(p) => n = p,
                None => return false,
            }
        }
    }

    /// The states the engine tracks for node `id`. Hover and press apply to
    /// the node under the pointer and its ancestors, as in CSS.
    pub fn states(&self, id: NodeId) -> Vec<&'static str> {
        let mut s = Vec::new();
        if self.hovered.is_some_and(|(_, n)| self.holds(id, n)) {
            s.push("hover");
        }
        if self.pressed.is_some_and(|(_, n)| self.holds(id, n)) {
            s.push("pressed");
        }
        if self.focused == Some(id) {
            s.push("focused");
        }
        if let Some(n) = self.nodes.get(&id) {
            if n.prop("disabled").is_some_and(Data::truthy) {
                s.push("disabled");
            }
            if n.prop("selected").is_some_and(Data::truthy) {
                s.push("selected");
            }
        }
        s
    }

    /// The node's full style: text properties inherited from its parent (as
    /// in CSS), then theme rules for its kind, classes and states, then its
    /// own style.
    pub fn resolved_style(&self, id: NodeId) -> Props {
        let inherited = match self.nodes[&id].parent {
            Some(p) => self.resolved_style(p).into_iter().filter(|(k, _)| style::inherited(k)).collect(),
            // A root's text takes the theme's :text colour (and passes it
            // down), so a dark theme needs no rule per kind.
            None if self.theme.tokens.iter().any(|(k, _)| k == "text") => vec![("color".into(), Data::key("text"))],
            None => Vec::new(),
        };
        self.style_from(id, inherited)
    }

    /// Node id's style given what it inherits from its parent's.
    fn style_from(&self, id: NodeId, inherited: Props) -> Props {
        let n = &self.nodes[&id];
        let mut s = inherited;
        style::merge(&mut s, &style::kind_defaults(&n.kind));
        // A scroll container's children keep their size (they scroll
        // rather than shrink to fit).
        if n.parent.is_some_and(|p| self.nodes.get(&p).is_some_and(|pn| pn.kind == "scroll")) {
            style::merge(&mut s, &vec![("flex-shrink".into(), Data::Num(0.0))]);
        }
        style::merge(&mut s, &self.theme.resolve(&n.kind, &n.classes(), &self.states(id)));
        style::merge(&mut s, &n.style);
        if n.prop("hidden").is_some_and(Data::truthy) {
            style::merge(&mut s, &vec![("display".into(), Data::key("none"))]);
        }
        s
    }

    /// Recompute a node's style. True if it changed (it needs repainting;
    /// the layout is marked dirty if a layout property changed).
    fn restyle(&mut self, id: NodeId) -> bool {
        if !self.nodes.contains_key(&id) {
            return false;
        }
        let s = self.resolved_style(id);
        self.restyle_as(id, s)
    }

    /// Restyle id to s, its style as resolved (before transitions).
    fn restyle_as(&mut self, id: NodeId, mut s: Props) -> bool {
        if s == self.nodes[&id].resolved {
            return false;
        }
        let shown = self.nodes[&id].resolved.clone();
        if !shown.is_empty() {
            self.start_transitions(id, &shown, &mut s);
            if s == shown {
                return false;
            }
        }
        let layout_changed = {
            let old = &self.nodes[&id].resolved;
            let get = |p: &Props, k: &str| p.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
            s.iter().chain(old.iter()).any(|(k, _)| style::affects_layout(k) && get(&s, k) != get(old, k))
        };
        if let Ok(ls) = style::layout_style(&s) {
            let l = self.nodes[&id].layout;
            let _ = self.layout.set_style(l, ls);
        }
        self.nodes.get_mut(&id).unwrap().resolved = s;
        if layout_changed {
            self.dirty_layout(id);
        }
        true
    }

    // ---- frames -------------------------------------------------------------------------------

    /// Bring window `w`'s layout up to date, adding what moved to its
    /// damage (backends call this before hit-testing input).
    pub fn update_layout(&mut self, w: WindowId, measure: &mut dyn Measure) {
        let Some(win) = self.windows.get(&w) else { return };
        if !win.layout_dirty {
            return;
        }
        let old = win.rects.clone();
        self.run_layout(w, measure);
        let mut damage = Vec::new();
        let new = &self.windows[&w].rects;
        for (id, r) in new {
            if old.get(id) != Some(r) {
                damage.push(*r);
                if let Some(o) = old.get(id) {
                    damage.push(*o);
                }
            }
        }
        for (id, o) in &old {
            if !new.contains_key(id) {
                damage.push(*o);
            }
        }
        self.windows.get_mut(&w).unwrap().damage.extend(damage);
    }

    /// Lay out window `w` if needed and return the rectangles to repaint
    /// (empty: draw nothing). Counts frames, area and layout passes.
    pub fn frame(&mut self, w: WindowId, measure: &mut dyn Measure) -> Vec<Rect> {
        match self.windows.get(&w) {
            Some(win) if win.visible => {}
            _ => return Vec::new(),
        }
        let relaid = self.windows[&w].layout_dirty;
        self.update_layout(w, measure);
        self.apply_pending_scroll(w);
        let reveal: Vec<NodeId> = self.pending_reveal.iter().copied().filter(|n| self.window_of(*n) == Some(w)).collect();
        for id in reveal {
            self.pending_reveal.remove(&id);
            self.scroll_into_view(id);
        }
        if relaid {
            let ids: Vec<NodeId> = self.fields.keys().copied().filter(|f| self.window_of(*f) == Some(w)).collect();
            for f in ids {
                self.update_field_after_layout(f, measure);
            }
        }
        let eds: Vec<NodeId> = self.editors.keys().copied().filter(|e| self.window_of(*e) == Some(w)).collect();
        for e in eds {
            self.update_editor(e, measure);
        }
        let mut damage = std::mem::take(&mut self.windows.get_mut(&w).unwrap().damage);
        let rects = &self.windows[&w].rects;
        let repaint: Vec<NodeId> = self.repaint.iter().copied().filter(|id| rects.contains_key(id)).collect();
        for id in &repaint {
            damage.push(rects[id]);
            self.repaint.remove(id);
        }
        let (ww, wh) = self.windows[&w].size;
        let bounds = Rect { x: 0.0, y: 0.0, w: ww, h: wh };
        let mut out: Vec<Rect> = Vec::new();
        for r in damage.into_iter().map(|r| r.intersect(&bounds)).filter(|r| !r.is_empty()) {
            if !out.iter().any(|o| o.intersect(&r) == r) {
                out.retain(|o| r.intersect(o) != *o);
                out.push(r);
            }
        }
        if !out.is_empty() {
            self.stats.frames += 1;
            self.stats.area += out.iter().map(|r| r.area() as f64).sum::<f64>();
        }
        out
    }

    fn run_layout(&mut self, w: WindowId, measure: &mut dyn Measure) {
        let win = self.windows.get_mut(&w).unwrap();
        win.layout_dirty = false;
        let Some(root) = win.root else {
            win.rects.clear();
            return;
        };
        let fit = win.props.iter().any(|(k, v)| k == "fit-content" && v.truthy());
        let size = if fit {
            Size { width: AvailableSpace::MaxContent, height: AvailableSpace::MaxContent }
        } else {
            Size { width: AvailableSpace::Definite(win.size.0), height: AvailableSpace::Definite(win.size.1) }
        };
        let root_layout = self.nodes[&root].layout;
        // The root fills the window (fitting, it keeps its own size and
        // sizes the window).
        if !fit && let Ok(mut s) = self.layout.style(root_layout).cloned() {
            s.size = Size { width: length(win.size.0), height: length(win.size.1) };
            let _ = self.layout.set_style(root_layout, s);
        }
        let nodes = &self.nodes;
        let theme_styles: HashMap<NodeId, Props> =
            nodes.iter().filter(|(_, n)| n.children.is_empty()).map(|(id, _)| (*id, self.resolved_style(*id))).collect();
        let _ = self.layout.compute_layout_with_measure(root_layout, size, |known, avail, _tid, ctx, _style| {
            let Some(id) = ctx.copied() else { return Size::ZERO };
            let Some(n) = nodes.get(&id) else { return Size::ZERO };
            if !n.children.is_empty() {
                return Size::ZERO;
            }
            if n.kind == "image" {
                let [width, height] = n.raster.as_ref().map_or([0.0; 2], |image| image.size.map(|n| n as f32));
                return match (known.width, known.height) {
                    (Some(w), None) if width > 0.0 => Size { width: w, height: w * height / width },
                    (None, Some(h)) if height > 0.0 => Size { width: h * width / height, height: h },
                    (w, h) => Size { width: w.unwrap_or(width), height: h.unwrap_or(height) },
                };
            }
            // Min-content is the text wrapped at every chance (its longest
            // word); max-content is the text unwrapped.
            let max_w = known.width.or(match avail.width {
                AvailableSpace::Definite(x) => Some(x),
                AvailableSpace::MinContent => Some(0.0),
                AvailableSpace::MaxContent => None,
            });
            let st = theme_styles.get(&id).cloned().unwrap_or_default();
            // :white-space :pre (or :nowrap): text that doesn't wrap
            let max_w = if crate::style::no_wrap(&st) { None } else { max_w };
            let (mw, mh) = measure.measure(n, &st, max_w);
            Size { width: known.width.unwrap_or(mw), height: known.height.unwrap_or(mh) }
        });
        self.stats.layouts += 1;
        if fit && let Ok(l) = self.layout.layout(root_layout) {
            let (fw, fh) = (l.size.width.ceil(), l.size.height.ceil());
            let win = self.windows.get_mut(&w).unwrap();
            if win.size != (fw, fh) {
                win.size = (fw, fh);
                win.damage.push(Rect { x: 0.0, y: 0.0, w: fw, h: fh });
                self.fitted.push((w, fw, fh));
            }
        }
        self.recollect(w);
    }

    /// Recompute window w's rectangles and clips from the last layout and
    /// the scroll offsets (no layout pass: scrolling uses this).
    fn recollect(&mut self, w: WindowId) {
        let Some(root) = self.windows.get(&w).and_then(|x| x.root) else { return };
        let (ww, wh) = self.windows[&w].size;
        let mut rects = HashMap::new();
        let mut clips = HashMap::new();
        self.collect_rects(root, 0.0, 0.0, Rect { x: 0.0, y: 0.0, w: ww, h: wh }, &mut rects, &mut clips);
        let win = self.windows.get_mut(&w).unwrap();
        win.rects = rects;
        win.clips = clips;
    }

    /// Does node id clip its children (and so may be scrolled by Norn)?
    fn scrolls(&self, id: NodeId) -> bool {
        let n = &self.nodes[&id];
        n.kind == "scroll" || n.resolved.iter().any(|(k, v)| k == "overflow" && matches!(v.as_key(), Some("scroll" | "hidden")))
    }

    /// May the user scroll node id (the wheel, bringing a focused node into
    /// view)? A scroll container, not a box whose overflow is only hidden:
    /// that clips, as in CSS, and the wheel passes over it.
    fn user_scrolls(&self, id: NodeId) -> bool {
        let n = &self.nodes[&id];
        n.kind == "scroll" || n.resolved.iter().any(|(k, v)| k == "overflow" && v.as_key() == Some("scroll"))
    }

    fn collect_rects(&self, id: NodeId, ox: f32, oy: f32, clip: Rect, out: &mut HashMap<NodeId, Rect>, clips: &mut HashMap<NodeId, Rect>) {
        let n = &self.nodes[&id];
        let Ok(l) = self.layout.layout(n.layout) else { return };
        let r = Rect { x: ox + l.location.x, y: oy + l.location.y, w: l.size.width, h: l.size.height };
        out.insert(id, r);
        clips.insert(id, clip);
        let (child_clip, (sx, sy)) =
            if self.scrolls(id) { (clip.intersect(&r), self.scroll.get(&id).copied().unwrap_or((0.0, 0.0))) } else { (clip, (0.0, 0.0)) };
        for c in &n.children {
            self.collect_rects(*c, r.x - sx, r.y - sy, child_clip, out, clips);
        }
    }

    /// How far a scroll container can scroll: (content - viewport), per axis.
    /// The scroll bars of node id, if the user scrolls it and its content
    /// is bigger than its view: thumbs (vertical, then horizontal), in
    /// window coordinates, showing where the view is.
    pub fn scroll_thumbs(&self, win: &Window, id: NodeId) -> Vec<Rect> {
        if !self.user_scrolls(id) {
            return Vec::new();
        }
        let (Some(r), Ok(l)) = (win.rects.get(&id), self.layout.layout(self.nodes[&id].layout)) else { return Vec::new() };
        let (cw, ch) = (l.content_size.width, l.content_size.height);
        let (sx, sy) = self.scroll.get(&id).copied().unwrap_or((0.0, 0.0));
        let mut out = Vec::new();
        if ch > r.h + 1.0 && r.h > 20.0 {
            let th = (r.h * r.h / ch).clamp(24.0, r.h);
            let at = (sy / (ch - r.h)).clamp(0.0, 1.0);
            out.push(Rect { x: r.x + r.w - 8.0, y: r.y + at * (r.h - th), w: 6.0, h: th });
        }
        if cw > r.w + 1.0 && r.w > 20.0 {
            let tw = (r.w * r.w / cw).clamp(24.0, r.w);
            let at = (sx / (cw - r.w)).clamp(0.0, 1.0);
            out.push(Rect { x: r.x + at * (r.w - tw), y: r.y + r.h - 8.0, w: tw, h: 6.0 });
        }
        out
    }

    fn scroll_range(&self, w: WindowId, id: NodeId) -> (f32, f32) {
        let win = &self.windows[&w];
        let (Some(r), Some(&(sx, sy))) = (win.rects.get(&id), Some(self.scroll.get(&id).unwrap_or(&(0.0, 0.0)))) else { return (0.0, 0.0) };
        let mut right: f32 = r.x + r.w;
        let mut bottom: f32 = r.y + r.h;
        let mut stack: Vec<NodeId> = self.nodes[&id].children.clone();
        while let Some(c) = stack.pop() {
            if let Some(cr) = win.rects.get(&c) {
                right = right.max(cr.x + cr.w + sx);
                bottom = bottom.max(cr.y + cr.h + sy);
            }
            stack.extend(self.nodes[&c].children.iter().copied());
        }
        ((right - (r.x + r.w)).max(0.0), (bottom - (r.y + r.h)).max(0.0))
    }

    /// Deliver the wheel to a :wheel handler, if one takes it: true if so.
    /// Going up from the node under the pointer, the first of an enabled
    /// handler and a scroll view or editor that can still move that way
    /// takes the wheel. An inner scroll view keeps scrolling inside an
    /// outer handler, which hears the wheel once the view can't move.
    pub fn dispatch_wheel(&mut self, w: WindowId, x: f32, y: f32, dx: f32, dy: f32, modifiers: &[&str]) -> bool {
        let mut at = self.hit(w, x, y);
        let id = loop {
            let Some(id) = at else { return false };
            let Some(n) = self.nodes.get(&id) else { return false };
            if n.wants("wheel") && !n.prop("disabled").is_some_and(Data::truthy) {
                break id;
            }
            let parent = n.parent;
            if (self.is_editor(id) && self.editor_scroll_target(id, dx, dy).is_some())
                || (self.user_scrolls(id) && self.container_scroll_target(w, id, dx, dy).is_some())
            {
                return false;
            }
            at = parent;
        };
        let Some(r) = self.windows.get(&w).and_then(|win| win.rects.get(&id)).copied() else { return false };
        let modifiers = Data::Set(modifiers.iter().map(|m| Data::key(m)).collect());
        // A wheel event still waiting for Norn, for the same node and
        // modifiers, takes this one's movement: a fast trackpad scroll
        // reaches Norn as a few events, not one per OS event.
        if let Some(last) = self.events.last_mut()
            && last.kind == "wheel"
            && last.get("window") == Some(&Data::Num(w as f64))
            && last.get("node") == Some(&Data::Num(id as f64))
            && last.get("modifiers") == Some(&modifiers)
        {
            for (key, value) in last.fields.iter_mut() {
                match (*key, value) {
                    ("x", v) => *v = Data::Num((x - r.x) as f64),
                    ("y", v) => *v = Data::Num((y - r.y) as f64),
                    ("dx", Data::Num(n)) => *n += dx as f64,
                    ("dy", Data::Num(n)) => *n += dy as f64,
                    _ => {}
                }
            }
            return true;
        }
        self.events.push(Event::new(
            "wheel",
            vec![
                ("window", Data::Num(w as f64)),
                ("node", Data::Num(id as f64)),
                ("x", Data::Num((x - r.x) as f64)),
                ("y", Data::Num((y - r.y) as f64)),
                ("dx", Data::Num(dx as f64)),
                ("dy", Data::Num(dy as f64)),
                ("modifiers", modifiers),
            ],
        ));
        true
    }

    /// Where scroll container id would move for a wheel of (dx, dy), if
    /// it moves. A box that scrolls only sideways (a strip of tabs) takes a
    /// plain wheel's turn as sideways.
    fn container_scroll_target(&self, w: WindowId, id: NodeId, dx: f32, dy: f32) -> Option<(f32, f32)> {
        let (sx, sy) = self.scroll.get(&id).copied().unwrap_or((0.0, 0.0));
        let (mx, my) = self.scroll_range(w, id);
        let (dx, dy) = if my == 0.0 && mx > 0.0 && dx == 0.0 { (dy, 0.0) } else { (dx, dy) };
        let (nx, ny) = ((sx + dx).clamp(0.0, mx), (sy + dy).clamp(0.0, my));
        ((nx, ny) != (sx, sy)).then_some((sx + dx, sy + dy))
    }

    pub fn wheel(&mut self, w: WindowId, x: f32, y: f32, dx: f32, dy: f32, m: &mut dyn Measure) {
        if !self.dispatch_wheel(w, x, y, dx, dy, &[]) {
            self.scroll_wheel(w, x, y, dx, dy, m);
        }
    }

    /// Scroll the nearest scroll container that can move in this direction.
    pub fn scroll_wheel(&mut self, w: WindowId, x: f32, y: f32, dx: f32, dy: f32, m: &mut dyn Measure) {
        let mut at = self.hit(w, x, y);
        while let Some(id) = at {
            if self.is_editor(id) && self.editor_wheel(id, dx, dy, m) {
                return;
            }
            if self.user_scrolls(id)
                && let Some((nx, ny)) = self.container_scroll_target(w, id, dx, dy)
                && self.set_scroll(w, id, nx, ny)
            {
                return;
            }
            at = self.nodes.get(&id).and_then(|n| n.parent);
        }
    }

    /// Scroll container id of window w to (x, y), clamped to its range: it
    /// repaints, and Norn hears if it asked (:scroll). False if it didn't
    /// move.
    fn set_scroll(&mut self, w: WindowId, id: NodeId, x: f32, y: f32) -> bool {
        let (mx, my) = self.scroll_range(w, id);
        let (sx, sy) = self.scroll.get(&id).copied().unwrap_or((0.0, 0.0));
        let (nx, ny) = (x.clamp(0.0, mx), y.clamp(0.0, my));
        if (nx, ny) == (sx, sy) {
            return false;
        }
        self.scroll.insert(id, (nx, ny));
        self.recollect(w);
        let win = self.windows.get_mut(&w).unwrap();
        let visible = win.rects[&id].intersect(&win.clips[&id]);
        win.damage.push(visible);
        if self.nodes[&id].wants("scroll") {
            let r = self.reported_scroll.entry(id).or_default();
            r.push_back((nx, ny));
            if r.len() > 16 {
                r.pop_front();
            }
            self.events.retain(|e| !(e.kind == "scroll" && e.get("node") == Some(&Data::Num(id as f64))));
            self.events.push(Event::new(
                "scroll",
                vec![("window", Data::Num(w as f64)), ("node", Data::Num(id as f64)), ("x", Data::Num(nx as f64)), ("y", Data::Num(ny as f64))],
            ));
        }
        true
    }

    /// The scroll bar under window point (x, y), if any: the node it
    /// scrolls, which way, and its thumb and track.
    fn scroll_bar_at(&self, w: WindowId, x: f32, y: f32) -> Option<(NodeId, bool, Rect, Rect)> {
        let win = self.windows.get(&w)?;
        let inside = |r: &Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
        let mut at = self.hit(w, x, y);
        while let Some(id) = at {
            let r = *win.rects.get(&id)?;
            let thumbs: Vec<(bool, Rect)> = if let Some(t) = self.editor_thumb(win, id) {
                vec![(true, t)]
            } else {
                self.scroll_thumbs(win, id).into_iter().map(|t| (t.h > t.w || t.x >= r.x + r.w - 8.5, t)).collect()
            };
            for (vertical, t) in thumbs {
                // the track: the strip along the edge the thumb runs in
                let track = if vertical { Rect { x: r.x + r.w - 10.0, y: r.y, w: 10.0, h: r.h } } else { Rect { x: r.x, y: r.y + r.h - 10.0, w: r.w, h: 10.0 } };
                if inside(&track) {
                    return Some((id, vertical, t, track));
                }
            }
            at = self.nodes.get(&id).and_then(|n| n.parent);
        }
        None
    }

    /// An editor's scroll bar thumb, when its text is taller than its view.
    pub fn editor_thumb(&self, win: &Window, id: NodeId) -> Option<Rect> {
        let e = self.editors.get(&id)?;
        let r = win.rects.get(&id)?;
        let total = e.line_count as f32 * e.line_h;
        if total <= r.h + 1.0 || r.h <= 20.0 {
            return None;
        }
        let th = (r.h * r.h / total).clamp(24.0, r.h);
        let at = (e.scroll_y / (total - r.h)).clamp(0.0, 1.0);
        Some(Rect { x: r.x + r.w - 8.0, y: r.y + at * (r.h - th), w: 6.0, h: th })
    }

    /// Move a dragged thumb so the pointer is at (x, y): scroll to match.
    fn drag_thumb(&mut self, d: ThumbDrag, x: f32, y: f32) {
        let Some(win) = self.windows.get(&d.window) else { return };
        let Some(&r) = win.rects.get(&d.node) else { return };
        let (pos, start, len, thumb) = if d.vertical { (y, r.y, r.h, d.thumb) } else { (x, r.x, r.w, d.thumb) };
        let room = (len - thumb).max(1.0);
        let frac = ((pos - d.grab - start) / room).clamp(0.0, 1.0);
        if let Some(e) = self.editors.get_mut(&d.node) {
            let total = e.line_count as f32 * e.line_h;
            let y = frac * (total - r.h).max(0.0);
            if y != e.scroll_y {
                e.scroll_y = y;
                self.damage_editor(d.node);
            }
            return;
        }
        let Ok(l) = self.layout.layout(self.nodes[&d.node].layout) else { return };
        let (sx, sy) = self.scroll.get(&d.node).copied().unwrap_or((0.0, 0.0));
        if d.vertical {
            self.set_scroll(d.window, d.node, sx, frac * (l.content_size.height - r.h).max(0.0));
        } else {
            self.set_scroll(d.window, d.node, frac * (l.content_size.width - r.w).max(0.0), sy);
        }
    }

    /// Scroll positions Norn set for window w's scroll containers.
    fn apply_pending_scroll(&mut self, w: WindowId) {
        let ids: Vec<NodeId> = self.pending_scroll.keys().copied().filter(|n| self.window_of(*n) == Some(w)).collect();
        let mut moved = Vec::new();
        for id in ids {
            let (x, y) = self.pending_scroll.remove(&id).unwrap();
            if !self.scrolls(id) || !self.windows[&w].rects.contains_key(&id) {
                continue;
            }
            let same = |a: Option<f32>, b: f32| a.is_none_or(|a| (a - b).abs() < 0.5);
            if self.reported_scroll.get(&id).is_some_and(|r| r.iter().any(|&(rx, ry)| same(x, rx) && same(y, ry))) {
                continue;
            }
            let (mx, my) = self.scroll_range(w, id);
            let (sx, sy) = self.scroll.get(&id).copied().unwrap_or((0.0, 0.0));
            let n = (x.unwrap_or(sx).clamp(0.0, mx), y.unwrap_or(sy).clamp(0.0, my));
            if n != (sx, sy) {
                self.scroll.insert(id, n);
                moved.push(id);
            }
        }
        if !moved.is_empty() {
            self.recollect(w);
            let win = self.windows.get_mut(&w).unwrap();
            for id in moved {
                let visible = win.rects[&id].intersect(&win.clips[&id]);
                win.damage.push(visible);
            }
        }
    }

    /// Scroll so node `id` is visible inside its scroll container, both
    /// ways (one bigger than the container shows its start).
    pub fn scroll_into_view(&mut self, id: NodeId) {
        let Some(w) = self.window_of(id) else { return };
        let mut at = self.nodes.get(&id).and_then(|n| n.parent);
        while let Some(p) = at {
            if self.user_scrolls(p) {
                let (Some(r), Some(pr)) = (self.windows[&w].rects.get(&id).copied(), self.windows[&w].rects.get(&p).copied()) else { return };
                let (sx, sy) = self.scroll.get(&p).copied().unwrap_or((0.0, 0.0));
                let off = |start: f32, len: f32, pstart: f32, plen: f32| {
                    if start < pstart {
                        start - pstart
                    } else if start + len > pstart + plen {
                        ((start + len) - (pstart + plen)).min(start - pstart)
                    } else {
                        0.0
                    }
                };
                let (dx, dy) = (off(r.x, r.w, pr.x, pr.w), off(r.y, r.h, pr.y, pr.h));
                if dx != 0.0 || dy != 0.0 {
                    self.set_scroll(w, p, sx + dx, sy + dy);
                }
                return;
            }
            at = self.nodes.get(&p).and_then(|n| n.parent);
        }
    }

    /// Node `id`'s rectangle in its window, from the last layout.
    pub fn rect(&self, id: NodeId) -> Option<Rect> {
        let w = self.window_of(id)?;
        self.windows[&w].rects.get(&id).copied()
    }

    // ---- input --------------------------------------------------------------------------------

    /// The deepest shown node at (x, y) in window w (later children on top).
    pub fn hit(&self, w: WindowId, x: f32, y: f32) -> Option<NodeId> {
        let win = self.windows.get(&w)?;
        let root = win.root?;
        fn walk(e: &Engine, win: &Window, id: NodeId, x: f32, y: f32) -> Option<NodeId> {
            let r = win.rects.get(&id)?;
            let clip = win.clips.get(&id).copied().unwrap_or(*r);
            if !r.intersect(&clip).contains(x, y) {
                return None;
            }
            for c in e.nodes[&id].children.iter().rev() {
                if let Some(hit) = walk(e, win, *c, x, y) {
                    return Some(hit);
                }
            }
            Some(id)
        }
        walk(self, win, root, x, y)
    }

    /// The mouse cursor for the pointer's position: the hovered node's (or
    /// an ancestor's) :cursor style; text in text fields.
    pub fn cursor(&self) -> &str {
        let Some((_, mut id)) = self.hovered else { return "default" };
        if self.drag.is_some_and(|d| d.4) {
            id = self.drag.unwrap().1;
        }
        loop {
            let Some(n) = self.nodes.get(&id) else { return "default" };
            if let Some(c) = n.resolved.iter().find(|(k, _)| k == "cursor").and_then(|(_, v)| v.as_key()) {
                return c;
            }
            if n.kind == "text-field" {
                return "text";
            }
            match n.parent {
                Some(p) => id = p,
                None => return "default",
            }
        }
    }

    /// The nearest node at or above `id` that wants event `e`.
    pub fn target(&self, mut id: NodeId, e: &str) -> Option<NodeId> {
        loop {
            let n = self.nodes.get(&id)?;
            if n.wants(e) && !n.prop("disabled").is_some_and(Data::truthy) {
                return Some(id);
            }
            id = n.parent?;
        }
    }

    fn set_state(&mut self, which: fn(&mut Engine) -> &mut Option<(WindowId, NodeId)>, v: Option<(WindowId, NodeId)>) {
        let old = std::mem::replace(which(self), v);
        if old == v {
            return;
        }
        // The state applies to the node and its ancestors: it changed only
        // where the old and new chains differ (moving within a box leaves
        // the box, and everything above it, as it was). Each of those is
        // restyled root-first; styles pass down, so a subtree is restyled
        // only as far as styles keep changing.
        let chain = |e: &Engine, n: Option<(WindowId, NodeId)>| -> Vec<NodeId> {
            let mut out = Vec::new();
            let mut at = n.map(|(_, id)| id);
            while let Some(id) = at {
                out.push(id);
                at = e.nodes.get(&id).and_then(|x| x.parent);
            }
            out.reverse();
            out
        };
        let (a, b) = (chain(self, old), chain(self, v));
        let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        for id in a[common..].iter().chain(&b[common..]).copied().collect::<Vec<_>>() {
            self.restyle_down(id);
        }
    }

    /// Restyle id and everything under it.
    fn restyle_subtree(&mut self, id: NodeId) {
        self.restyle_walk(id, true);
    }

    /// Restyle id, and its children while its style changes.
    fn restyle_down(&mut self, id: NodeId) {
        self.restyle_walk(id, false);
    }

    /// Each node's style is resolved once and handed to its children
    /// (resolving it per node from the root would cost the depth again).
    fn restyle_walk(&mut self, id: NodeId, all: bool) {
        if !self.nodes.contains_key(&id) {
            return;
        }
        let fresh = self.resolved_style(id);
        self.restyle_walk_as(id, fresh, all);
    }

    fn restyle_walk_as(&mut self, id: NodeId, fresh: Props, all: bool) {
        let changed = self.restyle_as(id, fresh.clone());
        if changed {
            self.repaint.insert(id);
        }
        if all || changed || self.anims.iter().any(|a| a.node == id) {
            let inherited: Props = fresh.into_iter().filter(|(k, _)| style::inherited(k)).collect();
            let kids = self.nodes.get(&id).map(|n| n.children.clone()).unwrap_or_default();
            for c in kids {
                if self.nodes.contains_key(&c) {
                    let s = self.style_from(c, inherited.clone());
                    self.restyle_walk_as(c, s, all);
                }
            }
        }
    }

    /// Pointer motion: only restyles (hover), or drag-selects in a text
    /// field; nothing crosses to Norn.
    pub fn pointer_moved(&mut self, w: WindowId, x: f32, y: f32, m: &mut dyn Measure) {
        if let Some(d) = self.thumb_drag {
            self.drag_thumb(d, x, y);
            return;
        }
        if let Some(f) = self.dragging {
            self.field_pointer(f, x, true, m);
            return;
        }
        if self.rich_move(x, y, m) {
            return;
        }
        if let Some((ed, from, last, _)) = self.editor_drag {
            let pos = self.editor_position(ed, x, y, m);
            if pos != last {
                self.editor_drag = Some((ed, from, pos, true));
                let me = Data::Num(ed as f64);
                self.events.retain(|e| !(e.kind == "drag-select" && e.get("node") == Some(&me)));
                self.events.push(Event::new(
                    "drag-select",
                    vec![
                        ("window", Data::Num(w as f64)),
                        ("node", me),
                        ("anchor", Data::List(vec![Data::Num(from.0 as f64), Data::Num(from.1 as f64)])),
                        ("line", Data::Num(pos.0 as f64)),
                        ("col", Data::Num(pos.1 as f64)),
                    ],
                ));
            }
            return;
        }
        // Dragging a node that asked for :drag: movement since the last
        // event (coalesced), after a 3-pixel threshold.
        if let Some((dw, node, lx, ly, started)) = self.drag {
            let (dx, dy) = (x - lx, y - ly);
            if !started && dx.abs() < 3.0 && dy.abs() < 3.0 {
                return;
            }
            let me = Data::Num(node as f64);
            if !started {
                self.events.push(Event::new("drag-start", vec![("window", Data::Num(dw as f64)), ("node", me.clone())]));
            }
            let (mut sx, mut sy) = (dx, dy);
            let r = self.rect(node).unwrap_or_default();
            if let Some(i) = self.events.iter().position(|e| e.kind == "drag" && e.get("node") == Some(&me)) {
                let old = self.events.remove(i);
                sx += old.get("dx").and_then(Data::as_num).unwrap_or(0.0) as f32;
                sy += old.get("dy").and_then(Data::as_num).unwrap_or(0.0) as f32;
            }
            self.events.push(Event::new(
                "drag",
                vec![
                    ("window", Data::Num(dw as f64)),
                    ("node", me),
                    ("dx", Data::Num(sx as f64)),
                    ("dy", Data::Num(sy as f64)),
                    ("x", Data::Num(x as f64)),
                    ("y", Data::Num(y as f64)),
                    // the pointer in the dragged node's own coordinates
                    ("local-x", Data::Num((x - r.x) as f64)),
                    ("local-y", Data::Num((y - r.y) as f64)),
                ],
            ));
            self.drag = Some((dw, node, x, y, true));
            return;
        }
        let over = self.hit(w, x, y);
        self.set_state(|e| &mut e.hovered, over.map(|n| (w, n)));
        self.update_tooltip(w, x, y, over);
    }

    pub fn pointer_down(&mut self, w: WindowId, x: f32, y: f32, mods: &[&str], m: &mut dyn Measure) {
        // A scroll bar: drag its thumb, or press the track to jump there.
        if let Some((node, vertical, t, _)) = self.scroll_bar_at(w, x, y) {
            let (pos, tstart, tlen) = if vertical { (y, t.y, t.h) } else { (x, t.x, t.w) };
            let on_thumb = pos >= tstart && pos < tstart + tlen;
            let d = ThumbDrag { window: w, node, vertical, thumb: tlen, grab: if on_thumb { pos - tstart } else { tlen / 2.0 } };
            self.thumb_drag = Some(d);
            if !on_thumb {
                self.drag_thumb(d, x, y);
            }
            return;
        }
        let over = self.hit(w, x, y);
        self.hide_tooltip();
        self.dismiss_for_press(w, over);
        self.rich_down(over, x, y, mods.contains(&"shift"), m);
        self.set_state(|e| &mut e.pressed, over.map(|n| (w, n)));
        if let Some(id) = over.filter(|id| self.is_field(*id)) {
            self.field_pointer(id, x, mods.contains(&"shift"), m);
            self.dragging = Some(id);
        } else if let Some(id) = over.and_then(|id| self.target(id, "drag")) {
            self.drag = Some((w, id, x, y, false));
        }
        if let Some(id) = over.filter(|id| self.is_editor(*id)) {
            if self.focused != Some(id) {
                self.set_focus(id);
            }
            let pos = self.editor_position(id, x, y, m);
            self.editor_drag = Some((id, pos, pos, false));
        } else if let Some(id) = over.and_then(|id| self.focusable_at(id)) {
            self.set_focus(id);
        }
    }

    /// The nearest node at or above id that takes keyboard focus.
    fn focusable_at(&self, mut id: NodeId) -> Option<NodeId> {
        loop {
            let n = self.nodes.get(&id)?;
            if n.prop("focusable").is_some_and(Data::truthy) {
                return Some(id);
            }
            id = n.parent?;
        }
    }

    /// A release over the pressed node is a click, sent to the nearest node
    /// that wants `:click`.
    pub fn pointer_up(&mut self, w: WindowId, x: f32, y: f32, button: &str, count: u32, modifiers: &[&str]) {
        if self.thumb_drag.take().is_some() {
            return;
        }
        self.dragging = None;
        self.rich_up(w);
        let pressed_at = match self.editor_drag.take() {
            Some((_, _, _, true)) => {
                self.set_state(|e| &mut e.pressed, None);
                return;
            }
            Some((_, from, _, false)) => Some(from),
            None => None,
        };
        if let Some((dw, node, _, _, started)) = self.drag.take()
            && started
        {
            self.set_state(|e| &mut e.pressed, None);
            self.events.push(Event::new("drag-end", vec![("window", Data::Num(dw as f64)), ("node", Data::Num(node as f64))]));
            return;
        }
        let pressed = self.pressed;
        self.set_state(|e| &mut e.pressed, None);
        let Some(over) = self.hit(w, x, y) else { return };
        if pressed.map(|(_, n)| n) != Some(over) {
            return;
        }
        let ctrl_click = button == "left" && cfg!(target_os = "macos") && modifiers.contains(&"ctrl");
        if (button == "right" || ctrl_click) && self.context_click(w, x, y, over) {
            return;
        }
        if let Some(t) = self.target(over, "click") {
            let r = self.rect(t).unwrap_or_default();
            let mut fields = vec![
                ("window", Data::Num(w as f64)),
                ("node", Data::Num(t as f64)),
                ("button", Data::key(button)),
                ("count", Data::Num(count as f64)),
                ("x", Data::Num((x - r.x) as f64)),
                ("y", Data::Num((y - r.y) as f64)),
                ("modifiers", Data::Set(modifiers.iter().map(|m| Data::key(m)).collect())),
            ];
            if self.is_editor(t) {
                let (line, col) = pressed_at.unwrap_or_default();
                fields.push(("line", Data::Num(line as f64)));
                fields.push(("col", Data::Num(col as f64)));
            }
            self.events.push(Event::new("click", fields));
        }
    }

    /// A key: a focused text field edits with it; otherwise it goes to
    /// the focused node if that wants `:key`, else to the window.
    pub fn key(&mut self, w: WindowId, key: Data, modifiers: &[&str], repeat: bool, m: &mut dyn Measure) {
        self.hide_tooltip();
        if key.as_key() == Some("escape") && self.escape_popups(w) {
            return;
        }
        if self.field_key(&key, modifiers, m) {
            return;
        }
        let node = self.focused.and_then(|f| self.target(f, "key"));
        let mut fields = vec![
            ("window", Data::Num(w as f64)),
            ("key", key),
            ("modifiers", Data::Set(modifiers.iter().map(|m| Data::key(m)).collect())),
            ("repeat", Data::Bool(repeat)),
        ];
        if let Some(n) = node {
            fields.push(("node", Data::Num(n as f64)));
        }
        self.events.push(Event::new("key", fields));
    }

    /// The OS moved window w to (x, y) on screen (logical): tell Norn
    /// (coalesced), so apps can remember where their windows were.
    pub fn moved(&mut self, w: WindowId, x: f32, y: f32) {
        let me = Data::Num(w as f64);
        self.events.retain(|e| !(e.kind == "moved" && e.get("window") == Some(&me)));
        self.events.push(Event::new("moved", vec![("window", me), ("position", Data::List(vec![Data::Num(x as f64), Data::Num(y as f64)]))]));
    }

    /// The window changed size: lay it out again and tell Norn.
    pub fn resized(&mut self, w: WindowId, width: f32, height: f32, scale: f32) {
        let Some(win) = self.windows.get_mut(&w) else { return };
        win.size = (width, height);
        win.layout_dirty = true;
        win.damage.push(Rect { x: 0.0, y: 0.0, w: width, h: height });
        self.events.retain(|e| !(e.kind == "resize" && e.get("window") == Some(&Data::Num(w as f64))));
        self.events.push(Event::new(
            "resize",
            vec![
                ("window", Data::Num(w as f64)),
                ("size", Data::List(vec![Data::Num(width as f64), Data::Num(height as f64)])),
                ("scale", Data::Num(scale as f64)),
            ],
        ));
    }

    /// Show or hide a window (minimised, occluded): hidden windows draw nothing.
    pub fn set_visible(&mut self, w: WindowId, visible: bool) {
        if let Some(win) = self.windows.get_mut(&w) {
            if visible && !win.visible {
                win.damage.push(Rect { x: 0.0, y: 0.0, w: win.size.0, h: win.size.1 });
            }
            win.visible = visible;
        }
    }

    pub fn close_requested(&mut self, w: WindowId) {
        self.events.push(Event::new("close-requested", vec![("window", Data::Num(w as f64))]));
    }
}
