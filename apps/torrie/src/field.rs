//! A one-line text field that edits like a browser's: click or drag to place and select, double-click a word
//! (triple: all), ⇧/⌥/⌘ arrows, ⌥/⌘ delete, Copy/Cut/Paste, Undo/Redo, and input methods (accents, emoji,
//! CJK) through gpui's input handler. Adapted from gpui's `examples/input.rs`.
//!
//! The owner keeps an `Entity<TextField>`, reads `text()`, listens for [`Edited`], and handles Enter, Esc and
//! Tab itself (they pass through).

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, GlobalElementId, KeyDownEvent, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, div, fill,
    point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::Theme;

/// The text changed by typing, pasting, cutting or undoing (not by [`TextField::set_text`]).
pub struct Edited;

pub struct TextField {
    focus: FocusHandle,
    text: String,
    placeholder: SharedString,
    sel: Range<usize>,
    /// The caret is at `sel.start` (the selection grew leftward).
    reversed: bool,
    /// Text an input method is still composing.
    marked: Option<Range<usize>>,
    /// Which characters it takes and how many; typed or pasted text is cut to fit.
    allow: fn(char) -> bool,
    max: usize,
    secret: bool,
    /// Right-aligned while it fits (numbers).
    right: bool,
    undo: Vec<(String, Range<usize>)>,
    redo: Vec<(String, Range<usize>)>,
    selecting: bool,
    /// How far the text is scrolled left so the caret stays in view.
    scroll: Pixels,
    line: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
}

impl EventEmitter<Edited> for TextField {}

impl TextField {
    pub fn new(
        text: impl Into<String>,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = text.into();
        Self {
            focus: cx.focus_handle(),
            sel: text.len()..text.len(),
            text,
            placeholder: placeholder.into(),
            reversed: false,
            marked: None,
            allow: |_| true,
            max: usize::MAX,
            secret: false,
            right: false,
            undo: vec![],
            redo: vec![],
            selecting: false,
            scroll: px(0.),
            line: None,
            bounds: None,
        }
    }

    /// Takes only characters passing `allow`, up to `max` of them.
    pub fn only(mut self, allow: fn(char) -> bool, max: usize) -> Self {
        (self.allow, self.max) = (allow, max);
        self
    }

    /// Digits only, up to `max` of them.
    pub fn digits(self, max: usize) -> Self {
        self.only(|c| c.is_ascii_digit(), max)
    }

    /// Right-aligned while it fits.
    pub fn right(mut self) -> Self {
        self.right = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the text (caret at the end) without emitting [`Edited`].
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        self.sel = self.text.len()..self.text.len();
        (self.marked, self.reversed) = (None, false);
        self.undo.clear();
        self.redo.clear();
        cx.notify();
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn caret(&self) -> usize {
        if self.reversed {
            self.sel.start
        } else {
            self.sel.end
        }
    }

    fn move_to(&mut self, i: usize, cx: &mut Context<Self>) {
        (self.sel, self.reversed) = (i..i, false);
        cx.notify();
    }

    fn select_to(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.reversed {
            self.sel.start = i;
        } else {
            self.sel.end = i;
        }
        if self.sel.end < self.sel.start {
            self.reversed = !self.reversed;
            self.sel = self.sel.end..self.sel.start;
        }
        cx.notify();
    }

    fn prev_char(&self, i: usize) -> usize {
        (self.text.grapheme_indices(true).rev())
            .find_map(|(j, _)| (j < i).then_some(j))
            .unwrap_or(0)
    }

    fn next_char(&self, i: usize) -> usize {
        (self.text.grapheme_indices(true))
            .find_map(|(j, _)| (j > i).then_some(j))
            .unwrap_or(self.text.len())
    }

    /// The start of the word at or before `i` (⌥←). A password is one word, as in a browser.
    fn prev_word(&self, i: usize) -> usize {
        if self.secret {
            return 0;
        }
        (self.words().rev())
            .find_map(|(j, _)| (j < i).then_some(j))
            .unwrap_or(0)
    }

    /// The end of the word at or after `i` (⌥→).
    fn next_word(&self, i: usize) -> usize {
        if self.secret {
            return self.text.len();
        }
        (self.words())
            .find_map(|(j, w)| (j + w.len() > i).then_some(j + w.len()))
            .unwrap_or(self.text.len())
    }

    /// Words, skipping spaces and punctuation, so ⌥→ in `a.b/c` stops after each part.
    fn words(&self) -> impl DoubleEndedIterator<Item = (usize, &str)> {
        (self.text.split_word_bound_indices()).filter(|(_, w)| w.chars().any(char::is_alphanumeric))
    }

    /// The word around `i`, for a double-click.
    fn word_at(&self, i: usize) -> Range<usize> {
        (self.text.split_word_bound_indices())
            .find(|(j, w)| i < j + w.len())
            .map_or(i..i, |(j, w)| j..j + w.len())
    }

    /// Replaces `range` with `new` (filtered to what the field takes), remembering the old text for Undo.
    fn replace(&mut self, range: Range<usize>, new: &str, cx: &mut Context<Self>) {
        let room = self
            .max
            .saturating_sub(self.text[..range.start].chars().count())
            .saturating_sub(self.text[range.end..].chars().count());
        let allow = self.allow;
        let new: String = (new.chars())
            .filter(|c| !c.is_control() && allow(*c))
            .take(room)
            .collect();
        if new.is_empty() && range.is_empty() {
            return;
        }
        self.undo.push((self.text.clone(), self.sel.clone()));
        self.redo.clear();
        self.text.replace_range(range.clone(), &new);
        self.move_to(range.start + new.len(), cx);
        self.marked = None;
        cx.emit(Edited);
    }

    /// Deletes the selection, or from the caret to `to` when there's none.
    fn delete_to(&mut self, to: usize, cx: &mut Context<Self>) {
        if self.sel.is_empty() {
            self.select_to(to, cx);
        }
        self.replace(self.sel.clone(), "", cx);
    }

    fn restore(&mut self, redo: bool, cx: &mut Context<Self>) {
        let (from, to) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let Some((text, sel)) = from.pop() else {
            return;
        };
        to.push((std::mem::replace(&mut self.text, text), self.sel.clone()));
        (self.sel, self.reversed, self.marked) = (sel, false, None);
        cx.emit(Edited);
        cx.notify();
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = k.modifiers;
        let (cmd, alt, shift) = (m.platform, m.alt, m.shift);
        let caret = self.caret();
        let go = |this: &mut Self, i: usize, cx: &mut Context<Self>| {
            if shift {
                this.select_to(i, cx)
            } else {
                this.move_to(i, cx)
            }
        };
        match k.key.as_str() {
            "left" | "right" | "up" | "down" | "home" | "end" if m.control => return,
            "left" if cmd => go(self, 0, cx),
            "left" if alt => go(self, self.prev_word(caret), cx),
            "left" if !shift && !self.sel.is_empty() => self.move_to(self.sel.start, cx),
            "left" => go(self, self.prev_char(caret), cx),
            "right" if cmd => go(self, self.text.len(), cx),
            "right" if alt => go(self, self.next_word(caret), cx),
            "right" if !shift && !self.sel.is_empty() => self.move_to(self.sel.end, cx),
            "right" => go(self, self.next_char(caret), cx),
            "home" | "up" => go(self, 0, cx),
            "end" | "down" => go(self, self.text.len(), cx),
            "backspace" if cmd => self.delete_to(0, cx),
            "backspace" if alt => self.delete_to(self.prev_word(caret), cx),
            "backspace" => self.delete_to(self.prev_char(caret), cx),
            "delete" if alt => self.delete_to(self.next_word(caret), cx),
            "delete" => self.delete_to(self.next_char(caret), cx),
            "a" if cmd => {
                (self.sel, self.reversed) = (0..self.text.len(), false);
                cx.notify();
            }
            "c" | "x" if cmd => {
                if self.sel.is_empty() || self.secret {
                    return;
                }
                cx.write_to_clipboard(ClipboardItem::new_string(
                    self.text[self.sel.clone()].to_owned(),
                ));
                if k.key == "x" {
                    self.replace(self.sel.clone(), "", cx);
                }
            }
            "v" if cmd => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    // A pasted multi-line list becomes one line.
                    self.replace(
                        self.sel.clone(),
                        &text.trim().replace(['\n', '\r', '\t'], " "),
                        cx,
                    );
                }
            }
            "z" if cmd => self.restore(shift, cx),
            "space" if cmd && m.control => window.show_character_palette(),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn index_at(&self, p: Point<Pixels>) -> usize {
        let (Some(b), Some(line)) = (self.bounds, self.line.as_ref()) else {
            return 0;
        };
        if self.text.is_empty() {
            return 0;
        }
        self.text_index(line.closest_index_for_x(p.x - b.left() - self.x0(line, b)))
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window);
        let i = self.index_at(ev.position);
        match ev.click_count {
            2 if !self.secret => {
                (self.sel, self.reversed) = (self.word_at(i), false);
                cx.notify();
            }
            2 | 3 => {
                (self.sel, self.reversed) = (0..self.text.len(), false);
                cx.notify();
            }
            _ if ev.modifiers.shift => self.select_to(i, cx),
            _ => {
                self.move_to(i, cx);
                self.selecting = true;
            }
        }
        // Keeps a window's click-outside blur from ending the edit being clicked into.
        cx.stop_propagation();
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.index_at(ev.position), cx);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    /// Where the shaped line starts inside the field: right-aligned numbers, else scrolled to the caret.
    fn x0(&self, line: &ShapedLine, b: Bounds<Pixels>) -> Pixels {
        if self.right && line.width < b.size.width {
            b.size.width - line.width
        } else {
            -self.scroll
        }
    }

    /// What's drawn: the text, or a dot per character.
    fn shown(&self) -> String {
        if self.secret {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }

    fn shown_index(&self, i: usize) -> usize {
        if self.secret {
            self.text[..i].chars().count() * '•'.len_utf8()
        } else {
            i
        }
    }

    fn text_index(&self, i: usize) -> usize {
        if !self.secret {
            return i;
        }
        (self.text.char_indices().map(|(j, _)| j))
            .nth(i / '•'.len_utf8())
            .unwrap_or(self.text.len())
    }

    fn to_utf16(&self, i: usize) -> usize {
        self.text[..i.min(self.text.len())].encode_utf16().count()
    }

    fn utf16_to_index(&self, n: usize) -> usize {
        utf16_to_byte(&self.text, n)
    }

    fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.utf16_to_index(r.start)..self.utf16_to_index(r.end)
    }
}

/// The byte offset in `s` of its `n`th UTF-16 unit (what macOS counts in).
fn utf16_to_byte(s: &str, n: usize) -> usize {
    let mut units = 0;
    for (j, c) in s.char_indices() {
        if units >= n {
            return j;
        }
        units += c.len_utf16();
    }
    s.len()
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        r: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let r = self.range_from_utf16(&r);
        actual.replace(self.to_utf16(r.start)..self.to_utf16(r.end));
        Some(self.text[r].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.to_utf16(self.sel.start)..self.to_utf16(self.sel.end),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (self.marked.as_ref()).map(|r| self.to_utf16(r.start)..self.to_utf16(r.end))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        r: Option<Range<usize>>,
        new: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let r = (r.as_ref().map(|r| self.range_from_utf16(r)))
            .or(self.marked.clone())
            .unwrap_or(self.sel.clone());
        self.replace(r, new, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        r: Option<Range<usize>>,
        new: &str,
        new_sel: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Composing text isn't filtered or undone; it lands through `replace_text_in_range` once committed.
        let r = (r.as_ref().map(|r| self.range_from_utf16(r)))
            .or(self.marked.clone())
            .unwrap_or(self.sel.clone());
        self.text.replace_range(r.clone(), new);
        self.marked = (!new.is_empty()).then(|| r.start..r.start + new.len());
        self.sel = match new_sel {
            Some(n) => r.start + utf16_to_byte(new, n.start)..r.start + utf16_to_byte(new, n.end),
            None => r.start + new.len()..r.start + new.len(),
        };
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        r: Range<usize>,
        b: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.line.as_ref()?;
        let r = self.range_from_utf16(&r);
        let x0 = b.left() + self.x0(line, b);
        Some(Bounds::from_corners(
            point(x0 + line.x_for_index(self.shown_index(r.start)), b.top()),
            point(x0 + line.x_for_index(self.shown_index(r.end)), b.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.bounds?.localize(&p)?;
        Some(self.to_utf16(self.index_at(p)))
    }
}

impl Render for TextField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Text")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .cursor(CursorStyle::IBeam)
            .size_full()
            .flex()
            .items_center()
            .overflow_hidden()
            .child(TextElement(cx.entity()))
    }
}

struct TextElement(Entity<TextField>);

struct Painted {
    line: ShapedLine,
    x0: Pixels,
    caret: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Painted;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        b: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Painted {
        let t = Theme::of(window, cx);
        let f = self.0.read(cx);
        let style = window.text_style();
        let empty = f.text.is_empty();
        let shown: SharedString = if empty {
            f.placeholder.clone()
        } else {
            f.shown().into()
        };
        let run = TextRun {
            len: shown.len(),
            font: style.font(),
            color: if empty { t.faint } else { style.color },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match f.marked.clone().filter(|_| !empty && !f.secret) {
            Some(m) => [
                TextRun {
                    len: m.start,
                    ..run.clone()
                },
                TextRun {
                    len: m.len(),
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: shown.len() - m.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![run],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(shown, font_size, &runs, None);

        // Scroll just enough to keep the caret in view; the placeholder never scrolls.
        let caret_x = if empty {
            px(0.)
        } else {
            line.x_for_index(f.shown_index(f.caret()))
        };
        let caret_w = px(1.5);
        let width = b.size.width;
        let max_scroll = (line.width + caret_w - width).max(px(0.));
        let mut scroll = f.scroll.min(max_scroll);
        if empty {
            scroll = px(0.);
        } else if caret_x - scroll > width - caret_w {
            scroll = caret_x - width + caret_w;
        } else if caret_x < scroll {
            scroll = caret_x;
        }
        let x0 = if f.right && line.width < width {
            width - line.width
        } else {
            -scroll
        };
        let left = b.left() + x0;
        let (h, top) = (
            px(16.).min(b.size.height),
            b.top() + (b.size.height - px(16.).min(b.size.height)) / 2.,
        );
        let (caret, selection) = if f.sel.is_empty() {
            let caret_x = if f.right && empty {
                width - caret_w
            } else {
                x0 + caret_x
            };
            (
                Some(fill(
                    Bounds::new(point(b.left() + caret_x, top), size(caret_w, h)),
                    t.accent,
                )),
                None,
            )
        } else {
            let (s, e) = (
                line.x_for_index(f.shown_index(f.sel.start)),
                line.x_for_index(f.shown_index(f.sel.end)),
            );
            (
                None,
                Some(fill(
                    Bounds::from_corners(point(left + s, top), point(left + e, top + h)),
                    t.tint(t.accent),
                )),
            )
        };
        self.0.update(cx, |f, _| f.scroll = scroll);
        Painted {
            line,
            x0,
            caret,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        b: Bounds<Pixels>,
        _: &mut (),
        p: &mut Painted,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.0.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(b, self.0.clone()), cx);
        let focused = focus.is_focused(window);
        if focused && let Some(s) = p.selection.take() {
            window.paint_quad(s);
        }
        _ = p.line.paint(
            point(b.left() + p.x0, b.top()),
            window.line_height(),
            window,
            cx,
        );
        if focused && let Some(c) = p.caret.take() {
            window.paint_quad(c);
        }
        let line = p.line.clone();
        self.0.update(cx, |f, _| {
            f.line = Some(line);
            f.bounds = Some(b);
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn utf16() {
        // "é" is one unit, "😀" two; offsets past the end clamp.
        let s = "é😀a";
        assert_eq!(super::utf16_to_byte(s, 1), 2);
        assert_eq!(super::utf16_to_byte(s, 3), 6);
        assert_eq!(super::utf16_to_byte(s, 9), s.len());
    }
}
