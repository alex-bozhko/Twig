//! The plain-text viewer: monospaced lines with a line-number gutter.

use super::content::TEXT_LIMIT;
use super::{Ctx, Pref, Viewer};
use crate::util::{format_count, format_size};
use crate::win::{draw_text, fill, font_height, scale, text_width, wash, DT_CENTER_V, DT_RIGHT_V};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::*;

const WHEEL_LINES: i32 = 3;

pub struct TextView {
    lines: Vec<Vec<u16>>,
    max_cols: usize,
    truncated: bool,
    total: u64,
    top: usize,
    left: i32,
    // Metrics from the last paint, used for scrolling.
    line_h: i32,
    char_w: i32,
    view: (i32, i32),
}

impl TextView {
    pub fn new(lines: Vec<Vec<u16>>, max_cols: usize, truncated: bool, total: u64) -> TextView {
        TextView { lines, max_cols, truncated, total, top: 0, left: 0, line_h: 16, char_w: 8, view: (0, 0) }
    }

    fn visible_lines(&self) -> usize {
        (self.view.1 / self.line_h.max(1)).max(1) as usize
    }

    fn max_top(&self) -> usize {
        self.lines.len().saturating_sub(self.visible_lines())
    }

    fn max_left(&self) -> i32 {
        (self.max_cols as i32 * self.char_w + self.char_w - self.view.0).max(0)
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
        self.char_w = char_w;

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

        let digits = self.lines.len().max(1).to_string().len().max(2) as i32;
        let gutter = digits * char_w + 2 * pad;
        let text_left = body.left + gutter + pad;
        self.view = (body.right - text_left, body.bottom - body.top - pad);
        self.top = self.top.min(self.max_top());
        self.left = self.left.clamp(0, self.max_left());

        fill(hdc, &RECT { right: body.left + gutter, ..body }, pal.toolbar);
        unsafe {
            let old = SelectObject(hdc, cx.mono);
            SetBkMode(hdc, TRANSPARENT);
            let first = self.top;
            let count = ((body.bottom - body.top) / line_h + 2) as usize;
            for (i, line) in self.lines.iter().enumerate().skip(first).take(count) {
                let y = body.top + pad / 2 + (i - first) as i32 * line_h;
                let nr = RECT { left: body.left, top: y, right: body.left + gutter - pad, bottom: y + line_h };
                draw_text(hdc, &(i + 1).to_string(), &nr, pal.text_dim, DT_RIGHT_V);
                if !line.is_empty() {
                    let saved = SaveDC(hdc);
                    IntersectClipRect(hdc, text_left - pad / 2, body.top, body.right, body.bottom);
                    SetTextColor(hdc, pal.text);
                    let _ = TextOutW(hdc, text_left - self.left, y + scale(1, cx.dpi), line);
                    let _ = RestoreDC(hdc, saved);
                }
            }
            SelectObject(hdc, old);
        }

        // Thin scroll position indicators.
        let thick = scale(3, cx.dpi);
        let edge = scale(2, cx.dpi);
        let n = self.lines.len();
        let vis = self.visible_lines();
        if n > vis {
            let track = body.bottom - body.top - 2 * edge;
            let len = ((track as i64 * vis as i64 / n as i64) as i32).max(scale(24, cx.dpi)).min(track);
            let pos = ((track - len) as i64 * self.top as i64 / self.max_top().max(1) as i64) as i32;
            let x = body.right - edge - thick;
            wash(hdc, &RECT { left: x, top: body.top + edge + pos, right: x + thick, bottom: body.top + edge + pos + len }, pal.text_dim, 140);
        }
        let content_w = self.max_cols as i32 * char_w + char_w;
        if content_w > self.view.0 && self.view.0 > 0 {
            let track = body.right - text_left - 2 * edge;
            let len = ((track as i64 * self.view.0 as i64 / content_w as i64) as i32).max(scale(24, cx.dpi)).min(track);
            let pos = ((track - len) as i64 * self.left as i64 / self.max_left().max(1) as i64) as i32;
            let y = body.bottom - edge - thick;
            wash(hdc, &RECT { left: text_left + edge + pos, top: y, right: text_left + edge + pos + len, bottom: y + thick }, pal.text_dim, 140);
        }
    }

    fn wheel(&mut self, delta: i32, horizontal: bool) -> bool {
        if horizontal {
            let step = self.char_w * 8 * delta.signum();
            let left = (self.left + step).clamp(0, self.max_left());
            let changed = left != self.left;
            self.left = left;
            changed
        } else {
            let lines = (delta.abs() * WHEEL_LINES / 120).max(1) as usize;
            let top = if delta > 0 { self.top.saturating_sub(lines) } else { (self.top + lines).min(self.max_top()) };
            let changed = top != self.top;
            self.top = top;
            changed
        }
    }

    fn subtitle(&self) -> String {
        let n = self.lines.len();
        let more = if self.truncated { "+" } else { "" };
        format!("{}{more} {} · {}", format_count(n), if n == 1 { "line" } else { "lines" }, format_size(self.total))
    }
}
