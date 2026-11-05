//! The plain-text viewer: monospaced lines with a line-number gutter, wrapped to the window width.

use super::content::TEXT_LIMIT;
use super::{Ctx, Pref, Viewer};
use crate::util::{format_count, format_size};
use crate::win::{draw_text, fill, font_height, pt_in, scale, text_width, wash, DT_CENTER_V, DT_RIGHT_V};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::*;

const WHEEL_LINES: i32 = 3;
const SPACE: u16 = b' ' as u16;
// Scroll bar, in 96-DPI pixels: the strip that takes the mouse, and the thumb's thickness at
// rest and when the bar is hovered or dragged.
const BAR_ZONE: i32 = 16;
const THUMB_THIN: i32 = 4;
const THUMB_WIDE: i32 = 8;
const THUMB_MIN: i32 = 24;

/// Scroll bar geometry from the last paint (client coordinates).
#[derive(Clone, Copy)]
struct Bar {
    zone: RECT,
    /// Thumb offset from the zone's top, and length.
    pos: i32,
    len: i32,
}

/// One displayed row: a slice of a source line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Row {
    line: usize,
    start: usize,
    end: usize,
}

pub struct TextView {
    lines: Vec<Vec<u16>>,
    truncated: bool,
    total: u64,
    rows: Vec<Row>,
    /// Columns `rows` was wrapped to (0: not yet).
    cols: usize,
    /// First visible row.
    top: usize,
    // Metrics from the last paint, used for scrolling.
    line_h: i32,
    view_h: i32,
    bar: Option<Bar>,
    bar_hot: bool,
    /// Dragging the thumb: where in it the mouse grabbed.
    grab: Option<i32>,
}

/// Splits a line into rows at most `cols` wide, breaking after a space where one follows
/// some text in the row, otherwise mid-word.
fn wrap(line: &[u16], cols: usize) -> Vec<(usize, usize)> {
    let cols = cols.max(1);
    let mut out = Vec::new();
    let mut s = 0;
    while line.len() - s > cols {
        let lim = s + cols;
        let text_from = line[s..lim].iter().position(|&c| c != SPACE).map_or(lim, |i| s + i);
        let end = (text_from + 2..=lim).rev().find(|&p| line[p - 1] == SPACE && line[p] != SPACE).unwrap_or(lim);
        out.push((s, end));
        s = end;
    }
    out.push((s, line.len()));
    out
}

impl TextView {
    pub fn new(lines: Vec<Vec<u16>>, truncated: bool, total: u64) -> TextView {
        TextView { lines, truncated, total, rows: Vec::new(), cols: 0, top: 0, line_h: 16, view_h: 0, bar: None, bar_hot: false, grab: None }
    }

    /// Width of the line-number gutter.
    fn gutter(&self, char_w: i32, pad: i32) -> i32 {
        let digits = self.lines.len().max(1).to_string().len().max(2) as i32;
        digits * char_w + 2 * pad
    }

    /// Scrolls so the thumb's top sits at `pos` along the bar.
    fn drag_to(&mut self, b: &Bar, pos: i32) {
        let room = (b.zone.bottom - b.zone.top - b.len).max(1);
        let pos = pos.clamp(0, room);
        self.top = ((pos as i64 * self.max_top() as i64 + room as i64 / 2) / room as i64) as usize;
    }

    /// Re-wraps for `cols` columns, keeping the top line in view.
    fn rewrap(&mut self, cols: usize) {
        if cols == self.cols {
            return;
        }
        let top_line = self.rows.get(self.top).map_or(0, |r| r.line);
        self.rows.clear();
        for (i, l) in self.lines.iter().enumerate() {
            self.rows.extend(wrap(l, cols).into_iter().map(|(start, end)| Row { line: i, start, end }));
        }
        self.cols = cols;
        self.top = self.rows.iter().position(|r| r.line >= top_line).unwrap_or(0);
    }

    fn visible_rows(&self) -> usize {
        (self.view_h / self.line_h.max(1)).max(1) as usize
    }

    fn max_top(&self) -> usize {
        self.rows.len().saturating_sub(self.visible_rows())
    }

    fn note(&self) -> Option<String> {
        if !self.truncated {
            return None;
        }
        Some(if self.total > TEXT_LIMIT {
            format!("Showing the first {} of {}", format_size(TEXT_LIMIT), format_size(self.total))
        } else {
            format!("Showing the first {} lines", format_count(self.lines.len()))
        })
    }
}

impl Viewer for TextView {
    fn preferred(&self) -> Pref {
        Pref::Doc
    }

    fn paint(&mut self, hdc: HDC, r: &RECT, cx: &Ctx) {
        let pal = cx.pal;
        fill(hdc, r, pal.bg);
        let pad = scale(8, cx.dpi);
        let line_h = font_height(cx.mono) + scale(2, cx.dpi);
        let char_w = text_width(hdc, cx.mono, "0").max(1);
        self.line_h = line_h;

        let mut body = *r;
        if let Some(note) = self.note() {
            let nh = cx.fonts.ui_height + pad;
            let nr = RECT { top: r.bottom - nh, ..*r };
            fill(hdc, &nr, pal.toolbar);
            fill(hdc, &RECT { bottom: nr.top + 1, ..nr }, pal.border);
            unsafe { SelectObject(hdc, cx.fonts.ui) };
            draw_text(hdc, &note, &nr, pal.text_dim, DT_CENTER_V);
            body.bottom = nr.top;
        }

        let gutter = self.gutter(char_w, pad);
        let text_left = body.left + gutter + pad;
        self.rewrap(((body.right - 2 * pad - text_left) / char_w).max(1) as usize);
        self.view_h = body.bottom - body.top - pad;
        self.top = self.top.min(self.max_top());

        fill(hdc, &RECT { right: body.left + gutter, ..body }, pal.toolbar);
        unsafe {
            let old = SelectObject(hdc, cx.mono);
            SetBkMode(hdc, TRANSPARENT);
            let count = ((body.bottom - body.top) / line_h + 2) as usize;
            for (i, row) in self.rows.iter().skip(self.top).take(count).enumerate() {
                let y = body.top + pad / 2 + i as i32 * line_h;
                if row.start == 0 {
                    let nr = RECT { left: body.left, top: y, right: body.left + gutter - pad, bottom: y + line_h };
                    draw_text(hdc, &(row.line + 1).to_string(), &nr, pal.text_dim, DT_RIGHT_V);
                }
                if row.end > row.start {
                    SetTextColor(hdc, pal.text);
                    let _ = TextOutW(hdc, text_left, y + scale(1, cx.dpi), &self.lines[row.line][row.start..row.end]);
                }
            }
            SelectObject(hdc, old);
        }

        // Scroll bar: a thin thumb that widens, over a faint track, when hovered or dragged.
        let edge = scale(2, cx.dpi);
        let n = self.rows.len();
        let vis = self.visible_rows();
        self.bar = None;
        if n > vis {
            let zone = RECT { left: body.right - scale(BAR_ZONE, cx.dpi), top: body.top + edge, right: body.right, bottom: body.bottom - edge };
            let track = zone.bottom - zone.top;
            let len = ((track as i64 * vis as i64 / n as i64) as i32).max(scale(THUMB_MIN, cx.dpi)).min(track);
            let pos = ((track - len) as i64 * self.top as i64 / self.max_top().max(1) as i64) as i32;
            let wide = self.bar_hot || self.grab.is_some();
            let thick = scale(if wide { THUMB_WIDE } else { THUMB_THIN }, cx.dpi);
            let x = body.right - scale(4, cx.dpi) - thick;
            if wide {
                let tr = RECT { left: x - edge, top: zone.top, right: x + thick + edge, bottom: zone.bottom };
                wash(hdc, &tr, pal.text_dim, 40);
            }
            let alpha = if self.grab.is_some() { 230 } else if wide { 190 } else { 140 };
            wash(hdc, &RECT { left: x, top: zone.top + pos, right: x + thick, bottom: zone.top + pos + len }, pal.text_dim, alpha);
            self.bar = Some(Bar { zone, pos, len });
        }
    }

    fn natural_width(&self, hdc: HDC, cx: &Ctx) -> Option<i32> {
        let pad = scale(8, cx.dpi);
        let char_w = text_width(hdc, cx.mono, "0").max(1);
        let longest = self.lines.iter().map(Vec::len).max().unwrap_or(0) as i32;
        Some(self.gutter(char_w, pad) + pad + longest * char_w + 2 * pad)
    }

    fn claims(&self, x: i32, y: i32) -> bool {
        self.bar.is_some_and(|b| pt_in(&b.zone, x, y))
    }

    fn press(&mut self, x: i32, y: i32) -> bool {
        let Some(b) = self.bar.filter(|b| pt_in(&b.zone, x, y)) else { return false };
        let thumb_top = b.zone.top + b.pos;
        if y < thumb_top || y >= thumb_top + b.len {
            // Off the thumb: center it on the click, then drag from there.
            self.drag_to(&b, y - b.zone.top - b.len / 2);
            self.grab = Some(b.len / 2);
        } else {
            self.grab = Some(y - thumb_top);
        }
        true
    }

    fn hover(&mut self, x: i32, y: i32) -> bool {
        if let (Some(g), Some(b)) = (self.grab, self.bar) {
            let top = self.top;
            self.drag_to(&b, y - b.zone.top - g);
            return top != self.top;
        }
        let hot = self.claims(x, y);
        std::mem::replace(&mut self.bar_hot, hot) != hot
    }

    fn release(&mut self, x: i32, y: i32) -> bool {
        let was = (self.grab.take().is_some(), self.bar_hot);
        self.bar_hot = self.claims(x, y);
        was != (false, self.bar_hot)
    }

    fn wheel(&mut self, delta: i32, horizontal: bool) -> bool {
        if horizontal {
            return false;
        }
        let rows = (delta.abs() * WHEEL_LINES / 120).max(1) as usize;
        let top = if delta > 0 { self.top.saturating_sub(rows) } else { (self.top + rows).min(self.max_top()) };
        let changed = top != self.top;
        self.top = top;
        changed
    }

    fn subtitle(&self) -> String {
        let n = self.lines.len();
        let more = if self.truncated { "+" } else { "" };
        format!("{}{more} {} · {}", format_count(n), if n == 1 { "line" } else { "lines" }, format_size(self.total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(s: &str, cols: usize) -> Vec<String> {
        let w: Vec<u16> = s.encode_utf16().collect();
        wrap(&w, cols).into_iter().map(|(a, b)| String::from_utf16_lossy(&w[a..b])).collect()
    }

    #[test]
    fn wraps_at_spaces() {
        assert_eq!(rows("short", 10), ["short"]);
        assert_eq!(rows("", 10), [""]);
        assert_eq!(rows("hello world foo", 8), ["hello ", "world ", "foo"]);
        assert_eq!(rows("ab cd", 3), ["ab ", "cd"]);
    }

    #[test]
    fn breaks_long_words_and_keeps_indent() {
        assert_eq!(rows("abcdefgh", 3), ["abc", "def", "gh"]);
        // Leading indentation is never split off on its own.
        assert_eq!(rows("    abcdefg", 6), ["    ab", "cdefg"]);
        assert_eq!(rows("  ab cdefgh", 6), ["  ab ", "cdefgh"]);
    }
}