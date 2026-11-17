//! The data that crosses between `skein.ui` and the engine, in Rust form.
//! The extension (crates/skein-ui) converts Skein values to and from these on
//! a carrier, so the engine itself never touches the Skein heap and can run
//! on any thread (the main thread, for the macOS event loop).

use std::sync::Arc;

/// A Skein value, as far as the protocol needs one.
#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    Nil,
    Bool(bool),
    Num(f64),
    Str(String),
    /// A keyword, without the colon.
    Key(String),
    List(Vec<Data>),
    /// A map, in the order given (keys are usually keywords).
    Map(Vec<(Data, Data)>),
    Set(Vec<Data>),
    /// Immutable bytes shared by the node tree and paint items.
    Bytes(Arc<Vec<u8>>),
}

impl Data {
    pub fn key(k: &str) -> Data {
        Data::Key(k.to_owned())
    }
    pub fn str(s: &str) -> Data {
        Data::Str(s.to_owned())
    }
    pub fn as_num(&self) -> Option<f64> {
        if let Data::Num(n) = self { Some(*n) } else { None }
    }
    pub fn as_key(&self) -> Option<&str> {
        if let Data::Key(k) = self { Some(k) } else { None }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Data::Str(s) = self { Some(s) } else { None }
    }
    pub fn as_list(&self) -> Option<&[Data]> {
        if let Data::List(xs) = self { Some(xs) } else { None }
    }
    pub fn truthy(&self) -> bool {
        !matches!(self, Data::Nil | Data::Bool(false))
    }
    /// A map's value for keyword `k`.
    pub fn get(&self, k: &str) -> Option<&Data> {
        if let Data::Map(kvs) = self {
            kvs.iter().find(|(key, _)| key.as_key() == Some(k)).map(|(_, v)| v)
        } else {
            None
        }
    }
    /// Is `k` a member of this set (or list) of keywords?
    pub fn has_key(&self, k: &str) -> bool {
        match self {
            Data::Set(xs) | Data::List(xs) => xs.iter().any(|x| x.as_key() == Some(k)),
            Data::Key(x) => x == k,
            _ => false,
        }
    }
}

/// Properties or style entries: keyword name → value, in order.
pub type Props = Vec<(String, Data)>;

pub type NodeId = u64;
pub type WindowId = u64;

/// One patch op (docs/ui-protocol.md §4).
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Hello(u32),
    Window { id: WindowId, props: Props },
    CloseWindow(WindowId),
    Create { id: NodeId, kind: String, props: Props },
    Set { id: NodeId, props: Props },
    Style { id: NodeId, style: Props },
    Text { id: NodeId, text: String },
    Insert { parent: NodeId, index: usize, id: NodeId },
    Move { parent: NodeId, from: usize, to: usize },
    Remove(NodeId),
    Theme(Data),
    Focus(NodeId),
    Stats(u64),
    DecodeImage { req: u64, src: Data, size: Option<Data> },
    /// Editor-surface ops (protocol §6): line-count, lines, carets,
    /// decorations, reveal, invalidate.
    Editor { id: NodeId, op: String, args: Vec<Data> },
    /// Platform requests the backend carries out (protocol §4): menu,
    /// dialog, clipboard-set, clipboard-get.
    Platform { op: String, args: Vec<Data> },
}

/// An event for Skein: `{:event kind …fields}`.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub kind: &'static str,
    pub fields: Vec<(&'static str, Data)>,
}

impl Event {
    pub fn new(kind: &'static str, fields: Vec<(&'static str, Data)>) -> Event {
        Event { kind, fields }
    }
    pub fn get(&self, k: &str) -> Option<&Data> {
        self.fields.iter().find(|(key, _)| *key == k).map(|(_, v)| v)
    }
}

/// A rectangle in logical pixels, window coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = (self.x + self.w).min(o.x + o.w);
        let b = (self.y + self.h).min(o.y + o.h);
        Rect { x, y, w: (r - x).max(0.0), h: (b - y).max(0.0) }
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}
