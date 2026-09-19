//! Reading a picture off the system clipboard.
//!
//! A screenshot is the one piece of source material that arrives without
//! being a file first, and importing it any other way means saving it
//! somewhere by hand and then finding it again. So the clipboard is treated
//! as a place media can come from, and pasting writes a real file: the bin
//! holds paths, an export references paths, and a picture that existed only
//! in memory could be neither.
//!
//! Windows hands out a DIB — a bitmap header followed by rows of pixels, in
//! one of several arrangements. The parsing of that is a pure function taking
//! bytes, so the fiddly part is tested rather than reasoned about; the unsafe
//! shell around it does nothing but hand over those bytes.

use image::RgbaImage;

/// A picture on the clipboard right now, if there is one.
#[cfg(windows)]
pub fn image_from_clipboard() -> Option<RgbaImage> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;

    // SAFETY: the clipboard is opened and closed on every path below, and the
    // handle it hands back is only read from, while locked, for the length it
    // reports. Nothing here outlives the lock.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        // V5 first: it carries the same layout with a longer header, and
        // asking for the older format when both exist gets a synthesised
        // copy rather than the original.
        let format = if IsClipboardFormatAvailable(CF_DIBV5) != 0 {
            CF_DIBV5
        } else if IsClipboardFormatAvailable(CF_DIB) != 0 {
            CF_DIB
        } else {
            CloseClipboard();
            return None;
        };

        let handle: HANDLE = GetClipboardData(format);
        if handle.is_null() {
            CloseClipboard();
            return None;
        }
        let ptr = GlobalLock(handle as _);
        if ptr.is_null() {
            CloseClipboard();
            return None;
        }
        let len = GlobalSize(handle as _);
        let bytes = std::slice::from_raw_parts(ptr as *const u8, len).to_vec();
        GlobalUnlock(handle as _);
        CloseClipboard();

        dib_to_rgba(&bytes)
    }
}

#[cfg(not(windows))]
pub fn image_from_clipboard() -> Option<RgbaImage> {
    None
}

/// Whether Ctrl+V was pressed since this was last asked.
///
/// Roughcut never reads the keyboard directly anywhere else, and would not
/// here either if there were a choice. egui-winit intercepts Ctrl+V before
/// the application sees it: it asks the clipboard for *text*, emits a paste
/// event only if it finds some, and returns either way — so the key event is
/// dropped. A clipboard holding a screenshot and nothing else therefore
/// produces no event at all, and the shortcut everyone will press is simply
/// not deliverable by the normal route.
///
/// The low bit of `GetAsyncKeyState` means "pressed since the previous
/// call", which is the edge this needs. Called once a pass, and only while
/// the window has focus, so it cannot pick up a Ctrl+V meant for another
/// program.
#[cfg(windows)]
pub fn paste_chord_pressed() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    const VK_CONTROL: i32 = 0x11;
    const VK_V: i32 = 0x56;

    // SAFETY: reads keyboard state and touches nothing else.
    unsafe {
        // Asked every pass whatever the answer, so the "since last call" bit
        // always covers just the last pass rather than an open-ended stretch.
        let v_edge = GetAsyncKeyState(VK_V) & 1 != 0;
        let ctrl_down = (GetAsyncKeyState(VK_CONTROL) as u16 & 0x8000) != 0;
        v_edge && ctrl_down
    }
}

#[cfg(not(windows))]
pub fn paste_chord_pressed() -> bool {
    false
}

/// Turn a packed DIB into pixels.
///
/// The header says how big it is, and its own size says which kind it is —
/// 40 bytes for the plain one, 108 or 124 for the later ones that add colour
/// masks and profiles. All of them are followed by the colour table, if any,
/// and then the rows.
///
/// Two things routinely catch people out and both are handled here: a
/// negative height means the rows are stored top-down rather than the usual
/// bottom-up, and each row is padded to a multiple of four bytes.
#[cfg(any(windows, test))]
pub fn dib_to_rgba(bytes: &[u8]) -> Option<RgbaImage> {
    const MAX_SIDE: u32 = 16_384;

    let header_size = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?);
    if header_size < 40 {
        // A BITMAPCOREHEADER, from Windows 3.0. Nothing has produced one this
        // century, and guessing at it would be worse than declining.
        return None;
    }
    let width = i32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
    let height = i32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?);
    let bit_count = u16::from_le_bytes(bytes.get(14..16)?.try_into().ok()?);
    let compression = u32::from_le_bytes(bytes.get(16..20)?.try_into().ok()?);
    let clr_used = u32::from_le_bytes(bytes.get(32..36)?.try_into().ok()?);

    // Rows run bottom-up unless the height is negative, which is how a DIB
    // says top-down.
    let top_down = height < 0;
    let (w, h) = (width, height.unsigned_abs() as i32);
    if w <= 0 || h <= 0 || w as u32 > MAX_SIDE || h as u32 > MAX_SIDE {
        return None;
    }
    let (w, h) = (w as u32, h as u32);

    // BI_RGB is 0 and BI_BITFIELDS is 3; the latter puts three or four masks
    // after the header, which for 32-bit screenshots are always the ordinary
    // channel positions. Anything compressed is a JPEG or PNG payload
    // wearing a bitmap header, which is not worth unpicking here.
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    if compression != BI_RGB && compression != BI_BITFIELDS {
        return None;
    }
    if bit_count != 24 && bit_count != 32 {
        // Palette images would need the colour table walked. Screenshots and
        // anything from a browser or an image editor are 24 or 32.
        return None;
    }

    // Where the pixels start: past the header, the masks a BI_BITFIELDS
    // BITMAPINFOHEADER carries, and any colour table.
    let masks = if compression == BI_BITFIELDS && header_size == 40 {
        12
    } else {
        0
    };
    let table = clr_used as usize * 4;
    let start = header_size as usize + masks + table;

    // Rows are padded out to a four-byte boundary.
    let stride = ((w as usize * bit_count as usize).div_ceil(32)) * 4;
    let needed = start.checked_add(stride.checked_mul(h as usize)?)?;
    if bytes.len() < needed {
        return None;
    }

    let mut out = RgbaImage::new(w, h);
    let bytes_per_pixel = bit_count as usize / 8;
    for y in 0..h {
        // Bottom-up storage means the first row in the buffer is the last
        // row of the picture.
        let row = if top_down { y } else { h - 1 - y };
        let base = start + row as usize * stride;
        for x in 0..w {
            let p = base + x as usize * bytes_per_pixel;
            // DIBs are BGR, and the fourth byte of a 32-bit one is alpha —
            // except when nothing set it, which is most of the time, and a
            // picture that is entirely transparent is worse than one that is
            // entirely opaque. Whether to trust it is decided below.
            let (b, g, r) = (bytes[p], bytes[p + 1], bytes[p + 2]);
            let a = if bytes_per_pixel == 4 { bytes[p + 3] } else { 255 };
            out.put_pixel(x, y, image::Rgba([r, g, b, a]));
        }
    }

    // A 32-bit DIB whose alpha channel is entirely zero is not a transparent
    // picture; it is a picture whose alpha nobody filled in. Trusting it
    // would paste an invisible frame.
    if bytes_per_pixel == 4 && out.pixels().all(|p| p.0[3] == 0) {
        for p in out.pixels_mut() {
            p.0[3] = 255;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a BITMAPINFOHEADER DIB the way Windows hands one over.
    fn dib(w: i32, h: i32, bpp: u16, pixels: &[[u8; 4]]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&40u32.to_le_bytes()); // header size
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // planes
        v.extend_from_slice(&bpp.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        v.extend_from_slice(&0u32.to_le_bytes()); // image size
        v.extend_from_slice(&[0u8; 16]); // resolution, palette counts
        let bytes_per_pixel = bpp as usize / 8;
        let stride = ((w.unsigned_abs() as usize * bpp as usize).div_ceil(32)) * 4;
        let rows = h.unsigned_abs() as usize;
        for row in 0..rows {
            let mut line = Vec::new();
            for x in 0..w.unsigned_abs() as usize {
                let px = pixels[row * w.unsigned_abs() as usize + x];
                line.extend_from_slice(&px[..bytes_per_pixel]);
            }
            line.resize(stride, 0); // pad to the four-byte boundary
            v.extend_from_slice(&line);
        }
        v
    }

    /// Blue, green, red, alpha — the order a DIB stores them in.
    const RED: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    #[test]
    fn a_bottom_up_bitmap_comes_out_the_right_way_up() {
        // Two rows, stored bottom-up: the buffer holds the BOTTOM row first,
        // so red is written first and must end up underneath.
        let v = dib(1, 2, 32, &[RED, GREEN]);
        let img = dib_to_rgba(&v).expect("a 32-bit DIB is readable");
        assert_eq!(img.dimensions(), (1, 2));
        assert_eq!(img.get_pixel(0, 0).0, [0, 255, 0, 255], "top row should be green");
        assert_eq!(img.get_pixel(0, 1).0, [255, 0, 0, 255], "bottom row should be red");
    }

    /// A negative height means the rows are already in reading order.
    #[test]
    fn a_top_down_bitmap_is_not_flipped_again() {
        let v = dib(1, -2, 32, &[RED, GREEN]);
        let img = dib_to_rgba(&v).expect("readable");
        assert_eq!(img.dimensions(), (1, 2));
        assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255], "top row should be red");
        assert_eq!(img.get_pixel(0, 1).0, [0, 255, 0, 255]);
    }

    /// 24-bit rows are padded to a four-byte boundary, which is the classic
    /// way to read a picture back sheared.
    #[test]
    fn twentyfour_bit_rows_are_padded_and_still_line_up() {
        // 3 pixels x 3 bytes = 9, padded to 12.
        let px = [RED, GREEN, RED, GREEN, RED, GREEN];
        let v = dib(3, 2, 24, &px);
        let img = dib_to_rgba(&v).expect("a 24-bit DIB is readable");
        assert_eq!(img.dimensions(), (3, 2));
        // Bottom-up again: the second row of the buffer is the top row.
        assert_eq!(img.get_pixel(0, 0).0, [0, 255, 0, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(2, 0).0, [0, 255, 0, 255]);
        // 24-bit has no alpha byte; every pixel must still be opaque.
        assert!(img.pixels().all(|p| p.0[3] == 255));
    }

    /// A screenshot whose alpha channel was never filled in must not paste
    /// as an invisible rectangle.
    #[test]
    fn an_unset_alpha_channel_is_treated_as_opaque() {
        let clear: [u8; 4] = [10, 20, 30, 0];
        let v = dib(2, 1, 32, &[clear, clear]);
        let img = dib_to_rgba(&v).expect("readable");
        assert!(
            img.pixels().all(|p| p.0[3] == 255),
            "a picture with no alpha set came out fully transparent"
        );
        // The colour is still what it was, and still un-swapped.
        assert_eq!(img.get_pixel(0, 0).0, [30, 20, 10, 255]);
    }

    /// Real alpha is left alone.
    #[test]
    fn a_genuine_alpha_channel_survives() {
        let half: [u8; 4] = [0, 0, 255, 128];
        let solid: [u8; 4] = [0, 0, 255, 255];
        let v = dib(2, 1, 32, &[half, solid]);
        let img = dib_to_rgba(&v).expect("readable");
        assert_eq!(img.get_pixel(0, 0).0[3], 128);
        assert_eq!(img.get_pixel(1, 0).0[3], 255);
    }

    /// Nonsense is declined rather than guessed at.
    #[test]
    fn unreadable_bitmaps_are_refused() {
        assert!(dib_to_rgba(&[]).is_none(), "empty");
        assert!(dib_to_rgba(&[0; 8]).is_none(), "too short to hold a header");
        // A truncated pixel buffer must not be read past.
        let mut v = dib(4, 4, 32, &[RED; 16]);
        v.truncate(40 + 8);
        assert!(dib_to_rgba(&v).is_none(), "truncated pixels");
        // Palette and compressed forms are declined.
        let mut v = dib(1, 1, 32, &[RED]);
        v[14..16].copy_from_slice(&8u16.to_le_bytes()); // 8 bpp
        assert!(dib_to_rgba(&v).is_none(), "palette");
        let mut v = dib(1, 1, 32, &[RED]);
        v[16..20].copy_from_slice(&4u32.to_le_bytes()); // BI_JPEG
        assert!(dib_to_rgba(&v).is_none(), "compressed payload");
        // A width nobody could have meant.
        let mut v = dib(1, 1, 32, &[RED]);
        v[4..8].copy_from_slice(&999_999i32.to_le_bytes());
        assert!(dib_to_rgba(&v).is_none(), "absurd width");
    }
}
