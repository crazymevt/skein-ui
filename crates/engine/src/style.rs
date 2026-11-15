//! Style maps (docs/ui-protocol.md §5) to taffy layout styles and paint
//! properties, and themes: rules from selectors to style maps.

use crate::data::{Data, Props};
use taffy::prelude::*;

fn dim(v: &Data) -> Option<Dimension> {
    match v {
        Data::Num(n) => Some(length(*n as f32)),
        Data::Key(k) if k == "auto" => Some(auto()),
        Data::List(xs) if xs.len() == 2 && xs[0].as_key() == Some("%") => xs[1].as_num().map(|p| percent(p as f32 / 100.0)),
        _ => None,
    }
}

fn len_pct(v: &Data) -> Option<LengthPercentage> {
    match v {
        Data::Num(n) => Some(length(*n as f32)),
        Data::List(xs) if xs.len() == 2 && xs[0].as_key() == Some("%") => xs[1].as_num().map(|p| percent(p as f32 / 100.0)),
        _ => None,
    }
}

fn len_pct_auto(v: &Data) -> Option<LengthPercentageAuto> {
    match v {
        Data::Num(n) => Some(length(*n as f32)),
        Data::Key(k) if k == "auto" => Some(auto()),
        Data::List(xs) if xs.len() == 2 && xs[0].as_key() == Some("%") => xs[1].as_num().map(|p| percent(p as f32 / 100.0)),
        _ => None,
    }
}

/// CSS-style box sides: n, [vertical horizontal] or [top right bottom left].
fn sides<T: Copy>(v: &Data, one: impl Fn(&Data) -> Option<T>) -> Option<taffy::Rect<T>> {
    match v {
        Data::List(xs) if xs.len() == 2 && xs[0].as_key() != Some("%") => {
            let (a, b) = (one(&xs[0])?, one(&xs[1])?);
            Some(taffy::Rect { top: a, bottom: a, left: b, right: b })
        }
        Data::List(xs) if xs.len() == 4 => {
            Some(taffy::Rect { top: one(&xs[0])?, right: one(&xs[1])?, bottom: one(&xs[2])?, left: one(&xs[3])? })
        }
        _ => {
            let a = one(v)?;
            Some(taffy::Rect { top: a, bottom: a, left: a, right: a })
        }
    }
}

fn align(v: &Data) -> Option<AlignItems> {
    Some(match v.as_key()? {
        "start" | "flex-start" => AlignItems::FlexStart,
        "end" | "flex-end" => AlignItems::FlexEnd,
        "center" => AlignItems::Center,
        "stretch" => AlignItems::Stretch,
        "baseline" => AlignItems::Baseline,
        _ => return None,
    })
}

fn justify(v: &Data) -> Option<JustifyContent> {
    Some(match v.as_key()? {
        "start" | "flex-start" => JustifyContent::FlexStart,
        "end" | "flex-end" => JustifyContent::FlexEnd,
        "center" => JustifyContent::Center,
        "space-between" => JustifyContent::SpaceBetween,
        "space-around" => JustifyContent::SpaceAround,
        "space-evenly" => JustifyContent::SpaceEvenly,
        _ => return None,
    })
}

fn tracks(v: &Data) -> Option<Vec<TrackSizingFunction>> {
    let xs = v.as_list()?;
    xs.iter()
        .map(|t| match t {
            Data::Num(n) => Some(length(*n as f32)),
            Data::Key(k) if k == "auto" => Some(auto()),
            Data::List(p) if p.len() == 2 && p[0].as_key() == Some("fr") => p[1].as_num().map(|f| fr(f as f32)),
            Data::List(p) if p.len() == 2 && p[0].as_key() == Some("%") => p[1].as_num().map(|f| percent(f as f32 / 100.0)),
            _ => None,
        })
        .collect()
}

/// Apply style map entries to a taffy style. Unknown keys are paint or
/// text properties (handled elsewhere); a bad value is an error.
pub fn layout_style(entries: &Props) -> Result<Style, String> {
    let mut s = Style { display: Display::Flex, ..Style::default() };
    for (k, v) in entries {
        let bad = || format!("bad value for :{k}: {v:?}");
        match k.as_str() {
            "display" => {
                s.display = match v.as_key() {
                    Some("flex") => Display::Flex,
                    Some("grid") => Display::Grid,
                    Some("block") => Display::Block,
                    Some("none") => Display::None,
                    _ => return Err(bad()),
                }
            }
            "flex-direction" => {
                s.flex_direction = match v.as_key() {
                    Some("row") => FlexDirection::Row,
                    Some("column") => FlexDirection::Column,
                    Some("row-reverse") => FlexDirection::RowReverse,
                    Some("column-reverse") => FlexDirection::ColumnReverse,
                    _ => return Err(bad()),
                }
            }
            "flex-grow" => s.flex_grow = v.as_num().ok_or_else(bad)? as f32,
            "flex-shrink" => s.flex_shrink = v.as_num().ok_or_else(bad)? as f32,
            "flex-basis" => s.flex_basis = dim(v).ok_or_else(bad)?,
            "flex-wrap" => s.flex_wrap = if v.truthy() && v.as_key() != Some("nowrap") { FlexWrap::Wrap } else { FlexWrap::NoWrap },
            "align-items" => s.align_items = Some(align(v).ok_or_else(bad)?),
            "align-self" => s.align_self = Some(align(v).ok_or_else(bad)?),
            "justify-content" => s.justify_content = Some(justify(v).ok_or_else(bad)?),
            "gap" => {
                let (r, c) = match v {
                    Data::List(xs) if xs.len() == 2 => (len_pct(&xs[0]).ok_or_else(bad)?, len_pct(&xs[1]).ok_or_else(bad)?),
                    _ => {
                        let g = len_pct(v).ok_or_else(bad)?;
                        (g, g)
                    }
                };
                s.gap = Size { width: c, height: r };
            }
            "padding" => s.padding = sides(v, len_pct).ok_or_else(bad)?,
            "margin" => s.margin = sides(v, len_pct_auto).ok_or_else(bad)?,
            "inset" => s.inset = sides(v, len_pct_auto).ok_or_else(bad)?,
            "width" => s.size.width = dim(v).ok_or_else(bad)?,
            "height" => s.size.height = dim(v).ok_or_else(bad)?,
            "min-width" => s.min_size.width = dim(v).ok_or_else(bad)?,
            "min-height" => s.min_size.height = dim(v).ok_or_else(bad)?,
            "max-width" => s.max_size.width = dim(v).ok_or_else(bad)?,
            "max-height" => s.max_size.height = dim(v).ok_or_else(bad)?,
            "position" => {
                s.position = match v.as_key() {
                    Some("relative") => Position::Relative,
                    Some("absolute") => Position::Absolute,
                    _ => return Err(bad()),
                }
            }
            "grid-template-columns" => s.grid_template_columns = tracks(v).ok_or_else(bad)?.into_iter().map(Into::into).collect(),
            "grid-template-rows" => s.grid_template_rows = tracks(v).ok_or_else(bad)?.into_iter().map(Into::into).collect(),
            "overflow" => {
                let o = match v.as_key() {
                    Some("hidden") => taffy::Overflow::Hidden,
                    Some("scroll") => taffy::Overflow::Scroll,
                    _ => taffy::Overflow::Visible,
                };
                s.overflow = taffy::Point { x: o, y: o };
            }
            _ => {}
        }
    }
    Ok(s)
}

/// Built-in styles for node kinds, under the theme: a scroll container is a
/// column that scrolls.
pub fn kind_defaults(kind: &str) -> Props {
    match kind {
        "scroll" => vec![("flex-direction".into(), Data::key("column")), ("overflow".into(), Data::key("scroll"))],
        "button" => vec![("align-items".into(), Data::key("center")), ("justify-content".into(), Data::key("center"))],
        "editor" => vec![("font-family".into(), Data::key("code")), ("overflow".into(), Data::key("hidden")), ("flex-grow".into(), Data::Num(1.0))],
        _ => Vec::new(),
    }
}

/// Text properties, which children inherit unless they set their own.
pub fn inherited(key: &str) -> bool {
    matches!(key, "color" | "font-family" | "font-size" | "font-weight" | "font-style" | "line-height" | "text-align")
}

/// Layout properties, which need a new layout when they change (the rest
/// only repaint).
pub fn affects_layout(key: &str) -> bool {
    matches!(
        key,
        "display" | "flex-direction" | "flex-grow" | "flex-shrink" | "flex-basis" | "flex-wrap" | "align-items" | "align-self"
            | "justify-content" | "gap" | "padding" | "margin" | "inset" | "width" | "height" | "min-width" | "min-height"
            | "max-width" | "max-height" | "position" | "grid-template-columns" | "grid-template-rows" | "overflow"
            | "font-size" | "font-family" | "font-weight" | "line-height"
    )
}

/// A theme: rules from selectors `[kind class state]` (any part `:*`) to
/// style maps; later rules win. Tokens name values used in styles.
#[derive(Clone, Debug, Default)]
pub struct Theme {
    pub tokens: Vec<(String, Data)>,
    pub rules: Vec<(Selector, Props)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    pub kind: Option<String>,
    pub class: Option<String>,
    pub state: Option<String>,
}

fn part(d: Option<&Data>) -> Option<String> {
    d.and_then(|x| x.as_key()).filter(|k| *k != "*").map(str::to_owned)
}

pub fn props_of(d: &Data) -> Result<Props, String> {
    match d {
        Data::Map(kvs) => kvs
            .iter()
            .map(|(k, v)| k.as_key().map(|k| (k.to_owned(), v.clone())).ok_or_else(|| format!("expected keyword keys, got {k:?}")))
            .collect(),
        Data::Nil => Ok(Vec::new()),
        _ => Err(format!("expected a map, got {d:?}")),
    }
}

impl Theme {
    pub fn from_data(d: &Data) -> Result<Theme, String> {
        let tokens = d.get("tokens").map(props_of).transpose()?.unwrap_or_default();
        let mut rules = Vec::new();
        if let Some(rs) = d.get("rules") {
            for r in rs.as_list().ok_or("theme :rules must be a vector")? {
                let pair = r.as_list().filter(|p| p.len() == 2).ok_or("a theme rule is [selector style]")?;
                let sel = pair[0].as_list().ok_or("a selector is [kind class state]")?;
                rules.push((Selector { kind: part(sel.first()), class: part(sel.get(1)), state: part(sel.get(2)) }, props_of(&pair[1])?));
            }
        }
        Ok(Theme { tokens, rules })
    }

    /// The style a node gets from the theme: kind, classes and states.
    pub fn resolve(&self, kind: &str, classes: &[String], states: &[&str]) -> Props {
        let mut out: Props = Vec::new();
        for (sel, style) in &self.rules {
            let ok = sel.kind.as_deref().is_none_or(|k| k == kind)
                && sel.class.as_deref().is_none_or(|c| classes.iter().any(|x| x == c))
                && sel.state.as_deref().is_none_or(|s| states.contains(&s));
            if ok {
                merge(&mut out, style);
            }
        }
        out
    }

    /// A token's value, or the value itself.
    pub fn token<'a>(&'a self, v: &'a Data) -> &'a Data {
        if let Some(k) = v.as_key()
            && let Some((_, t)) = self.tokens.iter().find(|(n, _)| n == k)
        {
            return t;
        }
        v
    }
}

/// Merge entries into `into` (later wins; Nil removes).
pub fn merge(into: &mut Props, from: &Props) {
    for (k, v) in from {
        into.retain(|(x, _)| x != k);
        if *v != Data::Nil {
            into.push((k.clone(), v.clone()));
        }
    }
}

/// Does a style keep its text unwrapped (:white-space :pre or :nowrap)?
pub fn no_wrap(style: &[(String, Data)]) -> bool {
    style.iter().any(|(k, v)| k == "white-space" && matches!(v.as_key(), Some("pre" | "nowrap")))
}
