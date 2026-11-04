//! Image decoding (WIC) and the image viewer.

use super::content::Pixels;
use super::{Ctx, Pref, Viewer};
use crate::util::wide;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{GENERIC_READ, HANDLE, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Variant::VT_UI2;
use windows_core::PROPVARIANT;

/// Rotation (clockwise degrees) then horizontal / vertical flip for an EXIF orientation.
pub fn orientation_ops(o: u16) -> (u32, bool, bool) {
    match o {
        2 => (0, true, false),
        3 => (180, false, false),
        4 => (0, false, true),
        5 => (90, true, false),
        6 => (90, false, false),
        7 => (270, true, false),
        8 => (270, false, false),
        _ => (0, false, false),
    }
}

/// Decodes an image file, at most `max_side` pixels on its long side.
/// Returns the pixels and the upright full-size dimensions.
pub fn decode_file(path: &str, max_side: u32) -> Result<(Pixels, u32, u32), String> {
    unsafe {
        let f = factory()?;
        let w = wide(path);
        let dec = f
            .CreateDecoderFromFilename(PCWSTR(w.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .map_err(|e| err(&e))?;
        decode(&f, &dec, max_side)
    }
}

/// Decodes an image held in memory.
pub fn decode_bytes(bytes: &[u8], max_side: u32) -> Result<(Pixels, u32, u32), String> {
    unsafe {
        let f = factory()?;
        let stream = f.CreateStream().map_err(|e| err(&e))?;
        stream.InitializeFromMemory(bytes).map_err(|e| err(&e))?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).map_err(|e| err(&e))?;
        decode(&f, &dec, max_side)
    }
}

fn err(e: &windows::core::Error) -> String {
    match e.code().0 as u32 {
        0x88982F50 => "no decoder is installed for this format".into(),
        _ => {
            let m = e.message();
            if m.is_empty() {
                format!("error 0x{:08X}", e.code().0 as u32)
            } else {
                m.trim_end_matches(['.', '\r', '\n']).to_string()
            }
        }
    }
}

unsafe fn factory() -> Result<IWICImagingFactory, String> {
    CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(|e| err(&e))
}

unsafe fn orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    let Ok(reader) = frame.GetMetadataQueryReader() else { return 1 };
    for q in ["/app1/ifd/{ushort=274}", "/ifd/{ushort=274}"] {
        let w = wide(q);
        let mut pv = PROPVARIANT::default();
        if reader.GetMetadataByName(PCWSTR(w.as_ptr()), &mut pv).is_ok() && pv.as_raw().Anonymous.Anonymous.vt == VT_UI2.0 {
            if let Ok(v) = u16::try_from(&pv) {
                return v;
            }
        }
    }
    1
}

unsafe fn decode(f: &IWICImagingFactory, dec: &IWICBitmapDecoder, max_side: u32) -> Result<(Pixels, u32, u32), String> {
    let frame = dec.GetFrame(0).map_err(|e| err(&e))?;
    let (mut w, mut h) = (0u32, 0u32);
    frame.GetSize(&mut w, &mut h).map_err(|e| err(&e))?;
    if w == 0 || h == 0 {
        return Err("the image is empty".into());
    }
    let (rot, flip_h, flip_v) = orientation_ops(orientation(&frame));
    let mut src: IWICBitmapSource = frame.cast().map_err(|e| err(&e))?;

    let long = w.max(h);
    if long > max_side {
        let k = max_side as f64 / long as f64;
        let (sw, sh) = (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1));
        let s = f.CreateBitmapScaler().map_err(|e| err(&e))?;
        s.Initialize(&src, sw, sh, WICBitmapInterpolationModeFant).map_err(|e| err(&e))?;
        src = s.cast().map_err(|e| err(&e))?;
    }
    let rotate = match rot {
        90 => Some(WICBitmapTransformRotate90),
        180 => Some(WICBitmapTransformRotate180),
        270 => Some(WICBitmapTransformRotate270),
        _ => None,
    };
    let flip = match (flip_h, flip_v) {
        (true, _) => Some(WICBitmapTransformFlipHorizontal),
        (_, true) => Some(WICBitmapTransformFlipVertical),
        _ => None,
    };
    // Rotate first, then flip, as separate steps so the order is unambiguous.
    for op in [rotate, flip].into_iter().flatten() {
        let r = f.CreateBitmapFlipRotator().map_err(|e| err(&e))?;
        r.Initialize(&src, op).map_err(|e| err(&e))?;
        src = r.cast().map_err(|e| err(&e))?;
    }
    let conv = f.CreateFormatConverter().map_err(|e| err(&e))?;
    conv.Initialize(&src, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut)
        .map_err(|e| err(&e))?;
    let (mut pw, mut ph) = (0u32, 0u32);
    conv.GetSize(&mut pw, &mut ph).map_err(|e| err(&e))?;
    let stride = pw * 4;
    let mut data = vec![0u8; (stride * ph) as usize];
    conv.CopyPixels(std::ptr::null(), stride, &mut data).map_err(|e| err(&e))?;
    let (ow, oh) = if rot == 90 || rot == 270 { (h, w) } else { (w, h) };
    Ok((Pixels { w: pw, h: ph, data }, ow, oh))
}

/// Largest size of `img` that fits in `max`, never enlarged, at least 1×1.
pub fn fit(img: (i32, i32), max: (i32, i32)) -> (i32, i32) {
    let (w, h) = (img.0.max(1), img.1.max(1));
    if w <= max.0 && h <= max.1 {
        return (w, h);
    }
    let k = (max.0 as f64 / w as f64).min(max.1 as f64 / h as f64);
    (((w as f64 * k).round() as i32).max(1), ((h as f64 * k).round() as i32).max(1))
}

/// Pixels as a GDI bitmap, alpha composited over a background color.
pub struct Bitmap {
    px: Pixels,
    hbmp: HBITMAP,
    bg: Option<u32>,
}

impl Bitmap {
    pub fn new(px: Pixels) -> Bitmap {
        Bitmap { px, hbmp: HBITMAP::default(), bg: None }
    }

    pub fn size(&self) -> (i32, i32) {
        (self.px.w as i32, self.px.h as i32)
    }

    fn ensure(&mut self, bg: windows::Win32::Foundation::COLORREF) {
        if self.bg == Some(bg.0) && !self.hbmp.is_invalid() {
            return;
        }
        unsafe {
            if !self.hbmp.is_invalid() {
                let _ = DeleteObject(self.hbmp);
                self.hbmp = HBITMAP::default();
            }
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: self.px.w as i32,
                    biHeight: -(self.px.h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let Ok(h) = CreateDIBSection(HDC::default(), &bi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) else { return };
            if bits.is_null() {
                let _ = DeleteObject(h);
                return;
            }
            let out = std::slice::from_raw_parts_mut(bits as *mut u8, self.px.data.len());
            // COLORREF is 0x00BBGGRR; pixels are B, G, R, A.
            let back = [(bg.0 >> 16) as u8 as u32, (bg.0 >> 8) as u8 as u32, bg.0 as u8 as u32];
            for (o, p) in out.chunks_exact_mut(4).zip(self.px.data.chunks_exact(4)) {
                let inv = 255 - p[3] as u32;
                for c in 0..3 {
                    o[c] = (p[c] as u32 + (back[c] * inv + 127) / 255).min(255) as u8;
                }
                o[3] = 255;
            }
            self.hbmp = h;
            self.bg = Some(bg.0);
        }
    }

    /// Draws the bitmap scaled into `dst`.
    pub fn draw(&mut self, hdc: HDC, dst: &RECT, bg: windows::Win32::Foundation::COLORREF) {
        self.ensure(bg);
        if self.hbmp.is_invalid() {
            return;
        }
        unsafe {
            let mem = CreateCompatibleDC(hdc);
            let old = SelectObject(mem, self.hbmp);
            let (dw, dh) = (dst.right - dst.left, dst.bottom - dst.top);
            let (sw, sh) = self.size();
            if dw == sw && dh == sh {
                let _ = BitBlt(hdc, dst.left, dst.top, dw, dh, mem, 0, 0, SRCCOPY);
            } else {
                let prev = SetStretchBltMode(hdc, HALFTONE);
                let _ = SetBrushOrgEx(hdc, 0, 0, None);
                let _ = StretchBlt(hdc, dst.left, dst.top, dw, dh, mem, 0, 0, sw, sh, SRCCOPY);
                SetStretchBltMode(hdc, STRETCH_BLT_MODE(prev));
            }
            SelectObject(mem, old);
            let _ = DeleteDC(mem);
        }
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        if !self.hbmp.is_invalid() {
            unsafe {
                let _ = DeleteObject(self.hbmp);
            }
        }
    }
}

/// Shows an image fitted inside the window, centered, never enlarged.
pub struct ImageView {
    bmp: Bitmap,
    orig: (u32, u32),
}

impl ImageView {
    pub fn new(px: Pixels, orig_w: u32, orig_h: u32) -> ImageView {
        ImageView { bmp: Bitmap::new(px), orig: (orig_w, orig_h) }
    }
}

impl Viewer for ImageView {
    fn preferred(&self) -> Pref {
        let (w, h) = self.bmp.size();
        Pref::Fit(w, h)
    }

    fn paint(&mut self, hdc: HDC, r: &RECT, cx: &Ctx) {
        crate::win::fill(hdc, r, cx.pal.bg);
        let (w, h) = fit(self.bmp.size(), (r.right - r.left, r.bottom - r.top));
        let x = r.left + (r.right - r.left - w) / 2;
        let y = r.top + (r.bottom - r.top - h) / 2;
        self.bmp.draw(hdc, &RECT { left: x, top: y, right: x + w, bottom: y + h }, cx.pal.bg);
    }

    fn subtitle(&self) -> String {
        format!("{} × {}", self.orig.0, self.orig.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_without_enlarging() {
        assert_eq!(fit((100, 50), (800, 600)), (100, 50));
        assert_eq!(fit((1600, 800), (800, 600)), (800, 400));
        assert_eq!(fit((500, 2000), (800, 600)), (150, 600));
        assert_eq!(fit((10000, 1), (100, 100)), (100, 1));
    }

    #[test]
    fn maps_exif_orientation() {
        assert_eq!(orientation_ops(1), (0, false, false));
        assert_eq!(orientation_ops(6), (90, false, false));
        assert_eq!(orientation_ops(5), (90, true, false));
        assert_eq!(orientation_ops(8), (270, false, false));
        assert_eq!(orientation_ops(4), (0, false, true));
        assert_eq!(orientation_ops(99), (0, false, false));
    }
}
