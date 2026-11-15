//! Style transitions: `:transition 150` (milliseconds) animates changes to
//! a node's background, color, border color and opacity; `:transition
//! [:background 150]` (or several such pairs) only those properties. A
//! change (a hover, a new prop) starts from what is on screen, so it never
//! jumps. While anything animates the engine wakes every frame; otherwise
//! it doesn't (the idle rules).

use crate::data::{Data, NodeId, Props};
use crate::engine::Engine;
use crate::text::{Rgba, color};
use std::time::{Duration, Instant};

pub const ANIMATABLE: [&str; 4] = ["background", "color", "border", "opacity"];
pub const FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Debug)]
pub struct Anim {
    pub node: NodeId,
    pub prop: String,
    pub from: Data,
    pub to: Data,
    /// The style value to leave in place at the end (a theme token, say).
    pub target: Data,
    pub start: Instant,
    pub dur: Duration,
}

/// How long each property of `style` transitions, from its :transition.
pub fn durations(style: &Props) -> Vec<(String, Duration)> {
    let Some((_, t)) = style.iter().find(|(k, _)| k == "transition") else { return Vec::new() };
    let ms = |d: &Data| d.as_num().map(|n| Duration::from_millis(n.max(0.0) as u64));
    match t {
        Data::Num(_) => ANIMATABLE.iter().map(|p| (p.to_string(), ms(t).unwrap())).collect(),
        Data::List(xs) => xs
            .chunks(2)
            .filter_map(|c| Some((c.first()?.as_key()?.to_owned(), ms(c.get(1)?)?)))
            .collect(),
        _ => Vec::new(),
    }
}

fn get<'a>(p: &'a Props, k: &str) -> Option<&'a Data> {
    p.iter().find(|(x, _)| x == k).map(|(_, v)| v)
}

fn set(p: &mut Props, k: &str, v: Data) {
    match p.iter_mut().find(|(x, _)| x == k) {
        Some(e) => e.1 = v,
        None => p.push((k.to_owned(), v)),
    }
}

fn rgba(c: Rgba) -> Data {
    Data::List(c.iter().map(|x| Data::Num(*x as f64)).collect())
}

/// a..b at t (0..1): colors, numbers, and borders [width color] of the
/// same width. None if they can't be blended.
fn blend(a: &Data, b: &Data, t: f32) -> Option<Data> {
    let mix = |x: f64, y: f64| x + (y - x) * t as f64;
    match (a, b) {
        (Data::Num(x), Data::Num(y)) => Some(Data::Num(mix(*x, *y))),
        (Data::List(x), Data::List(y)) if x.len() == 2 && y.len() == 2 && x[0] == y[0] => {
            Some(Data::List(vec![x[0].clone(), blend(&x[1], &y[1], t)?]))
        }
        _ => {
            let (ca, cb) = (color(a)?, color(b)?);
            let c: Vec<u8> = (0..4).map(|i| mix(ca[i] as f64, cb[i] as f64).round() as u8).collect();
            Some(rgba([c[0], c[1], c[2], c[3]]))
        }
    }
}

impl Engine {
    /// Theme tokens looked up, inside borders too.
    fn concrete(&self, d: &Data) -> Data {
        match d {
            Data::List(xs) if xs.len() == 2 => Data::List(vec![xs[0].clone(), self.theme.token(&xs[1]).clone()]),
            _ => self.theme.token(d).clone(),
        }
    }

    /// Node id's style goes from `old` (on screen) to `new`: start
    /// transitions for what changed, leaving the starting values in `new`.
    pub(crate) fn start_transitions(&mut self, id: NodeId, old: &Props, new: &mut Props) {
        let now = Instant::now();
        for (prop, dur) in durations(new) {
            let (Some(o), Some(n)) = (get(old, &prop).cloned(), get(new, &prop).cloned()) else { continue };
            let running = self.anims.iter().position(|a| a.node == id && a.prop == prop);
            if let Some(i) = running
                && self.anims[i].target == n
            {
                set(new, &prop, o);
                continue;
            }
            if o == n {
                continue;
            }
            let (from, to) = (self.concrete(&o), self.concrete(&n));
            if dur.is_zero() || blend(&from, &to, 0.0).is_none() {
                continue;
            }
            if let Some(i) = running {
                self.anims.remove(i);
            }
            self.anims.push(Anim { node: id, prop: prop.clone(), from, to, target: n, start: now, dur });
            set(new, &prop, o);
        }
    }

    /// A frame after the last one (not after now: asked again and again
    /// as events come, "a frame from now" would never arrive).
    pub(crate) fn anim_wakeup(&self) -> Option<Instant> {
        (!self.anims.is_empty()).then(|| self.anim_at.map_or_else(Instant::now, |t| t + FRAME))
    }

    /// Advance the transitions to `now`: each repaints its node.
    pub(crate) fn anim_tick(&mut self, now: Instant) {
        if self.anims.is_empty() {
            self.anim_at = None;
            return;
        }
        self.anim_at = Some(now);
        let anims = std::mem::take(&mut self.anims);
        for a in anims {
            let t = (now.duration_since(a.start).as_secs_f32() / a.dur.as_secs_f32()).min(1.0);
            let done = t >= 1.0 || !self.nodes.contains_key(&a.node);
            let v = if done { a.target.clone() } else { blend(&a.from, &a.to, t).unwrap_or(a.target.clone()) };
            if let Some(n) = self.nodes.get_mut(&a.node) {
                set(&mut n.resolved, &a.prop, v);
            }
            if let (Some(w), Some(r)) = (self.window_of(a.node), self.rect(a.node)) {
                self.windows.get_mut(&w).unwrap().damage.push(r);
            }
            if !done {
                self.anims.push(a);
            }
        }
    }
}
