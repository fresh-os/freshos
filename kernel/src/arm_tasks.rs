/// aarch64 desktop tasks — full compositor, shell, dashboard, keyboard.
///
/// Ported from the x86 desktop with these adaptations:
///   - EL1 direct calls instead of SVC (HVF limitation)
///   - UART keyboard instead of PS/2 (QEMU virt has no PS/2)
///   - No mouse (needs USB HID or virtio-input driver)
///   - No PC speaker (no equivalent on QEMU virt)
///   - Double-buffered compositor (back buffer in cached RAM)
use crate::font_aa;
use crate::framebuffer::{Color, Framebuffer};
use crate::ipc;

// ============================================================================
// Colours
// ============================================================================

const TEXT: Color = Color::new(0xE6, 0xED, 0xF3);
const SUBTLE: Color = Color::new(0x8B, 0x94, 0x9E);
const GREEN: Color = Color::new(0x3F, 0xB9, 0x50);
const DIM: Color = Color::new(0x30, 0x36, 0x3D);
const SEP: Color = Color::new(0x21, 0x26, 0x2D);
const ORANGE: Color = Color::new(0xFF, 0xA6, 0x58);
const ACCENT: Color = Color::new(0x60, 0x9B, 0xFF);
const ACCENT_DIM: Color = Color::new(0x30, 0x50, 0x80);

// ============================================================================
// Layout constants
// ============================================================================

const SURF_W: usize = 620;
const SURF_H: usize = 400;
const MENU_H: usize = 26;
const TBAR_H: usize = 36;
const WIN_TITLE_H: usize = 28;
const WIN_BORDER: usize = 2;
const STATS_PANEL_W: usize = 200;
const STATS_PANEL_H: usize = 184;
const UI_REFRESH_NS: u64 = 250_000_000;
const MENU_BG: Color = Color::new(0x10, 0x14, 0x1C);
const TBAR_BG: Color = Color::new(0x10, 0x14, 0x1C);
const DESKTOP_BG: Color = Color::new(0x0C, 0x10, 0x20);
const SHELL_BG: Color = Color::new(0x0A, 0x0E, 0x14);
const DASH_BG: Color = Color::new(0x0A, 0x0E, 0x14);
const PANEL_BG: Color = Color::new(0x08, 0x0C, 0x14);

// IPC channels (must match main.rs setup)
const CH_KBD_EVENTS: u32 = 0;
const CH_SHELL_KEYS: u32 = 1;
const CH_IPC_PROBE_PING: u32 = 2;
const CH_IPC_PROBE_PONG: u32 = 3;

// ============================================================================
// EL1 helpers — direct kernel calls
// ============================================================================

fn time_ns() -> u64 {
    crate::arch::time_ns()
}
fn yield_now() {
    crate::arch::interrupt_enable();
    crate::arch::halt();
}

fn task_count() -> u64 {
    crate::arch::context::task_count() as u64
}

fn channel_count() -> u64 {
    crate::ipc::channel_count() as u64
}

fn fbinfo() -> (Framebuffer, crate::arch::syscall::FbInfo) {
    let fbi = unsafe { *crate::arch::syscall::FB_INFO_PTR.0.get() };
    let fb = Framebuffer::new(
        fbi.address as *mut u8,
        fbi.width as usize,
        fbi.height as usize,
        fbi.stride as usize,
        fbi.is_bgr != 0,
    );
    (fb, fbi)
}

fn surface(idx: u32) -> Framebuffer {
    let si = unsafe { (*crate::arch::syscall::SURFACES_PTR.0.get())[idx as usize] };
    Framebuffer::new(
        si.address as *mut u8,
        si.width as usize,
        si.height as usize,
        si.stride as usize,
        true,
    )
}

fn trace_read(buf: &mut [ipc::TraceEntry]) -> usize {
    ipc::trace_read(buf, buf.len())
}

// ============================================================================
// Formatting helpers
// ============================================================================

fn fmt_hms(ns: u64, buf: &mut [u8; 8]) {
    let s = ns / 1_000_000_000;
    let m = s / 60;
    let h = m / 60;
    buf[0] = b'0' + (h / 10 % 10) as u8;
    buf[1] = b'0' + (h % 10) as u8;
    buf[2] = b':';
    buf[3] = b'0' + (m % 60 / 10) as u8;
    buf[4] = b'0' + (m % 60 % 10) as u8;
    buf[5] = b':';
    buf[6] = b'0' + (s % 60 / 10) as u8;
    buf[7] = b'0' + (s % 60 % 10) as u8;
}

fn fmt_u64(n: u64, buf: &mut [u8]) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut v = n;
    let mut d = [0u8; 20];
    let mut di = 0;
    while v > 0 {
        d[di] = b'0' + (v % 10) as u8;
        v /= 10;
        di += 1;
    }
    let len = di.min(buf.len());
    for i in 0..len {
        buf[i] = d[di - 1 - i];
    }
    len
}

fn fmt_latency(us: u64, buf: &mut [u8; 10]) -> usize {
    let mut i = 0;
    let ms = us / 1000;
    let frac = (us % 1000) / 100;
    if ms > 0 || us >= 1000 {
        let mut n = ms;
        if n == 0 {
            buf[i] = b'0';
            i += 1;
        } else {
            let mut d = [0u8; 6];
            let mut di = 0;
            while n > 0 {
                d[di] = b'0' + (n % 10) as u8;
                n /= 10;
                di += 1;
            }
            while di > 0 {
                di -= 1;
                buf[i] = d[di];
                i += 1;
            }
        }
        buf[i] = b'.';
        i += 1;
        buf[i] = b'0' + (frac % 10) as u8;
        i += 1;
        buf[i] = b'm';
        i += 1;
        buf[i] = b's';
        i += 1;
    } else {
        let mut n = us;
        if n == 0 {
            buf[i] = b'0';
            i += 1;
        } else {
            let mut d = [0u8; 6];
            let mut di = 0;
            while n > 0 {
                d[di] = b'0' + (n % 10) as u8;
                n /= 10;
                di += 1;
            }
            while di > 0 {
                di -= 1;
                buf[i] = d[di];
                i += 1;
            }
        }
        buf[i] = b'u';
        i += 1;
        buf[i] = b's';
        i += 1;
    }
    i
}

fn latency_str<'a>(us: u64, buf: &'a mut [u8; 10]) -> &'a str {
    let len = fmt_latency(us, buf);
    core::str::from_utf8(&buf[..len]).unwrap_or("?")
}

#[inline]
fn hud_metric_bucket(us: u64) -> u64 {
    if us >= 10_000 {
        us / 1_000
    } else if us >= 1_000 {
        us / 100
    } else {
        us / 50
    }
}

#[inline]
fn blend(fg: Color, bg: Color, alpha: u8) -> Color {
    let a = alpha as u16;
    let ia = 255 - a;
    Color::new(
        ((fg.r as u16 * a + bg.r as u16 * ia) / 255) as u8,
        ((fg.g as u16 * a + bg.g as u16 * ia) / 255) as u8,
        ((fg.b as u16 * a + bg.b as u16 * ia) / 255) as u8,
    )
}

#[derive(Clone, Copy)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Rect {
    #[inline]
    fn right(self) -> usize {
        self.x + self.w
    }

    #[inline]
    fn bottom(self) -> usize {
        self.y + self.h
    }
}

fn rect_union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect {
        x,
        y,
        w: right.saturating_sub(x),
        h: bottom.saturating_sub(y),
    }
}

fn rect_intersection(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    if right <= x || bottom <= y {
        None
    } else {
        Some(Rect {
            x,
            y,
            w: right - x,
            h: bottom - y,
        })
    }
}

fn extend_damage(slot: &mut Option<Rect>, rect: Rect) {
    *slot = Some(match *slot {
        Some(existing) => rect_union(existing, rect),
        None => rect,
    });
}

fn outer_rect_for_window(wx: usize, wy: usize) -> Rect {
    Rect {
        x: wx.saturating_sub(WIN_BORDER),
        y: wy.saturating_sub(WIN_TITLE_H + WIN_BORDER),
        w: SURF_W + WIN_BORDER * 2,
        h: SURF_H + WIN_TITLE_H + WIN_BORDER * 2,
    }
}

fn allocate_framebuffer(width: usize, height: usize, is_bgr: bool) -> Framebuffer {
    let bytes = width * height * 4;
    let pages = (bytes + 4095) / 4096;
    let addr = crate::frame_alloc::allocate_contiguous(pages).expect("framebuffer");
    unsafe { core::ptr::write_bytes(addr as *mut u8, 0, bytes) };
    Framebuffer::new(addr as *mut u8, width, height, width, is_bgr)
}

fn build_window_frame_cache(
    title: &str,
    accent: Color,
    is_active: bool,
    is_bgr: bool,
) -> Framebuffer {
    let outer = outer_rect_for_window(WIN_BORDER, WIN_TITLE_H + WIN_BORDER);
    let mut fb = allocate_framebuffer(outer.w, outer.h, is_bgr);
    draw_window_frame(
        &mut fb,
        WIN_BORDER,
        WIN_TITLE_H + WIN_BORDER,
        SURF_W,
        SURF_H,
        title,
        accent,
        is_active,
    );
    fb
}

fn blit_full_window(
    dst: &mut Framebuffer,
    frame_cache: &Framebuffer,
    surface: &Framebuffer,
    wx: usize,
    wy: usize,
) {
    let outer = outer_rect_for_window(wx, wy);
    dst.blit(frame_cache, outer.x, outer.y);
    dst.blit(surface, wx, wy);
}

fn metrics_damage_rect_to_rect(damage: crate::metrics::DamageRect) -> Option<Rect> {
    if damage.w == 0 || damage.h == 0 {
        None
    } else {
        Some(Rect {
            x: damage.x as usize,
            y: damage.y as usize,
            w: damage.w as usize,
            h: damage.h as usize,
        })
    }
}

// ============================================================================
// Drawing helpers — menu bar, taskbar, windows, stats, icons
// ============================================================================

fn build_menu_bar_cache(sw: usize, is_bgr: bool) -> Framebuffer {
    let mut fb = allocate_framebuffer(sw, MENU_H + 1, is_bgr);
    fb.draw_rect(0, 0, sw, MENU_H, MENU_BG);
    fb.draw_rect(0, MENU_H, sw, 1, ACCENT_DIM);
    fb.draw_aa_string(12, 5, "FreshOS", ACCENT, MENU_BG);
    fb.draw_aa_string(90, 5, "|", DIM, MENU_BG);
    fb.draw_aa_string(100, 5, "aarch64", DIM, MENU_BG);
    fb
}

fn draw_menu_bar(
    fb: &mut Framebuffer,
    cache: &Framebuffer,
    sw: usize,
    ns: u64,
    latency_us: u64,
    task_count: u64,
) {
    fb.blit(cache, 0, 0);

    // Clock (right)
    let mut hms = [0u8; 8];
    fmt_hms(ns, &mut hms);
    let time_str = core::str::from_utf8(&hms).unwrap_or("??:??:??");
    let clock_x = sw - 8 * font_aa::GLYPH_W - 12;
    fb.draw_aa_string(clock_x, 5, time_str, TEXT, MENU_BG);

    let mut sx = clock_x - 12;

    // Latency
    if latency_us > 0 {
        let mut lbuf = [0u8; 10];
        let llen = fmt_latency(latency_us, &mut lbuf);
        let lstr = core::str::from_utf8(&lbuf[..llen]).unwrap_or("?");
        let lw = llen * font_aa::GLYPH_W;
        sx -= lw + 8;
        let c = if latency_us < 2000 {
            GREEN
        } else if latency_us < 5000 {
            ORANGE
        } else {
            Color::new(0xFF, 0x44, 0x44)
        };
        fb.draw_aa_string(sx, 5, lstr, c, MENU_BG);
        sx -= 4;
    }

    // Task count
    {
        let mut tbuf = [0u8; 16];
        let tlen = fmt_u64(task_count, &mut tbuf);
        let mut full = [b' '; 10];
        for i in 0..tlen.min(4) {
            full[i] = tbuf[i];
        }
        let j = tlen.min(4);
        full[j] = b' ';
        full[j + 1] = b't';
        full[j + 2] = b'a';
        full[j + 3] = b's';
        full[j + 4] = b'k';
        full[j + 5] = b's';
        let slen = j + 6;
        let s = core::str::from_utf8(&full[..slen]).unwrap_or("?");
        sx -= slen * font_aa::GLYPH_W + 8;
        fb.draw_aa_string(sx, 5, s, SUBTLE, MENU_BG);
    }
}

fn draw_taskbar(fb: &mut Framebuffer, sw: usize, sh: usize, active_ws: usize) {
    let tbar_y = sh - TBAR_H;
    fb.draw_rect(0, tbar_y, sw, TBAR_H, TBAR_BG);
    fb.draw_rect(0, tbar_y, sw, 1, ACCENT_DIM);

    let pw = 100usize;
    let ph = 24usize;
    let gap = 12usize;
    let total = pw * 2 + gap;
    let px = (sw - total) / 2;
    let py = tbar_y + (TBAR_H - ph) / 2;

    // Shell pill
    {
        let active = active_ws == 0;
        let border = if active { GREEN } else { DIM };
        let fill = if active {
            Color::new(0x1A, 0x2E, 0x1A)
        } else {
            Color::new(0x18, 0x1C, 0x22)
        };
        let text = if active { GREEN } else { SUBTLE };
        fb.draw_rect(px, py, pw, ph, border);
        fb.draw_rect(px + 1, py + 1, pw - 2, ph - 2, fill);
        if active {
            fb.draw_rect(px + pw / 2 - 2, py + ph - 4, 4, 2, GREEN);
        }
        fb.draw_aa_string(px + pw / 2 - 25, py + 4, "Shell", text, fill);
    }

    // Dashboard pill
    {
        let active = active_ws == 1;
        let border = if active { ORANGE } else { DIM };
        let fill = if active {
            Color::new(0x2E, 0x22, 0x1A)
        } else {
            Color::new(0x18, 0x1C, 0x22)
        };
        let text = if active { ORANGE } else { SUBTLE };
        let px1 = px + pw + gap;
        fb.draw_rect(px1, py, pw, ph, border);
        fb.draw_rect(px1 + 1, py + 1, pw - 2, ph - 2, fill);
        if active {
            fb.draw_rect(px1 + pw / 2 - 2, py + ph - 4, 4, 2, ORANGE);
        }
        fb.draw_aa_string(px1 + pw / 2 - 43, py + 4, "Dashboard", text, fill);
    }
}

fn draw_window_frame(
    fb: &mut Framebuffer,
    wx: usize,
    wy: usize,
    ww: usize,
    wh: usize,
    title: &str,
    accent: Color,
    is_active: bool,
) {
    let border_c = if is_active { accent } else { DIM };
    let title_y = wy.saturating_sub(WIN_TITLE_H);
    let bw = WIN_BORDER;

    // Borders
    fb.draw_rect(
        wx.saturating_sub(bw),
        title_y.saturating_sub(bw),
        ww + bw * 2,
        bw,
        border_c,
    );
    fb.draw_rect(wx.saturating_sub(bw), wy + wh, ww + bw * 2, bw, border_c);
    fb.draw_rect(
        wx.saturating_sub(bw),
        title_y,
        bw,
        wh + WIN_TITLE_H,
        border_c,
    );
    fb.draw_rect(wx + ww, title_y, bw, wh + WIN_TITLE_H, border_c);

    // Title bar
    let bar_c = if is_active {
        Color::new(0x18, 0x1C, 0x24)
    } else {
        Color::new(0x10, 0x13, 0x18)
    };
    fb.draw_rect(wx, title_y, ww, WIN_TITLE_H, bar_c);

    let text_c = if is_active { TEXT } else { SUBTLE };
    fb.draw_aa_string(wx + 10, title_y + 6, title, text_c, bar_c);

    if is_active {
        let dot_x = wx + 10 + title.len() * font_aa::GLYPH_W + 8;
        fb.draw_rect(dot_x, title_y + 10, 4, 4, accent);
    }

    fb.draw_rect(
        wx,
        title_y + WIN_TITLE_H - 1,
        ww,
        1,
        if is_active { accent } else { DIM },
    );
}

fn draw_metric_pair_line(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    label: &str,
    sample: crate::metrics::MetricSample,
    bg: Color,
) {
    let mut latest = [0u8; 10];
    let mut max = [0u8; 10];
    fb.draw_aa_string(x, y, label, SUBTLE, bg);
    fb.draw_aa_string(x + 96, y, latency_str(sample.latest, &mut latest), TEXT, bg);
    fb.draw_aa_string(x + 148, y, "/", DIM, bg);
    fb.draw_aa_string(x + 158, y, latency_str(sample.max, &mut max), DIM, bg);
}

fn draw_phase_triplet_line(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    bg_us: u64,
    win_us: u64,
    ui_us: u64,
    bg: Color,
) {
    let mut bg_buf = [0u8; 10];
    let mut win_buf = [0u8; 10];
    let mut ui_buf = [0u8; 10];

    fb.draw_aa_string(x, y, "BG", SUBTLE, bg);
    fb.draw_aa_string(x + 20, y, latency_str(bg_us, &mut bg_buf), TEXT, bg);
    fb.draw_aa_string(x + 84, y, "WIN", SUBTLE, bg);
    fb.draw_aa_string(x + 114, y, latency_str(win_us, &mut win_buf), TEXT, bg);
    fb.draw_aa_string(x + 188, y, "UI", SUBTLE, bg);
    fb.draw_aa_string(x + 208, y, latency_str(ui_us, &mut ui_buf), TEXT, bg);
}

fn build_stats_overlay_cache(is_bgr: bool) -> Framebuffer {
    let mut fb = allocate_framebuffer(STATS_PANEL_W, STATS_PANEL_H, is_bgr);
    fb.draw_rect(0, 0, STATS_PANEL_W, STATS_PANEL_H, PANEL_BG);
    fb.draw_rect(0, 0, STATS_PANEL_W, 1, ACCENT_DIM);
    fb.draw_rect(0, STATS_PANEL_H - 1, STATS_PANEL_W, 1, DIM);

    let lx = 10;
    let mut y = 8;

    fb.draw_aa_string(lx, y, "System", ACCENT, PANEL_BG);
    y += 18;
    fb.draw_aa_string(lx, y, "Uptime", SUBTLE, PANEL_BG);
    y += 16;
    fb.draw_aa_string(lx, y, "Tasks", SUBTLE, PANEL_BG);
    y += 16;
    fb.draw_aa_string(lx, y, "Chans", SUBTLE, PANEL_BG);
    y += 16;
    fb.draw_aa_string(lx, y, "Frame", SUBTLE, PANEL_BG);
    y += 16;
    fb.draw_aa_string(lx, y, "Present", SUBTLE, PANEL_BG);
    y += 16;
    fb.draw_aa_string(lx, y, "IPC RTT", SUBTLE, PANEL_BG);
    y += 16;
    fb.draw_aa_string(lx, y, "Wake", SUBTLE, PANEL_BG);
    y += 20;
    fb.draw_aa_string(lx, y, "Photon", SUBTLE, PANEL_BG);

    fb
}

fn draw_metric_value(fb: &mut Framebuffer, x: usize, y: usize, value_us: u64, bg: Color) {
    let mut buf = [0u8; 10];
    fb.draw_aa_string(x, y, latency_str(value_us, &mut buf), TEXT, bg);
}

fn draw_stats_histogram(
    fb: &mut Framebuffer,
    x: usize,
    y: usize,
    latency_hist: &[u64; 16],
    hist_idx: usize,
) {
    let bar_area_w = STATS_PANEL_W - 20;
    let bar_h: usize = 30;
    let num_bars: usize = 16;
    let bar_w = bar_area_w / num_bars;

    let mut max_val: u64 = 1;
    for i in 0..num_bars {
        if latency_hist[i] > max_val {
            max_val = latency_hist[i];
        }
    }

    for i in 0..num_bars {
        let idx = (hist_idx + i) % num_bars;
        let val = latency_hist[idx];
        let h = if val > 0 {
            ((val as usize * bar_h) / max_val as usize).max(1)
        } else {
            0
        };
        let bx = x + i * bar_w;
        let by = y + bar_h - h;
        let c = if val < 2000 {
            GREEN
        } else if val < 5000 {
            ORANGE
        } else {
            Color::new(0xFF, 0x44, 0x44)
        };
        if h > 0 {
            for dy in 0..h {
                for dx in 0..bar_w.saturating_sub(1) {
                    fb.put_pixel(bx + dx, by + dy, c);
                }
            }
        }
    }
}

fn draw_stats_overlay(
    fb: &mut Framebuffer,
    cache: &Framebuffer,
    sw: usize,
    ns: u64,
    metrics: crate::metrics::Snapshot,
    latency_hist: &[u64; 16],
    hist_idx: usize,
    tasks_now: u64,
    channels_now: u64,
) {
    let panel_x = sw - STATS_PANEL_W - 16;
    let panel_y = MENU_H + 12;
    fb.blit(cache, panel_x, panel_y);

    let lx = panel_x + 10;
    let mut y = panel_y + 8;
    y += 18;

    let mut hms = [0u8; 8];
    fmt_hms(ns, &mut hms);
    let ts = core::str::from_utf8(&hms).unwrap_or("??:??:??");
    fb.draw_aa_string(lx + 80, y, ts, TEXT, PANEL_BG);
    y += 16;

    {
        let mut buf = [0u8; 16];
        let len = fmt_u64(tasks_now, &mut buf);
        let s = core::str::from_utf8(&buf[..len]).unwrap_or("?");
        fb.draw_aa_string(lx + 80, y, s, TEXT, PANEL_BG);
        fb.draw_aa_string(
            lx + 80 + len * font_aa::GLYPH_W + 6,
            y,
            "@ 1 kHz",
            TEXT,
            PANEL_BG,
        );
    }
    y += 16;

    {
        let mut buf = [0u8; 16];
        let len = fmt_u64(channels_now, &mut buf);
        let s = core::str::from_utf8(&buf[..len]).unwrap_or("?");
        fb.draw_aa_string(lx + 80, y, s, TEXT, PANEL_BG);
    }
    y += 16;

    draw_metric_value(fb, lx + 80, y, metrics.frame_us.latest, PANEL_BG);
    y += 16;

    draw_metric_value(fb, lx + 80, y, metrics.present_us.latest, PANEL_BG);
    y += 16;

    draw_metric_value(fb, lx + 80, y, metrics.ipc_rtt_us.latest, PANEL_BG);
    y += 16;

    draw_metric_value(fb, lx + 80, y, metrics.sched_wake_us.latest, PANEL_BG);
    y += 20;
    y += 14;
    draw_stats_histogram(fb, lx, y, latency_hist, hist_idx);
}

// Desktop icons (16x16 disk sprite drawn at 2x)
#[rustfmt::skip]
const DISK_ICON: [[u8; 16]; 16] = [
    [0,0,1,1,1,1,1,1,1,1,1,1,1,1,0,0],
    [0,1,3,3,3,3,3,3,3,3,3,3,3,3,1,0],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,4,4,4,4,4,4,4,4,4,4,2,3,1],
    [1,3,2,4,4,4,4,4,4,4,4,4,4,2,3,1],
    [1,3,2,4,4,4,4,4,4,4,4,4,4,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,2,2,2,2,2,2,2,2,2,2,3,1],
    [1,3,2,2,1,1,1,1,1,1,2,2,2,2,3,1],
    [1,3,2,2,1,3,3,3,3,1,2,2,2,2,3,1],
    [1,3,2,2,1,3,3,3,3,1,2,2,2,2,3,1],
    [0,1,3,3,1,3,3,3,3,1,3,3,3,3,1,0],
    [0,0,1,1,1,1,1,1,1,1,1,1,1,1,0,0],
];

fn draw_icon(fb: &mut Framebuffer, ix: usize, iy: usize, accent: Color) {
    for row in 0..16 {
        for col in 0..16 {
            let c = match DISK_ICON[row][col] {
                1 => Color::new(0x20, 0x25, 0x30),
                2 => blend(accent, DESKTOP_BG, 80),
                3 => blend(Color::new(0xFF, 0xFF, 0xFF), accent, 40),
                4 => Color::new(0x18, 0x1C, 0x24),
                _ => continue,
            };
            for sy in 0..2 {
                for sx in 0..2 {
                    fb.put_pixel(ix + col * 2 + sx, iy + row * 2 + sy, c);
                }
            }
        }
    }
}

fn draw_desktop_icons(fb: &mut Framebuffer) {
    let ix = 30;
    draw_icon(fb, ix, 50, ACCENT);
    fb.draw_aa_string(ix - 4, 86, "System", SUBTLE, DESKTOP_BG);
    draw_icon(fb, ix, 120, ORANGE);
    fb.draw_aa_string(ix - 4, 156, "Disk 0", SUBTLE, DESKTOP_BG);
}

// ============================================================================
// Task label helpers for message trace
// ============================================================================

const TASK_LABELS: [&str; 8] = ["idle", "kbd", "comp", "shell", "dash", "ping", "pong", "?"];
const TASK_COLORS: [Color; 8] = [
    DIM,
    ACCENT,
    SUBTLE,
    GREEN,
    ORANGE,
    Color::new(0x4F, 0x8B, 0xFF),
    Color::new(0xFF, 0xC1, 0x58),
    DIM,
];

fn tag_label(tag: u16) -> &'static str {
    match tag as u32 {
        ipc::MSG_IRQ => "IRQ",
        ipc::MSG_KEY_DOWN => "KEY",
        ipc::MSG_KEY_UP => "KEY_UP",
        ipc::MSG_PING => "PING",
        ipc::MSG_PONG => "PONG",
        _ => "MSG",
    }
}

// Ramfb display framebuffer — stored here so compositor_el1 can use it for the
// fallback path without passing it through the virtio-GPU init.
static mut RAMFB_DISPLAY: Option<Framebuffer> = None;

// ============================================================================
// Compositor — spatial workspaces, overlapping windows, virtio-GPU or ramfb
// ============================================================================

pub fn compositor_el1() -> ! {
    // Try virtio-GPU; fall back to ramfb if unavailable
    let (mut gpu, mut bb, sw, sh) =
        if let Some((gpu, fb)) = crate::arch::virtio_gpu::VirtioGpu::init() {
            let w = fb.width();
            let h = fb.height();
            crate::serial::serial_println!("[comp] virtio-GPU {}x{}", w, h);
            (Some(gpu), fb, w, h)
        } else {
            let (display_fb, fbi) = fbinfo();
            let w = fbi.width as usize;
            let h = fbi.height as usize;
            // Allocate back buffer for ramfb path
            let bb_bytes = w * h * 4;
            let bb_pages = (bb_bytes + 4095) / 4096;
            let bb_addr = crate::frame_alloc::allocate_contiguous(bb_pages).expect("back buffer");
            unsafe { core::ptr::write_bytes(bb_addr as *mut u8, 0, bb_bytes) };
            let bb = Framebuffer::new(bb_addr as *mut u8, w, h, w, fbi.is_bgr != 0);
            crate::serial::serial_println!("[comp] ramfb {}x{}", w, h);
            // Store display fb for ramfb flip
            // SAFETY: only this task touches RAMFB_DISPLAY
            unsafe { RAMFB_DISPLAY = Some(display_fb) };
            (None, bb, w, h)
        };

    let is_bgr = bb.is_bgr();
    let desktop_bytes = sw * sh * 4;
    let desktop_pages = (desktop_bytes + 4095) / 4096;
    let desktop_addr =
        crate::frame_alloc::allocate_contiguous(desktop_pages).expect("desktop cache");
    unsafe { core::ptr::write_bytes(desktop_addr as *mut u8, 0, desktop_bytes) };
    let mut desktop_cache = Framebuffer::new(desktop_addr as *mut u8, sw, sh, sw, bb.is_bgr());
    desktop_cache.clear(DESKTOP_BG);
    draw_desktop_icons(&mut desktop_cache);
    bb.copy_rect_from(&desktop_cache, 0, 0, sw, sh, 0, 0);
    let menu_bar_cache = build_menu_bar_cache(sw, is_bgr);
    let stats_overlay_cache = build_stats_overlay_cache(is_bgr);
    let shell_frame_active = build_window_frame_cache("Shell", GREEN, true, is_bgr);
    let shell_frame_inactive = build_window_frame_cache("Shell", GREEN, false, is_bgr);
    let dash_frame_active = build_window_frame_cache("Dashboard", ORANGE, true, is_bgr);
    let dash_frame_inactive = build_window_frame_cache("Dashboard", ORANGE, false, is_bgr);

    let s0 = surface(0);
    let s1 = surface(1);
    let tbar_y = sh - TBAR_H;
    let content_top = MENU_H + 4;
    let content_h = tbar_y.saturating_sub(4).saturating_sub(content_top);
    let stats_x = sw.saturating_sub(STATS_PANEL_W + 16);
    let stats_y = MENU_H + 12;
    let active_x = (sw - SURF_W) / 2;
    let active_y = content_top + content_h.saturating_sub(SURF_H + WIN_TITLE_H) / 2 + WIN_TITLE_H;

    let mut active_ws: usize = 0;
    let mut photon_hist = [0u64; 16];
    let mut hist_idx: usize = 0;
    let mut pending_workspace_input_ns: u64 = 0;
    let mut last_presented_shell_seq: u64 = 0;
    let mut last_shell_surface_seq: u64 = 0;
    let mut last_dashboard_surface_seq: u64 = 0;
    let mut last_clock_second: u64 = u64::MAX;
    let mut last_menu_latency_bucket: u64 = u64::MAX;
    let mut last_menu_task_count: u64 = u64::MAX;
    let mut last_overlay_second: u64 = u64::MAX;
    let mut last_overlay_task_count: u64 = u64::MAX;
    let mut last_overlay_channel_count: u64 = u64::MAX;
    let mut photon_hist_revision: u64 = 0;
    let mut last_overlay_hist_revision: u64 = u64::MAX;
    let mut last_redraw_log_second: u64 = u64::MAX;
    let mut redraws_this_second: u64 = 0;
    let mut content_redraws_this_second: u64 = 0;
    let mut frame_counter: u64 = 0;
    let mut needs_full_content = true;
    let mut menu_dirty = true;
    let mut stats_dirty = true;
    let mut taskbar_dirty = true;
    let mut full_present_needed = true;

    loop {
        let mut full_content_redraw = needs_full_content;
        let mut redraw_active_window_full = false;
        let mut redraw_inactive_window_full = false;
        let mut active_surface_damage: Option<Rect> = None;

        while let Some(msg) = ipc::try_recv(CH_KBD_EVENTS) {
            if msg.tag != ipc::MSG_KEY_DOWN {
                continue;
            }

            let ch = msg.payload[0] as u8;
            let input_ns = msg.payload[2];
            match ch {
                b'\t' | b'`' => {
                    active_ws = 1 - active_ws;
                    full_content_redraw = true;
                    needs_full_content = false;
                    stats_dirty = true;
                    taskbar_dirty = true;
                    pending_workspace_input_ns = input_ns;
                }
                b'1' => {
                    if active_ws != 0 {
                        active_ws = 0;
                        full_content_redraw = true;
                        needs_full_content = false;
                        stats_dirty = true;
                        taskbar_dirty = true;
                        pending_workspace_input_ns = input_ns;
                    }
                }
                b'2' => {
                    if active_ws != 1 {
                        active_ws = 1;
                        full_content_redraw = true;
                        needs_full_content = false;
                        stats_dirty = true;
                        taskbar_dirty = true;
                        pending_workspace_input_ns = input_ns;
                    }
                }
                _ if active_ws == 0 => {
                    let _ = ipc::send(CH_SHELL_KEYS, &msg);
                }
                _ => {}
            }
        }

        let frame_start_ns = time_ns();
        let ns = frame_start_ns;
        let metrics = crate::metrics::snapshot();
        let clock_second = ns / 1_000_000_000;
        let tasks_now = task_count();
        let channels_now = channel_count();
        let menu_latency_bucket = hud_metric_bucket(metrics.input_to_photon_us.latest);
        let shell_input_ns_before = crate::metrics::shell_input_ns();
        let (shell_seq_before, shell_damage_raw) = crate::metrics::surface_damage_snapshot(0);
        let (dashboard_seq_before, _) = crate::metrics::surface_damage_snapshot(1);

        if clock_second != last_redraw_log_second {
            if last_redraw_log_second != u64::MAX {
                crate::serial::serial_println!(
                    "[comp] redraws={} content={}",
                    redraws_this_second,
                    content_redraws_this_second
                );
            }
            last_redraw_log_second = clock_second;
            redraws_this_second = 0;
            content_redraws_this_second = 0;
        }

        if shell_seq_before != last_shell_surface_seq {
            if active_ws == 0 {
                if let Some(damage) = metrics_damage_rect_to_rect(shell_damage_raw) {
                    extend_damage(&mut active_surface_damage, damage);
                } else {
                    redraw_active_window_full = true;
                }
            } else {
                redraw_inactive_window_full = true;
                redraw_active_window_full = true;
            }
            last_shell_surface_seq = shell_seq_before;
            crate::metrics::clear_surface_damage(0, shell_seq_before);
        }
        if dashboard_seq_before != last_dashboard_surface_seq {
            if active_ws == 1 {
                redraw_active_window_full = true;
            }
            last_dashboard_surface_seq = dashboard_seq_before;
            crate::metrics::clear_surface_damage(1, dashboard_seq_before);
        }
        if clock_second != last_clock_second
            || menu_latency_bucket != last_menu_latency_bucket
            || tasks_now != last_menu_task_count
        {
            menu_dirty = true;
        }
        if clock_second != last_overlay_second
            || tasks_now != last_overlay_task_count
            || channels_now != last_overlay_channel_count
            || photon_hist_revision != last_overlay_hist_revision
        {
            stats_dirty = true;
        }

        let content_dirty = full_content_redraw
            || redraw_active_window_full
            || redraw_inactive_window_full
            || active_surface_damage.is_some();
        if !(content_dirty || menu_dirty || stats_dirty || taskbar_dirty) {
            yield_now();
            continue;
        }
        redraws_this_second += 1;
        if content_dirty {
            content_redraws_this_second += 1;
        }
        if full_content_redraw {
            needs_full_content = false;
        }

        let mut background_us = 0;
        let mut windows_us = 0;
        let inactive_x = if active_ws == 0 {
            active_x + 60
        } else {
            active_x.saturating_sub(60)
        };
        let inactive_y = active_y + 20;
        let active_outer = outer_rect_for_window(active_x, active_y);
        let inactive_outer = outer_rect_for_window(inactive_x, inactive_y);
        let (active_surface, active_frame_cache, inactive_surface, inactive_frame_cache) =
            if active_ws == 0 {
                (&s0, &shell_frame_active, &s1, &dash_frame_inactive)
            } else {
                (&s1, &dash_frame_active, &s0, &shell_frame_inactive)
            };
        let mut content_present: Option<Rect> = None;

        if full_content_redraw {
            let background_start_ns = time_ns();
            bb.copy_rect_from(
                &desktop_cache,
                0,
                content_top,
                sw,
                content_h,
                0,
                content_top,
            );
            let background_end_ns = time_ns();
            background_us = (background_end_ns - background_start_ns) / 1000;

            let windows_start_ns = background_end_ns;
            blit_full_window(
                &mut bb,
                inactive_frame_cache,
                inactive_surface,
                inactive_x,
                inactive_y,
            );
            blit_full_window(
                &mut bb,
                active_frame_cache,
                active_surface,
                active_x,
                active_y,
            );
            let windows_end_ns = time_ns();
            windows_us = (windows_end_ns - windows_start_ns) / 1000;
            content_present = Some(Rect {
                x: 0,
                y: content_top,
                w: sw,
                h: content_h,
            });
        } else if content_dirty {
            let windows_start_ns = time_ns();
            if redraw_inactive_window_full {
                blit_full_window(
                    &mut bb,
                    inactive_frame_cache,
                    inactive_surface,
                    inactive_x,
                    inactive_y,
                );
                content_present = Some(match content_present {
                    Some(existing) => rect_union(existing, inactive_outer),
                    None => inactive_outer,
                });
            }
            if redraw_active_window_full {
                blit_full_window(
                    &mut bb,
                    active_frame_cache,
                    active_surface,
                    active_x,
                    active_y,
                );
                content_present = Some(match content_present {
                    Some(existing) => rect_union(existing, active_outer),
                    None => active_outer,
                });
            } else if let Some(damage) = active_surface_damage {
                bb.copy_rect_from(
                    active_surface,
                    damage.x,
                    damage.y,
                    damage.w,
                    damage.h,
                    active_x + damage.x,
                    active_y + damage.y,
                );
                let screen_damage = Rect {
                    x: active_x + damage.x,
                    y: active_y + damage.y,
                    w: damage.w,
                    h: damage.h,
                };
                content_present = Some(match content_present {
                    Some(existing) => rect_union(existing, screen_damage),
                    None => screen_damage,
                });
            }
            if redraw_inactive_window_full && !redraw_active_window_full {
                if rect_intersection(inactive_outer, active_outer).is_some() {
                    blit_full_window(
                        &mut bb,
                        active_frame_cache,
                        active_surface,
                        active_x,
                        active_y,
                    );
                    content_present = Some(match content_present {
                        Some(existing) => rect_union(existing, active_outer),
                        None => active_outer,
                    });
                }
            }
            let windows_end_ns = time_ns();
            windows_us = (windows_end_ns - windows_start_ns) / 1000;
        }

        let chrome_start_ns = time_ns();
        if menu_dirty {
            draw_menu_bar(
                &mut bb,
                &menu_bar_cache,
                sw,
                ns,
                metrics.input_to_photon_us.latest,
                tasks_now,
            );
            last_clock_second = clock_second;
            last_menu_latency_bucket = menu_latency_bucket;
            last_menu_task_count = tasks_now;
        }
        if stats_dirty {
            draw_stats_overlay(
                &mut bb,
                &stats_overlay_cache,
                sw,
                ns,
                metrics,
                &photon_hist,
                hist_idx,
                tasks_now,
                channels_now,
            );
            last_overlay_second = clock_second;
            last_overlay_task_count = tasks_now;
            last_overlay_channel_count = channels_now;
            last_overlay_hist_revision = photon_hist_revision;
        }
        if taskbar_dirty {
            draw_taskbar(&mut bb, sw, sh, active_ws);
        }
        let chrome_end_ns = time_ns();

        let present_start_ns = time_ns();
        if let Some(ref mut g) = gpu {
            if full_present_needed {
                g.present_full();
                full_present_needed = false;
            } else {
                if menu_dirty {
                    g.present_rect(0, 0, sw as u32, (MENU_H + 1) as u32);
                }
                if let Some(rect) = content_present {
                    g.present_rect(rect.x as u32, rect.y as u32, rect.w as u32, rect.h as u32);
                }
                if stats_dirty {
                    g.present_rect(
                        stats_x as u32,
                        stats_y as u32,
                        STATS_PANEL_W as u32,
                        STATS_PANEL_H as u32,
                    );
                }
                if taskbar_dirty {
                    g.present_rect(0, tbar_y as u32, sw as u32, TBAR_H as u32);
                }
            }
        } else {
            unsafe {
                if let Some(ref mut display) = RAMFB_DISPLAY {
                    if full_present_needed {
                        bb.copy_to(display);
                        full_present_needed = false;
                    } else {
                        if menu_dirty {
                            bb.copy_rect_to(display, 0, 0, sw, MENU_H + 1, 0, 0);
                        }
                        if let Some(rect) = content_present {
                            bb.copy_rect_to(
                                display, rect.x, rect.y, rect.w, rect.h, rect.x, rect.y,
                            );
                        }
                        if stats_dirty {
                            bb.copy_rect_to(
                                display,
                                stats_x,
                                stats_y,
                                STATS_PANEL_W,
                                STATS_PANEL_H,
                                stats_x,
                                stats_y,
                            );
                        }
                        if taskbar_dirty {
                            bb.copy_rect_to(display, 0, tbar_y, sw, TBAR_H, 0, tbar_y);
                        }
                    }
                }
            }
        }
        let frame_end_ns = time_ns();

        crate::metrics::record_background_us(background_us);
        crate::metrics::record_windows_us(windows_us);
        crate::metrics::record_chrome_us((chrome_end_ns - chrome_start_ns) / 1000);
        crate::metrics::record_present_us((frame_end_ns - present_start_ns) / 1000);
        crate::metrics::record_frame_us((frame_end_ns - frame_start_ns) / 1000);
        frame_counter += 1;

        let mut recorded_photon_us = 0;
        if content_present.is_some()
            && shell_seq_before > last_presented_shell_seq
            && shell_input_ns_before > 0
        {
            last_presented_shell_seq = shell_seq_before;
            if frame_end_ns > shell_input_ns_before {
                recorded_photon_us = (frame_end_ns - shell_input_ns_before) / 1000;
            }
        } else if content_present.is_some()
            && pending_workspace_input_ns > 0
            && frame_end_ns > pending_workspace_input_ns
        {
            recorded_photon_us = (frame_end_ns - pending_workspace_input_ns) / 1000;
            pending_workspace_input_ns = 0;
        }
        if recorded_photon_us > 0 {
            crate::metrics::record_input_to_photon_us(recorded_photon_us);
            photon_hist[hist_idx] = recorded_photon_us;
            hist_idx = (hist_idx + 1) % photon_hist.len();
            photon_hist_revision += 1;
            crate::serial::serial_println!("[photon] {}us", recorded_photon_us);
        }

        if frame_counter % 240 == 0 {
            let metrics = crate::metrics::snapshot();
            crate::serial::serial_println!(
                "[metrics] frame={}us bg={}us win={}us ui={}us present={}us photon={}us ipc_rtt={}us wake={}us",
                metrics.frame_us.latest,
                metrics.background_us.latest,
                metrics.windows_us.latest,
                metrics.chrome_us.latest,
                metrics.present_us.latest,
                metrics.input_to_photon_us.latest,
                metrics.ipc_rtt_us.latest,
                metrics.sched_wake_us.latest
            );
        }

        menu_dirty = false;
        stats_dirty = false;
        taskbar_dirty = false;

        yield_now();
    }
}

// ============================================================================
// Shell — renders to surface 0, receives keys via IPC
// ============================================================================

struct LineBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> LineBuf<N> {
    fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len < N {
            self.buf[self.len] = byte;
            self.len += 1;
        }
    }

    fn push_str(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            self.push_byte(byte);
        }
    }

    fn push_u64(&mut self, value: u64) {
        let mut digits = [0u8; 20];
        let len = fmt_u64(value, &mut digits);
        if let Ok(text) = core::str::from_utf8(&digits[..len]) {
            self.push_str(text);
        }
    }

    fn pad_to(&mut self, width: usize) {
        while self.len < width {
            self.push_byte(b' ');
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }
}

fn shell_clear_body(
    surf: &mut Framebuffer,
    left: usize,
    start_y: usize,
    damage: &mut Option<Rect>,
) {
    surf.draw_rect(left, start_y, SURF_W - left, SURF_H - start_y, SHELL_BG);
    extend_damage(
        damage,
        Rect {
            x: left,
            y: start_y,
            w: SURF_W - left,
            h: SURF_H - start_y,
        },
    );
}

fn shell_advance_line(
    surf: &mut Framebuffer,
    left: usize,
    start_y: usize,
    max_y: usize,
    line_h: usize,
    cy: &mut usize,
    damage: &mut Option<Rect>,
) {
    *cy += line_h;
    if *cy >= max_y {
        *cy = start_y;
        shell_clear_body(surf, left, start_y, damage);
    }
}

fn shell_draw_prompt(
    surf: &mut Framebuffer,
    left: usize,
    cy: usize,
    char_w: usize,
    damage: &mut Option<Rect>,
) -> usize {
    let mut cx = left;
    surf.draw_aa_char(cx, cy, '>', ACCENT, SHELL_BG);
    cx += char_w;
    surf.draw_aa_char(cx, cy, ' ', ACCENT, SHELL_BG);
    cx += char_w;
    extend_damage(
        damage,
        Rect {
            x: left,
            y: cy,
            w: 2 * char_w,
            h: font_aa::GLYPH_H,
        },
    );
    cx
}

fn shell_print_line(
    surf: &mut Framebuffer,
    left: usize,
    start_y: usize,
    max_y: usize,
    line_h: usize,
    cy: &mut usize,
    text: &str,
    color: Color,
    damage: &mut Option<Rect>,
) {
    let max_cols = (SURF_W.saturating_sub(left + 4)) / font_aa::GLYPH_W;
    let text = if text.len() > max_cols {
        &text[..max_cols]
    } else {
        text
    };
    surf.draw_rect(left, *cy, SURF_W - left, font_aa::GLYPH_H, SHELL_BG);
    surf.draw_aa_string(left, *cy, text, color, SHELL_BG);
    extend_damage(
        damage,
        Rect {
            x: left,
            y: *cy,
            w: SURF_W - left,
            h: font_aa::GLYPH_H,
        },
    );
    crate::serial::serial_println!("[shell] {}", text);
    shell_advance_line(surf, left, start_y, max_y, line_h, cy, damage);
}

fn shell_service_state_label(status: crate::init_abi::ServiceStatus) -> &'static str {
    if status.state & crate::init_abi::SERVICE_STATE_RUNNING != 0 {
        "run"
    } else if status.last_exit_reason == crate::init_abi::SERVICE_EXIT_FAULT {
        "fault"
    } else if status.last_exit_reason == crate::init_abi::SERVICE_EXIT_CLEAN {
        "down"
    } else {
        "idle"
    }
}

fn shell_service_exit_label(reason: u64) -> &'static str {
    match reason {
        crate::init_abi::SERVICE_EXIT_CLEAN => "clean",
        crate::init_abi::SERVICE_EXIT_FAULT => "fault",
        _ => "-",
    }
}

fn shell_service_line_color(status: crate::init_abi::ServiceStatus) -> Color {
    if status.state & crate::init_abi::SERVICE_STATE_RUNNING != 0 {
        TEXT
    } else if status.last_exit_reason == crate::init_abi::SERVICE_EXIT_FAULT {
        ORANGE
    } else {
        SUBTLE
    }
}

fn shell_run_command(
    surf: &mut Framebuffer,
    left: usize,
    start_y: usize,
    max_y: usize,
    line_h: usize,
    cy: &mut usize,
    command: &str,
    damage: &mut Option<Rect>,
) {
    let command = command.trim();
    if command.is_empty() {
        return;
    }

    crate::serial::serial_println!("[shell] > {}", command);

    let mut parts = command.split_ascii_whitespace();
    match parts.next() {
        Some("help") => {
            shell_print_line(
                surf,
                left,
                start_y,
                max_y,
                line_h,
                cy,
                "commands: help services ps restart <name>",
                SUBTLE,
                damage,
            );
        }
        Some("services") | Some("ps") => {
            shell_print_line(
                surf,
                left,
                start_y,
                max_y,
                line_h,
                cy,
                "name    state  task rs ex last",
                SUBTLE,
                damage,
            );

            for index in 0..crate::init_abi::service_count() {
                let Some(service) = crate::init_abi::service_record(index) else {
                    continue;
                };
                let Some(status) = crate::init_abi::service_status(service.id) else {
                    continue;
                };

                let mut line = LineBuf::<80>::new();
                line.push_str(service.name);
                line.pad_to(8);
                line.push_str(shell_service_state_label(status));
                line.pad_to(15);
                if status.task_id == 0 {
                    line.push_byte(b'-');
                } else {
                    line.push_u64(status.task_id);
                }
                line.pad_to(20);
                line.push_u64(status.restart_count);
                line.pad_to(23);
                line.push_u64(status.exit_count);
                line.pad_to(26);
                line.push_str(shell_service_exit_label(status.last_exit_reason));

                shell_print_line(
                    surf,
                    left,
                    start_y,
                    max_y,
                    line_h,
                    cy,
                    line.as_str(),
                    shell_service_line_color(status),
                    damage,
                );
            }
        }
        Some("restart") | Some("start") => {
            let Some(service_name) = parts.next() else {
                shell_print_line(
                    surf,
                    left,
                    start_y,
                    max_y,
                    line_h,
                    cy,
                    "usage: restart <name>",
                    ORANGE,
                    damage,
                );
                return;
            };
            if parts.next().is_some() {
                shell_print_line(
                    surf,
                    left,
                    start_y,
                    max_y,
                    line_h,
                    cy,
                    "usage: restart <name>",
                    ORANGE,
                    damage,
                );
                return;
            }

            let Some(service) = crate::init_abi::find_service(service_name) else {
                let mut line = LineBuf::<80>::new();
                line.push_str("unknown service: ");
                line.push_str(service_name);
                shell_print_line(
                    surf,
                    left,
                    start_y,
                    max_y,
                    line_h,
                    cy,
                    line.as_str(),
                    ORANGE,
                    damage,
                );
                return;
            };

            if let Some(status) = crate::init_abi::service_status(service.id) {
                if status.state & crate::init_abi::SERVICE_STATE_RUNNING != 0 {
                    let mut line = LineBuf::<80>::new();
                    line.push_str(service.name);
                    line.push_str(" already running");
                    if status.task_id > 0 {
                        line.push_str(" (task ");
                        line.push_u64(status.task_id);
                        line.push_byte(b')');
                    }
                    shell_print_line(
                        surf,
                        left,
                        start_y,
                        max_y,
                        line_h,
                        cy,
                        line.as_str(),
                        SUBTLE,
                        damage,
                    );
                    return;
                }
            }

            match crate::init_abi::spawn_service(service.id) {
                Ok(task_id) => {
                    let mut line = LineBuf::<80>::new();
                    line.push_str("started ");
                    line.push_str(service.name);
                    line.push_str(" task ");
                    line.push_u64(task_id as u64);
                    shell_print_line(
                        surf,
                        left,
                        start_y,
                        max_y,
                        line_h,
                        cy,
                        line.as_str(),
                        GREEN,
                        damage,
                    );
                }
                Err(code) => {
                    let mut line = LineBuf::<80>::new();
                    line.push_str("restart failed ");
                    line.push_str(service.name);
                    line.push_str(" code ");
                    if code < 0 {
                        line.push_byte(b'-');
                        line.push_u64((-code) as u64);
                    } else {
                        line.push_u64(code as u64);
                    }
                    shell_print_line(
                        surf,
                        left,
                        start_y,
                        max_y,
                        line_h,
                        cy,
                        line.as_str(),
                        ORANGE,
                        damage,
                    );
                }
            }
        }
        Some(other) => {
            let mut line = LineBuf::<80>::new();
            line.push_str("unknown command: ");
            line.push_str(other);
            shell_print_line(
                surf,
                left,
                start_y,
                max_y,
                line_h,
                cy,
                line.as_str(),
                ORANGE,
                damage,
            );
        }
        None => {}
    }
}

pub fn shell_el1() -> ! {
    let mut surf = surface(0);
    surf.clear(SHELL_BG);

    surf.draw_rect(0, 0, SURF_W, 38, Color::new(0x0D, 0x12, 0x18));
    surf.draw_aa_string(14, 10, "~  shell", GREEN, Color::new(0x0D, 0x12, 0x18));
    surf.draw_rect(0, 38, SURF_W, 1, SEP);

    let left = 14;
    let char_w = font_aa::GLYPH_W;
    let line_h = font_aa::GLYPH_H + 3;
    let start_y = 48;
    let max_y = SURF_H - line_h;
    let prompt_x = left + 2 * char_w;
    let mut command_buf = [0u8; 64];
    let mut command_len: usize = 0;
    let mut cy = start_y;

    // Prompt
    let mut initial_damage = None;
    let mut cx = shell_draw_prompt(&mut surf, left, cy, char_w, &mut initial_damage);
    if let Some(damage) = initial_damage {
        crate::metrics::mark_surface_damage(
            0,
            damage.x as u32,
            damage.y as u32,
            damage.w as u32,
            damage.h as u32,
        );
    }

    crate::serial::serial_println!("[shell] waiting for keys");

    loop {
        match ipc::recv(CH_SHELL_KEYS) {
            Ok(msg) => {
                if msg.tag != ipc::MSG_KEY_DOWN {
                    continue;
                }
                let ch = msg.payload[0] as u8;
                let input_ns = msg.payload[2];
                let mut changed = false;
                let mut damage: Option<Rect> = None;
                match ch {
                    b'\n' => {
                        let command =
                            core::str::from_utf8(&command_buf[..command_len]).unwrap_or("");
                        shell_advance_line(
                            &mut surf,
                            left,
                            start_y,
                            max_y,
                            line_h,
                            &mut cy,
                            &mut damage,
                        );
                        shell_run_command(
                            &mut surf,
                            left,
                            start_y,
                            max_y,
                            line_h,
                            &mut cy,
                            command,
                            &mut damage,
                        );
                        cx = shell_draw_prompt(&mut surf, left, cy, char_w, &mut damage);
                        command_len = 0;
                        changed = true;
                    }
                    0x7F | 0x08 => {
                        if command_len > 0 && cx > prompt_x {
                            command_len -= 1;
                            cx -= char_w;
                            surf.draw_rect(cx, cy, char_w, font_aa::GLYPH_H, SHELL_BG);
                            extend_damage(
                                &mut damage,
                                Rect {
                                    x: cx,
                                    y: cy,
                                    w: char_w,
                                    h: font_aa::GLYPH_H,
                                },
                            );
                            changed = true;
                        }
                    }
                    0x20..=0x7E => {
                        if command_len >= command_buf.len() {
                            continue;
                        }
                        let draw_x = cx;
                        let draw_y = cy;
                        surf.draw_aa_char(cx, cy, ch as char, GREEN, SHELL_BG);
                        command_buf[command_len] = ch;
                        command_len += 1;
                        cx += char_w;
                        if cx + char_w >= SURF_W {
                            shell_advance_line(
                                &mut surf,
                                left,
                                start_y,
                                max_y,
                                line_h,
                                &mut cy,
                                &mut damage,
                            );
                            cx = left;
                        }
                        extend_damage(
                            &mut damage,
                            Rect {
                                x: draw_x,
                                y: draw_y,
                                w: char_w,
                                h: font_aa::GLYPH_H,
                            },
                        );
                        changed = true;
                    }
                    _ => {}
                }
                if changed {
                    if let Some(damage) = damage {
                        crate::metrics::mark_shell_surface_damage(
                            input_ns,
                            damage.x as u32,
                            damage.y as u32,
                            damage.w as u32,
                            damage.h as u32,
                        );
                    } else {
                        crate::metrics::mark_shell_surface_damage(
                            input_ns,
                            0,
                            0,
                            SURF_W as u32,
                            SURF_H as u32,
                        );
                    }
                }
            }
            Err(_) => {
                yield_now();
            }
        }
    }
}

// ============================================================================
// Dashboard — renders to surface 1, shows system info + message trace
// ============================================================================

pub fn dashboard_el1() -> ! {
    let mut surf = surface(1);

    let mut trace_buf = [ipc::TraceEntry {
        timestamp_ns: 0,
        from_task: 0,
        to_task: 0,
        channel: 0,
        tag: 0,
    }; 16];

    crate::serial::serial_println!("[dash] running");
    let mut last_metrics_log_ns: u64 = 0;
    let mut next_redraw_ns: u64 = 0;

    loop {
        let ns = time_ns();
        if ns < next_redraw_ns {
            yield_now();
            continue;
        }
        next_redraw_ns = ns.saturating_add(UI_REFRESH_NS);

        surf.clear(DASH_BG);

        // Header
        surf.draw_rect(0, 0, SURF_W, 38, Color::new(0x0D, 0x12, 0x18));
        surf.draw_aa_string(14, 10, "~  dashboard", ORANGE, Color::new(0x0D, 0x12, 0x18));
        surf.draw_rect(0, 38, SURF_W, 1, SEP);

        let metrics = crate::metrics::snapshot();
        if ns.saturating_sub(last_metrics_log_ns) >= 1_000_000_000 {
            last_metrics_log_ns = ns;
            crate::serial::serial_println!(
                "[metrics] frame={}us bg={}us win={}us ui={}us present={}us photon={}us ipc_rtt={}us wake={}us",
                metrics.frame_us.latest,
                metrics.background_us.latest,
                metrics.windows_us.latest,
                metrics.chrome_us.latest,
                metrics.present_us.latest,
                metrics.input_to_photon_us.latest,
                metrics.ipc_rtt_us.latest,
                metrics.sched_wake_us.latest
            );
        }
        let mut y = 50;

        // Clock
        let mut hms = [0u8; 8];
        fmt_hms(ns, &mut hms);
        let ts = core::str::from_utf8(&hms).unwrap_or("??:??:??");
        surf.draw_aa_string_2x(14, y, ts, TEXT, DASH_BG);
        y += 40;

        surf.draw_aa_string(14, y, "uptime", DIM, DASH_BG);
        y += 22;
        surf.draw_rect(14, y, SURF_W - 28, 1, SEP);
        y += 10;

        // System info
        surf.draw_aa_string(14, y, "Tasks", SUBTLE, DASH_BG);
        {
            let mut buf = [0u8; 16];
            let len = fmt_u64(task_count(), &mut buf);
            let s = core::str::from_utf8(&buf[..len]).unwrap_or("?");
            surf.draw_aa_string(110, y, s, TEXT, DASH_BG);
            surf.draw_aa_string(
                110 + len * font_aa::GLYPH_W + 6,
                y,
                "EL1 @ 1 kHz",
                TEXT,
                DASH_BG,
            );
        }
        y += 18;
        surf.draw_aa_string(14, y, "Arch", SUBTLE, DASH_BG);
        surf.draw_aa_string(110, y, "aarch64 (Apple Silicon)", TEXT, DASH_BG);
        y += 18;
        surf.draw_aa_string(14, y, "Chans", SUBTLE, DASH_BG);
        {
            let mut buf = [0u8; 16];
            let len = fmt_u64(channel_count(), &mut buf);
            let s = core::str::from_utf8(&buf[..len]).unwrap_or("?");
            surf.draw_aa_string(110, y, s, TEXT, DASH_BG);
        }
        y += 18;
        draw_metric_pair_line(&mut surf, 14, y, "Frame", metrics.frame_us, DASH_BG);
        y += 18;
        draw_metric_pair_line(&mut surf, 14, y, "Present", metrics.present_us, DASH_BG);
        y += 18;
        draw_metric_pair_line(
            &mut surf,
            14,
            y,
            "Photon",
            metrics.input_to_photon_us,
            DASH_BG,
        );
        y += 18;
        draw_metric_pair_line(&mut surf, 14, y, "IPC RTT", metrics.ipc_rtt_us, DASH_BG);
        y += 18;
        draw_metric_pair_line(&mut surf, 14, y, "Wake", metrics.sched_wake_us, DASH_BG);
        y += 18;
        draw_phase_triplet_line(
            &mut surf,
            14,
            y,
            metrics.background_us.latest,
            metrics.windows_us.latest,
            metrics.chrome_us.latest,
            DASH_BG,
        );
        y += 22;

        // Message flow
        surf.draw_aa_string(14, y, "Message Flow", ORANGE, DASH_BG);
        y += 14;
        surf.draw_rect(14, y, SURF_W - 28, 1, SEP);
        y += 8;

        let now = time_ns();
        let count = trace_read(&mut trace_buf);

        if count == 0 {
            surf.draw_aa_string(14, y, "(waiting for messages...)", DIM, DASH_BG);
        } else {
            let show = count.min(4);
            let start = count.saturating_sub(show);

            for row in 0..show {
                let e = &trace_buf[start + row];
                let age_ms = if now > e.timestamp_ns {
                    (now - e.timestamp_ns) / 1_000_000
                } else {
                    0
                };
                let color = if age_ms < 300 {
                    GREEN
                } else if age_ms < 2000 {
                    SUBTLE
                } else {
                    DIM
                };

                let mut rx = 14;
                let gw = font_aa::GLYPH_W;

                // Age
                let mut abuf = [b' '; 4];
                let mut av = age_ms;
                let mut ai = 3;
                loop {
                    abuf[ai] = b'0' + (av % 10) as u8;
                    av /= 10;
                    if av == 0 || ai == 0 {
                        break;
                    }
                    ai -= 1;
                }
                for &b in &abuf {
                    surf.draw_aa_char(rx, y, b as char, DIM, DASH_BG);
                    rx += gw;
                }
                surf.draw_aa_string(rx, y, "ms", DIM, DASH_BG);
                rx += gw * 3;

                let fi = (e.from_task as usize).min(TASK_LABELS.len() - 1);
                let ti = (e.to_task as usize).min(TASK_LABELS.len() - 1);
                surf.draw_aa_string(rx, y, TASK_LABELS[fi], TASK_COLORS[fi], DASH_BG);
                rx += TASK_LABELS[fi].len() * gw;
                surf.draw_aa_string(rx, y, ">", color, DASH_BG);
                rx += gw;
                if (e.to_task as usize) < TASK_LABELS.len() - 1 {
                    surf.draw_aa_string(rx, y, TASK_LABELS[ti], TASK_COLORS[ti], DASH_BG);
                    rx += TASK_LABELS[ti].len() * gw + gw;
                } else {
                    surf.draw_aa_string(rx, y, "?", DIM, DASH_BG);
                    rx += gw * 2;
                }
                surf.draw_aa_string(rx, y, tag_label(e.tag), color, DASH_BG);
                y += 14;
            }
        }

        // Footer
        surf.draw_aa_string(
            14,
            SURF_H - 22,
            "The architecture is perceptible.",
            DIM,
            DASH_BG,
        );
        crate::metrics::mark_surface_damage(1, 0, 0, SURF_W as u32, SURF_H as u32);
        yield_now();
    }
}

// ============================================================================
// IPC probe — ping/pong round-trip benchmark over real channels
// ============================================================================

pub fn ipc_probe_ping_el1() -> ! {
    crate::serial::serial_println!("[probe] ping");
    let mut sample_count: u64 = 0;

    loop {
        let start_ns = time_ns();
        let mut msg = ipc::Message::new(ipc::MSG_PING);
        msg.payload[0] = start_ns;
        msg.len = 8;
        let _ = ipc::send(CH_IPC_PROBE_PING, &msg);

        if let Ok(reply) = ipc::recv(CH_IPC_PROBE_PONG) {
            if reply.tag == ipc::MSG_PONG {
                let sent_ns = reply.payload[0];
                let now_ns = time_ns();
                if sent_ns > 0 && now_ns > sent_ns {
                    let rtt_us = (now_ns - sent_ns) / 1000;
                    crate::metrics::record_ipc_rtt_us(rtt_us);
                    sample_count += 1;
                    if sample_count % 64 == 0 {
                        crate::serial::serial_println!("[ipc] rtt={}us", rtt_us);
                    }
                }
            }
        }

        for _ in 0..25 {
            yield_now();
        }
    }
}

pub fn ipc_probe_pong_el1() -> ! {
    crate::serial::serial_println!("[probe] pong");

    loop {
        if let Ok(msg) = ipc::recv(CH_IPC_PROBE_PING) {
            let mut reply = ipc::Message::new(ipc::MSG_PONG);
            reply.payload[0] = msg.payload[0];
            reply.len = 8;
            let _ = ipc::send(CH_IPC_PROBE_PONG, &reply);
        }
    }
}

pub fn supervised_pulse_el1() -> ! {
    crate::serial::serial_println!("[pulse] builtin start");

    for beat in 1..=3u64 {
        for _ in 0..500 {
            yield_now();
        }
        crate::serial::serial_println!("[pulse] builtin beat {}", beat);
    }

    crate::serial::serial_println!("[pulse] builtin exit");
    crate::arch::context::terminate_current_with_reason(crate::init_abi::SERVICE_EXIT_CLEAN)
}

pub fn supervised_fault_el1() -> ! {
    crate::serial::serial_println!("[fault] builtin start");
    for beat in 1..=2u64 {
        for _ in 0..300 {
            yield_now();
        }
        crate::serial::serial_println!("[fault] builtin beat {}", beat);
    }
    crate::serial::serial_println!("[fault] builtin crash");
    unsafe { core::arch::asm!("brk #0", options(noreturn)) }
}

// ============================================================================
// Keyboard — polls PL011 UART, sends key events via IPC
// ============================================================================

pub fn keyboard_el1() -> ! {
    crate::serial::serial_println!("[kbd] polling UART");

    loop {
        if let Some(byte) = crate::arch::serial_try_read() {
            let ch = if byte == b'\r' { b'\n' } else { byte };
            let irq_ns = time_ns(); // approximate — no real IRQ timestamp

            let mut msg = ipc::Message::new(ipc::MSG_KEY_DOWN);
            msg.payload[0] = ch as u64;
            msg.payload[2] = irq_ns; // for latency measurement
            let _ = ipc::send(CH_KBD_EVENTS, &msg);
        }
        yield_now();
    }
}
