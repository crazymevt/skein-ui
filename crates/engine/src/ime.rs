//! IME composition: the text an input method is composing (the preedit)
//! shows at the caret of the focused field or editor, underlined, before
//! it is committed. Commits arrive as ordinary text input. The backend
//! asks `ime_area` where to put the candidate window.

use crate::data::{NodeId, Rect};
use crate::engine::Engine;

/// The composition in progress: the node, its text, and the cursor within
/// it (characters), if the input method gave one.
#[derive(Clone, Debug, PartialEq)]
pub struct Preedit {
    pub node: NodeId,
    pub text: String,
    pub cursor: Option<usize>,
}

impl Engine {
    /// The input method's composition changed (empty text: it ended).
    /// `cursor` is a byte offset into `text`.
    pub fn ime_preedit(&mut self, text: &str, cursor: Option<usize>) {
        let node = self.focused.filter(|f| self.is_field(*f) || self.is_editor(*f));
        let new = match node {
            Some(n) if !text.is_empty() => Some(Preedit {
                node: n,
                text: text.to_owned(),
                cursor: cursor.map(|b| text[..b.min(text.len())].chars().count()),
            }),
            _ => None,
        };
        if new == self.preedit {
            return;
        }
        let touched: Vec<NodeId> = self.preedit.iter().chain(new.iter()).map(|p| p.node).collect();
        self.preedit = new;
        for n in touched {
            if let Some(r) = self.ime_damage(n)
                && let Some(w) = self.window_of(n)
            {
                self.windows.get_mut(&w).unwrap().damage.push(r);
            }
        }
    }

    /// What a composition in node n repaints: a field, or the editor line
    /// holding the primary caret.
    fn ime_damage(&self, n: NodeId) -> Option<Rect> {
        if self.is_editor(n) {
            let line = self.editors.get(&n)?.carets.last()?.line;
            self.editor_line_rect(n, line)
        } else {
            self.rect(n)
        }
    }

    /// The composition shown in node n, if any.
    pub fn preedit_in(&self, n: NodeId) -> Option<&Preedit> {
        self.preedit.as_ref().filter(|p| p.node == n)
    }

    /// Where the focused caret is (window coordinates), for the input
    /// method's candidate window.
    pub fn ime_area(&self, m: &mut dyn crate::engine::Measure) -> Option<(crate::data::WindowId, Rect)> {
        let f = self.focused?;
        let w = self.window_of(f)?;
        if let Some(field) = self.fields.get(&f) {
            return Some((w, field.caret_rect));
        }
        Some((w, self.editor_caret_rect(f, m)?))
    }

    /// The rectangle of editor id's primary caret, in window coordinates
    /// (for popups at the caret: completion, signatures).
    pub fn editor_caret_rect(&self, id: NodeId, m: &mut dyn crate::engine::Measure) -> Option<Rect> {
        let e = self.editors.get(&id)?;
        let c = e.carets.last()?;
        let r = self.rect(id)?;
        let top = r.y + c.line as f32 * e.line_h - e.scroll_y;
        let x = r.x + e.gutter_w + 4.0 - e.scroll_x + self.editor_col_x(id, c.line, c.col, m);
        Some(Rect { x, y: top, w: 2.0, h: e.line_h })
    }
}
