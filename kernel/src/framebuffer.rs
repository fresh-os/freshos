/// Direct framebuffer access — the foundation of every pixel FreshOS draws.
///
/// After UEFI boot services are gone, this is the only way to talk to the
/// display. The framebuffer is a flat memory region mapped by the GPU; we
/// write BGRA or RGBA pixels directly.
use crate::font;
use crate::font_aa;

// ----------------------------------------------------------------------------
// Colour
// ----------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

// ----------------------------------------------------------------------------
// Framebuffer
// ----------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct Framebuffer {
    base: *mut u8,
    width: usize,
    height: usize,
    stride: usize, // pixels per scan line (may be > width)
    is_bgr: bool,
}

impl Framebuffer {
    pub fn new(base: *mut u8, width: usize, height: usize, stride: usize, is_bgr: bool) -> Self {
        Self {
            base,
            width,
            height,
            stride,
            is_bgr,
        }
    }

    // -- Primitives ----------------------------------------------------------

    #[inline]
    fn packed_pixel(&self, c: Color) -> u32 {
        if self.is_bgr {
            ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32)
        } else {
            ((c.b as u32) << 16) | ((c.g as u32) << 8) | (c.r as u32)
        }
    }

    #[inline]
    pub fn put_pixel(&mut self, x: usize, y: usize, c: Color) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.stride + x) * 4;
        unsafe {
            (self.base.add(offset) as *mut u32).write_unaligned(self.packed_pixel(c));
        }
    }

    pub fn clear(&mut self, c: Color) {
        self.draw_rect(0, 0, self.width, self.height, c);
    }

    pub fn draw_rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: Color) {
        if x >= self.width || y >= self.height || w == 0 || h == 0 {
            return;
        }

        let copy_w = w.min(self.width - x);
        let copy_h = h.min(self.height - y);
        let pixel = self.packed_pixel(c);

        for dy in 0..copy_h {
            let row_ptr = unsafe { self.base.add(((y + dy) * self.stride + x) * 4) as *mut u32 };
            for dx in 0..copy_w {
                unsafe {
                    row_ptr.add(dx).write_unaligned(pixel);
                }
            }
        }
    }

    // -- Text ----------------------------------------------------------------

    /// Draw a single character at `(x, y)` with integer `scale` multiplier.
    pub fn draw_char(&mut self, x: usize, y: usize, ch: char, color: Color, scale: usize) {
        let glyph = font::glyph(ch);
        for (row, &bits) in glyph.iter().enumerate() {
            for col in 0..font::CHAR_WIDTH {
                if bits & (0x80 >> col) != 0 {
                    // Fill a scale×scale block for this pixel
                    for sy in 0..scale {
                        for sx in 0..scale {
                            self.put_pixel(x + col * scale + sx, y + row * scale + sy, color);
                        }
                    }
                }
            }
        }
    }

    /// Draw a string. Characters are spaced `(CHAR_WIDTH + 1) * scale` apart.
    pub fn draw_string(&mut self, x: usize, y: usize, s: &str, color: Color, scale: usize) {
        let advance = (font::CHAR_WIDTH + 1) * scale;
        let mut cx = x;
        for ch in s.chars() {
            self.draw_char(cx, y, ch, color, scale);
            cx += advance;
        }
    }

    // -- Anti-aliased text (font_aa) ----------------------------------------

    /// Draw a single character using the anti-aliased font.
    /// Alpha-blends the glyph against `bg` for smooth edges.
    pub fn draw_aa_char(&mut self, x: usize, y: usize, ch: char, fg: Color, bg: Color) {
        let alpha_data = font_aa::glyph_alpha(ch);
        for row in 0..font_aa::GLYPH_H {
            for col in 0..font_aa::GLYPH_W {
                let a = alpha_data[row * font_aa::GLYPH_W + col] as u16;
                if a == 0 {
                    continue;
                }
                let inv = 255 - a;
                let r = ((fg.r as u16 * a + bg.r as u16 * inv) / 255) as u8;
                let g = ((fg.g as u16 * a + bg.g as u16 * inv) / 255) as u8;
                let b = ((fg.b as u16 * a + bg.b as u16 * inv) / 255) as u8;
                self.put_pixel(x + col, y + row, Color::new(r, g, b));
            }
        }
    }

    /// Draw a string using the anti-aliased font.
    pub fn draw_aa_string(&mut self, x: usize, y: usize, s: &str, fg: Color, bg: Color) {
        let mut cx = x;
        for ch in s.chars() {
            self.draw_aa_char(cx, y, ch, fg, bg);
            cx += font_aa::GLYPH_W;
        }
    }

    /// Draw a string at 2x scale using the anti-aliased font.
    pub fn draw_aa_string_2x(&mut self, x: usize, y: usize, s: &str, fg: Color, bg: Color) {
        let mut cx = x;
        for ch in s.chars() {
            let alpha_data = font_aa::glyph_alpha(ch);
            for row in 0..font_aa::GLYPH_H {
                for col in 0..font_aa::GLYPH_W {
                    let a = alpha_data[row * font_aa::GLYPH_W + col] as u16;
                    if a == 0 {
                        continue;
                    }
                    let inv = 255 - a;
                    let r = ((fg.r as u16 * a + bg.r as u16 * inv) / 255) as u8;
                    let g = ((fg.g as u16 * a + bg.g as u16 * inv) / 255) as u8;
                    let b = ((fg.b as u16 * a + bg.b as u16 * inv) / 255) as u8;
                    let c = Color::new(r, g, b);
                    self.put_pixel(cx + col * 2, y + row * 2, c);
                    self.put_pixel(cx + col * 2 + 1, y + row * 2, c);
                    self.put_pixel(cx + col * 2, y + row * 2 + 1, c);
                    self.put_pixel(cx + col * 2 + 1, y + row * 2 + 1, c);
                }
            }
            cx += font_aa::GLYPH_W * 2;
        }
    }

    // -- Compositing ---------------------------------------------------------

    /// Blit another surface onto this framebuffer at (dest_x, dest_y).
    /// Copies row-by-row using memcpy for speed.
    pub fn blit(&mut self, src: &Framebuffer, dest_x: usize, dest_y: usize) {
        self.copy_rect_from(src, 0, 0, src.width, src.height, dest_x, dest_y);
    }

    /// Copy a rectangular region from `src` into this framebuffer.
    pub fn copy_rect_from(
        &mut self,
        src: &Framebuffer,
        src_x: usize,
        src_y: usize,
        w: usize,
        h: usize,
        dest_x: usize,
        dest_y: usize,
    ) {
        if src_x >= src.width
            || src_y >= src.height
            || dest_x >= self.width
            || dest_y >= self.height
            || w == 0
            || h == 0
        {
            return;
        }

        let copy_w = w.min(src.width - src_x).min(self.width - dest_x);
        let copy_h = h.min(src.height - src_y).min(self.height - dest_y);
        let bytes_per_row = copy_w * 4;

        for y in 0..copy_h {
            let src_off = ((src_y + y) * src.stride + src_x) * 4;
            let dst_off = ((dest_y + y) * self.stride + dest_x) * 4;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src.base.add(src_off),
                    self.base.add(dst_off),
                    bytes_per_row,
                );
            }
        }
    }

    /// Copy a rectangular region from this framebuffer into `dst`.
    pub fn copy_rect_to(
        &self,
        dst: &mut Framebuffer,
        src_x: usize,
        src_y: usize,
        w: usize,
        h: usize,
        dest_x: usize,
        dest_y: usize,
    ) {
        dst.copy_rect_from(self, src_x, src_y, w, h, dest_x, dest_y);
    }

    /// Blit a surface at half scale (every other pixel, every other row).
    pub fn blit_scaled_half(&mut self, src: &Framebuffer, dest_x: usize, dest_y: usize) {
        let half_w = src.width / 2;
        let half_h = src.height / 2;

        for y in 0..half_h {
            if dest_y + y >= self.height {
                break;
            }
            for x in 0..half_w {
                if dest_x + x >= self.width {
                    break;
                }
                // Sample from source at 2x coordinates
                let src_off = (y * 2 * src.stride + x * 2) * 4;
                let dst_off = ((dest_y + y) * self.stride + dest_x + x) * 4;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src.base.add(src_off),
                        self.base.add(dst_off),
                        4, // one pixel (4 bytes)
                    );
                }
            }
        }
    }

    /// Read the colour of a pixel at `(x, y)`.
    #[inline]
    pub fn read_pixel(&self, x: usize, y: usize) -> Color {
        if x >= self.width || y >= self.height {
            return Color::new(0, 0, 0);
        }
        let offset = (y * self.stride + x) * 4;
        unsafe {
            let p = self.base.add(offset);
            if self.is_bgr {
                Color::new(
                    p.add(2).read_volatile(),
                    p.add(1).read_volatile(),
                    p.read_volatile(),
                )
            } else {
                Color::new(
                    p.read_volatile(),
                    p.add(1).read_volatile(),
                    p.add(2).read_volatile(),
                )
            }
        }
    }

    /// Width in pixels.
    #[inline]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    #[inline]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Stride in pixels (may be wider than width).
    #[inline]
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Return the base pointer (for creating sub-framebuffers from surfaces).
    pub fn base_ptr(&self) -> *mut u8 {
        self.base
    }

    #[inline]
    pub fn is_bgr(&self) -> bool {
        self.is_bgr
    }

    /// Copy the entire contents of this framebuffer to `dst`.
    pub fn copy_to(&self, dst: &mut Framebuffer) {
        self.copy_rect_to(dst, 0, 0, self.width, self.height, 0, 0);
    }
}
