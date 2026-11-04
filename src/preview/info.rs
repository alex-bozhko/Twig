//! The info card: icon or thumbnail, name and properties. Shown for items that have no
//! richer preview, and as the placeholder while a preview loads.

use super::content::{Pixels, Target};
use super::image::{fit, Bitmap};
use super::{Ctx, Pref, Viewer};
use crate::model::Kind;
use crate::util::{format_size, parent_path};
use crate::win::{draw_icon, draw_text, fill, scale, DT_LEFT_V};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::*;

const MARGIN: i32 = 24;
const THUMB: i32 = 128;
const LABEL_W: i32 = 92;
const ROW: i32 = 24;

pub struct InfoView {
    target: Target,
    thumb: Option<Bitmap>,
    rows: Vec<(String, String)>,
    note: Option<String>,
}

impl InfoView {
    pub fn new(target: &Target, thumb: Option<Pixels>, extra: Vec<(String, String)>, note: Option<String>) -> InfoView {
        let mut rows = vec![("Kind".to_string(), kind_name(target))];
        if matches!(target.kind, Kind::File | Kind::Zip | Kind::ZipFile) {
            rows.push(("Size".into(), format!("{} ({} bytes)", format_size(target.size), crate::util::format_count(target.size as usize))));
        }
        let modified = crate::grid::format_time(target.modified);
        if !modified.is_empty() {
            rows.push(("Modified".into(), modified));
        }
        rows.extend(extra);
        if let Some(parent) = parent_path(&target.path) {
            rows.push(("Location".into(), parent));
        }
        InfoView { target: target.clone(), thumb: thumb.map(Bitmap::new), rows, note }
    }

    /// The placeholder shown while the real preview loads.
    pub fn loading(target: &Target) -> InfoView {
        let mut v = InfoView::new(target, None, Vec::new(), None);
        v.note = Some("Loading…".into());
        v
    }
}

fn kind_name(t: &Target) -> String {
    match t.kind {
        Kind::Dir => "Folder".into(),
        Kind::ZipDir => "Folder in archive".into(),
        _ if !t.type_name.is_empty() => t.type_name.to_string(),
        Kind::Drive => "Drive".into(),
        _ => "File".into(),
    }
}

impl Viewer for InfoView {
    fn preferred(&self) -> Pref {
        // Height the content needs at 96 DPI.
        let rows = self.rows.len() as i32 + 2;
        let note = if self.note.is_some() { 60 } else { 0 };
        Pref::Card(2 * MARGIN + (rows * ROW).max(THUMB) + note)
    }

    fn paint(&mut self, hdc: HDC, r: &RECT, cx: &Ctx) {
        let pal = cx.pal;
        let s = |v| scale(v, cx.dpi);
        fill(hdc, r, pal.bg);
        let (m, tsz) = (s(MARGIN), s(THUMB));
        let tx = r.left + m;
        let ty = r.top + m;
        match &mut self.thumb {
            Some(bmp) => {
                let (w, h) = fit(bmp.size(), (tsz, tsz));
                let x = tx + (tsz - w) / 2;
                let y = ty + (tsz - h) / 2;
                bmp.draw(hdc, &RECT { left: x, top: y, right: x + w, bottom: y + h }, pal.bg);
            }
            None => {
                let isz = if tsz >= 256 { 256 } else if tsz >= 96 { 96 } else { 48 };
                draw_icon(hdc, self.target.icon, tx + (tsz - isz) / 2, ty + (tsz - isz) / 2, isz);
            }
        }

        let x = tx + tsz + m;
        let right = r.right - m;
        let mut y = ty;
        unsafe {
            let old = SelectObject(hdc, cx.heading);
            let hh = crate::win::font_height(cx.heading) + s(8);
            draw_text(hdc, &self.target.name, &RECT { left: x, top: y, right, bottom: y + hh }, pal.text, DT_LEFT_V);
            y += hh + s(4);
            SelectObject(hdc, cx.fonts.ui);
            let row = s(ROW).max(cx.fonts.ui_height + s(6));
            for (label, value) in &self.rows {
                draw_text(hdc, label, &RECT { left: x, top: y, right: x + s(LABEL_W), bottom: y + row }, pal.text_dim, DT_LEFT_V);
                let vr = RECT { left: x + s(LABEL_W), top: y, right, bottom: y + row };
                if label == "Location" {
                    draw_text(hdc, value, &vr, pal.text, DRAW_TEXT_FORMAT((DT_LEFT_V.0 & !DT_END_ELLIPSIS.0) | DT_PATH_ELLIPSIS.0));
                } else {
                    draw_text(hdc, value, &vr, pal.text, DT_LEFT_V);
                }
                y += row;
            }
            if let Some(note) = &self.note {
                y = y.max(ty + tsz) + s(12);
                let mut w: Vec<u16> = note.encode_utf16().collect();
                let mut nr = RECT { left: r.left + m, top: y, right, bottom: r.bottom - s(8) };
                SetTextColor(hdc, pal.text_dim);
                SetBkMode(hdc, TRANSPARENT);
                DrawTextW(hdc, &mut w, &mut nr, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX | DT_END_ELLIPSIS);
            }
            SelectObject(hdc, old);
        }
    }

    fn subtitle(&self) -> String {
        kind_name(&self.target)
    }
}

