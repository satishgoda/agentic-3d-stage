//! CPU-side mailbox live-feed panel rasterizer (bitmap text -> RGBA).
//!
//! The live-feed HUD starts as a title bar so the 3D view stays visible.
//! Expand (`+`) to fill the window, or undock into a floating panel.

use crate::feed::{FeedEvent, MailboxFeed, FEED_HIT_CAP};
use std::time::Instant;

pub const CHAR_W: usize = 5;
pub const CHAR_H: usize = 7;
pub const CELL_W: usize = 6; // 1px gap
pub const CELL_H: usize = 9;
pub const PAD_X: usize = 10;
pub const PAD_Y: usize = 10;
const STRIPE_W: usize = 3;
const CARD_PAD_Y: usize = 2;
const CARD_GAP: usize = 4;

pub const COL_APPLY: [u8; 4] = [90, 210, 170, 255];
pub const COL_QUERY: [u8; 4] = [230, 175, 70, 255];
pub const COL_STATUS: [u8; 4] = [150, 170, 200, 255];
pub const COL_INSPECT: [u8; 4] = [200, 160, 90, 255];
pub const COL_FAIL: [u8; 4] = [230, 110, 100, 255];
pub const COL_OK: [u8; 4] = [110, 200, 130, 255];
pub const COL_BODY: [u8; 4] = [230, 234, 240, 255];
pub const COL_MUTED: [u8; 4] = [150, 158, 168, 255];

/// Title bar height in physical pixels.
pub const TITLE_BAR_H: f32 = 28.0;
/// Square control size (`-`/`+` and `[]`) in physical pixels.
pub const CTRL_SIZE: f32 = 22.0;
pub const CTRL_GAP: f32 = 4.0;
/// Default floating panel size (physical pixels).
pub const FLOAT_W: f32 = 520.0;
pub const FLOAT_H: f32 = 320.0;
/// Bottom-right grip for floating resize.
pub const RESIZE_GRIP: f32 = 18.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudMode {
    Fullscreen,
    Floating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudHit {
    Title,
    Body,
    Collapse,
    Fullscreen,
    Resize,
    Outside,
}

#[derive(Clone, Debug)]
pub struct HudPanel {
    pub mode: HudMode,
    pub collapsed: bool,
    window_w: f32,
    window_h: f32,
    float_x: f32,
    float_y: f32,
    float_w: f32,
    float_h: f32,
}

impl HudPanel {
    pub fn new(window_w: f32, window_h: f32) -> Self {
        let ww = window_w.max(1.0);
        let wh = window_h.max(1.0);
        Self {
            mode: HudMode::Fullscreen,
            collapsed: true,
            window_w: ww,
            window_h: wh,
            float_x: 12.0,
            float_y: 12.0,
            float_w: FLOAT_W.min(ww).max(160.0),
            float_h: FLOAT_H.min(wh).max(TITLE_BAR_H + 48.0),
        }
    }

    pub fn title_height(&self) -> f32 {
        TITLE_BAR_H
    }

    pub fn set_window_size(&mut self, w: f32, h: f32) {
        self.window_w = w.max(1.0);
        self.window_h = h.max(1.0);
        self.clamp_float();
    }

    /// Panel rectangle in physical pixels, top-left origin: (x, y, w, h).
    pub fn rect(&self) -> (f32, f32, f32, f32) {
        match self.mode {
            HudMode::Fullscreen => {
                let h = if self.collapsed {
                    TITLE_BAR_H
                } else {
                    self.window_h
                };
                (0.0, 0.0, self.window_w, h)
            }
            HudMode::Floating => {
                let h = if self.collapsed {
                    TITLE_BAR_H
                } else {
                    self.float_h
                };
                (self.float_x, self.float_y, self.float_w, h)
            }
        }
    }

    pub fn pixel_size(&self) -> (u32, u32) {
        let (_, _, w, h) = self.rect();
        (w.max(1.0).round() as u32, h.max(1.0).round() as u32)
    }

    pub fn hit(&self, px: f32, py: f32) -> HudHit {
        let (x, y, w, h) = self.rect();
        if px < x || py < y || px >= x + w || py >= y + h {
            return HudHit::Outside;
        }
        let (col, fs) = self.control_rects();
        if point_in(px, py, fs) {
            return HudHit::Fullscreen;
        }
        if point_in(px, py, col) {
            return HudHit::Collapse;
        }
        if self.mode == HudMode::Floating && !self.collapsed && point_in(px, py, self.resize_grip()) {
            return HudHit::Resize;
        }
        if py < y + TITLE_BAR_H {
            return HudHit::Title;
        }
        if self.collapsed {
            HudHit::Title
        } else {
            HudHit::Body
        }
    }

    pub fn toggle_collapse(&mut self) {
        self.collapsed = !self.collapsed;
    }

    /// Leave fullscreen into a floating panel, pinning the title under `cursor`.
    pub fn undock_and_drag(&mut self, cursor_x: f32, cursor_y: f32) {
        if self.mode != HudMode::Fullscreen {
            return;
        }
        self.mode = HudMode::Floating;
        let local_x = cursor_x.clamp(12.0, (self.float_w - 56.0).max(12.0));
        self.float_x = cursor_x - local_x;
        self.float_y = (cursor_y - TITLE_BAR_H * 0.5).max(0.0);
        self.clamp_float();
    }

    pub fn restore_fullscreen(&mut self) {
        self.mode = HudMode::Fullscreen;
        self.collapsed = false;
    }

    pub fn drag_to(&mut self, origin_x: f32, origin_y: f32) {
        if self.mode == HudMode::Floating {
            self.float_x = origin_x;
            self.float_y = origin_y;
            self.clamp_float();
        }
    }

    pub fn resize_grip(&self) -> (f32, f32, f32, f32) {
        let (x, y, w, h) = self.rect();
        (
            x + w - RESIZE_GRIP,
            y + h - RESIZE_GRIP,
            RESIZE_GRIP,
            RESIZE_GRIP,
        )
    }

    /// Bottom-right of the floating panel follows the cursor.
    pub fn resize_to(&mut self, cursor_x: f32, cursor_y: f32) {
        if self.mode != HudMode::Floating || self.collapsed {
            return;
        }
        self.float_w = (cursor_x - self.float_x).max(1.0);
        self.float_h = (cursor_y - self.float_y).max(1.0);
        self.clamp_float();
    }

    fn control_rects(&self) -> ((f32, f32, f32, f32), (f32, f32, f32, f32)) {
        let (x, y, w, _) = self.rect();
        let cy = y + (TITLE_BAR_H - CTRL_SIZE) * 0.5;
        let fs = (
            x + w - CTRL_GAP - CTRL_SIZE,
            cy,
            CTRL_SIZE,
            CTRL_SIZE,
        );
        let col = (
            fs.0 - CTRL_GAP - CTRL_SIZE,
            cy,
            CTRL_SIZE,
            CTRL_SIZE,
        );
        (col, fs)
    }

    fn clamp_float(&mut self) {
        let min_w = 160.0;
        let min_h = TITLE_BAR_H + 48.0;
        self.float_w = self.float_w.clamp(min_w, self.window_w.max(min_w));
        self.float_h = self.float_h.clamp(min_h, self.window_h.max(min_h));
        let max_x = (self.window_w - 48.0).max(0.0);
        let max_y = (self.window_h - TITLE_BAR_H).max(0.0);
        self.float_x = self.float_x.clamp(0.0, max_x);
        self.float_y = self.float_y.clamp(0.0, max_y);
    }
}

fn point_in(px: f32, py: f32, r: (f32, f32, f32, f32)) -> bool {
    px >= r.0 && py >= r.1 && px < r.0 + r.2 && py < r.1 + r.3
}

pub struct HudFrame {
    pub pixels: Vec<u8>, // rgba8
    pub width: u32,
    pub height: u32,
    pub active: bool,
    pub event_count: usize,
}

pub fn glyph_scale(panel_h: u32) -> usize {
    // 2x only when there is real room. A 220px float used to pick 2x and then
    // skip every card because none of them fit.
    if panel_h > 360 {
        2
    } else {
        1
    }
}

pub fn op_color(op: &str) -> [u8; 4] {
    match op {
        "apply" => COL_APPLY,
        "query" => COL_QUERY,
        "status" => COL_STATUS,
        "inspect" => COL_INSPECT,
        _ => COL_MUTED,
    }
}

fn op_label(op: &str) -> String {
    match op {
        "apply" => "APPLY".into(),
        "query" => "QUERY".into(),
        "status" => "STATUS".into(),
        "inspect" => "INSPECT".into(),
        other => other.to_ascii_uppercase(),
    }
}

fn stripe_color(event: &FeedEvent) -> [u8; 4] {
    if event.ok {
        op_color(&event.op)
    } else {
        COL_FAIL
    }
}

pub fn rasterize_feed(
    feed: &MailboxFeed,
    now: Instant,
    width: u32,
    height: u32,
    solid: bool,
) -> HudFrame {
    let width = width.max(1);
    let height = height.max(1);
    let w = width as usize;
    let h = height as usize;
    let mut pixels = vec![0u8; w * h * 4];
    let collapsed = (h as f32) <= TITLE_BAR_H + 0.5;
    let scale = glyph_scale(height);

    let mut bmp = Bitmap {
        pixels: &mut pixels,
        w,
        h,
    };

    // Floating panel is a solid sheet. Fullscreen is a clear scene plus cards.
    if solid {
        fill_rect(&mut bmp, 0, 0, w, h, [18, 22, 28, 240]);
    }
    // Title bar
    fill_rect(
        &mut bmp,
        0,
        0,
        w,
        TITLE_BAR_H as usize,
        [24, 30, 38, 240],
    );
    // Accent bar
    fill_rect(&mut bmp, 0, 0, 4, h, [70, 150, 220, 255]);

    let active = feed.is_active(now);
    let status = if active { "ACTIVE" } else { "IDLE" };
    let header = format!("MAILBOX LIVE FEED   {status}  {}", feed.len());
    let ctrl_reserve = (CTRL_SIZE * 2.0 + CTRL_GAP * 3.0) as usize + PAD_X;
    let cell_w = CELL_W * scale;
    let header_cols = w.saturating_sub(PAD_X + 4 + ctrl_reserve) / cell_w.max(1);
    let glyph_h = CHAR_H * scale;
    let title_y = ((TITLE_BAR_H as usize).saturating_sub(glyph_h)) / 2;
    draw_text(
        &mut bmp,
        PAD_X + 4,
        title_y,
        &truncate(&header, header_cols.max(8)),
        if active {
            [120, 210, 160, 255]
        } else {
            [200, 205, 210, 255]
        },
        scale,
    );

    // Collapse (`-` / `+`) and fullscreen (`[]`) controls, right-aligned.
    let fs_x = w.saturating_sub((CTRL_GAP + CTRL_SIZE) as usize);
    let col_x = fs_x.saturating_sub((CTRL_GAP + CTRL_SIZE) as usize);
    let ctrl_y = ((TITLE_BAR_H as usize).saturating_sub(glyph_h)) / 2;
    let ctrl_col = [210, 218, 228, 255];
    let collapse_glyph = if collapsed { "+" } else { "-" };
    let col_text_x = col_x + (CTRL_SIZE as usize).saturating_sub(CHAR_W * scale) / 2;
    let fs_text_x = fs_x + (CTRL_SIZE as usize).saturating_sub(CHAR_W * 2 * scale) / 2;
    draw_text(&mut bmp, col_text_x, ctrl_y, collapse_glyph, ctrl_col, scale);
    draw_text(&mut bmp, fs_text_x, ctrl_y, "[]", ctrl_col, scale);

    if !collapsed {
        // Separator under header
        fill_rect(
            &mut bmp,
            PAD_X,
            TITLE_BAR_H as usize,
            w.saturating_sub(PAD_X * 2),
            1,
            [60, 70, 80, 255],
        );

        let body_top = TITLE_BAR_H as usize + PAD_Y / 2;
        let text_x = PAD_X + STRIPE_W + 4;
        let cols = w.saturating_sub(text_x + PAD_X) / cell_w.max(1);
        let line_h = CELL_H * scale;

        let mut y = body_top;
        let mut drew_any = false;
        let grip_reserve = if solid { RESIZE_GRIP as usize } else { 0 };
        let body_limit = h.saturating_sub(grip_reserve);
        for event in feed.newest_first() {
            let body = card_body_lines(event, cols.max(1));
            let avail = body_limit.saturating_sub(y + CARD_PAD_Y);
            let max_lines = avail / line_h.max(1);
            if max_lines == 0 {
                break;
            }
            let take = max_lines.min(1 + body.len());
            let card_h = CARD_PAD_Y * 2 + take * line_h;
            drew_any = true;
            let plate_w = w.saturating_sub(PAD_X * 2);
            fill_rect(&mut bmp, PAD_X, y, plate_w, card_h, [16, 20, 26, 230]);
            fill_rect(&mut bmp, PAD_X, y, STRIPE_W, card_h, stripe_color(event));
            let mut ly = y + CARD_PAD_Y;
            draw_card_header(&mut bmp, text_x, ly, event, scale);
            ly += line_h;
            let mut drawn = 1usize;
            for line in &body {
                if drawn >= take {
                    break;
                }
                draw_text(&mut bmp, text_x, ly, line, COL_BODY, scale);
                ly += line_h;
                drawn += 1;
            }
            y += card_h;
            fill_rect(
                &mut bmp,
                PAD_X,
                y,
                plate_w,
                1,
                COL_MUTED,
            );
            y += 1 + CARD_GAP;
        }
        if solid && !collapsed {
            let gx = w.saturating_sub(RESIZE_GRIP as usize);
            let gy = h.saturating_sub(RESIZE_GRIP as usize);
            fill_rect(&mut bmp, gx, gy, RESIZE_GRIP as usize, RESIZE_GRIP as usize, [40, 48, 58, 255]);
            draw_text(&mut bmp, gx + 4, gy + 5, "/", COL_MUTED, 1);
        }

        if !drew_any && feed.is_empty() {
            draw_text(
                &mut bmp,
                PAD_X + 4,
                body_top,
                "waiting for mailbox traffic...",
                COL_MUTED,
                scale,
            );
        }
    }

    HudFrame {
        pixels,
        width,
        height,
        active,
        event_count: feed.len(),
    }
}

struct Bitmap<'a> {
    pixels: &'a mut [u8],
    w: usize,
    h: usize,
}

fn draw_card_header(bmp: &mut Bitmap<'_>, x: usize, y: usize, event: &FeedEvent, scale: usize) {
    let gap = CELL_W * scale.max(1);
    let mut cx = x;
    cx = draw_text(bmp, cx, y, &op_label(&event.op), op_color(&event.op), scale);
    cx += gap;
    if event.ok {
        cx = draw_text(bmp, cx, y, "ok", COL_OK, scale);
    } else {
        cx = draw_text(bmp, cx, y, "FAIL", COL_FAIL, scale);
    }
    if let Some(rev) = event.revision {
        cx += gap;
        cx = draw_text(bmp, cx, y, &format!("r{rev}"), COL_MUTED, scale);
    }
    cx += gap;
    let timing = if event.elapsed_ms < 10.0 {
        format!("{:.1}ms", event.elapsed_ms)
    } else {
        format!("{:.0}ms", event.elapsed_ms)
    };
    draw_text(bmp, cx, y, &timing, COL_MUTED, scale);
}

fn card_body_lines(event: &FeedEvent, cols: usize) -> Vec<String> {
    let mut lines = wrap_text(&event.summary, cols);
    if event.hits.is_empty() {
        return lines;
    }
    let extra = event.hits.len().saturating_sub(FEED_HIT_CAP);
    for id in event.hits.iter().take(FEED_HIT_CAP) {
        lines.extend(wrap_text(id, cols));
    }
    if extra > 0 {
        lines.push(format!("+{extra} more"));
    }
    lines
}

/// Word-wrap `s` into lines of at most `cols` characters. Never panics.
/// One-shot status chip (Cycles wait / sample count).
pub fn rasterize_banner(lines: &[&str]) -> (u32, u32, Vec<u8>) {
    let scale = 2usize;
    let line_h = CELL_H * scale;
    let max_chars = lines.iter().map(|s| s.chars().count()).max().unwrap_or(1);
    let w = (PAD_X * 2 + max_chars * CELL_W * scale).max(64);
    let h = (PAD_Y * 2 + lines.len() * line_h + STRIPE_W).max(28);
    let mut pixels = vec![0u8; w * h * 4];
    let mut bmp = Bitmap {
        pixels: &mut pixels,
        w,
        h,
    };
    fill_rect(&mut bmp, 0, 0, w, h, [18, 22, 30, 230]);
    fill_rect(&mut bmp, 0, 0, STRIPE_W, h, COL_APPLY);
    for (i, line) in lines.iter().enumerate() {
        let y = PAD_Y + i * line_h;
        draw_text(&mut bmp, PAD_X + STRIPE_W, y, line, COL_BODY, scale);
    }
    (w as u32, h as u32, pixels)
}

pub fn wrap_text(s: &str, cols: usize) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    if cols == 0 {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in s.split_whitespace() {
        let wlen = word.chars().count();
        if wlen > cols {
            if cur_len > 0 {
                lines.push(std::mem::take(&mut cur));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == cols {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            cur = chunk;
            cur_len = cur.chars().count();
            continue;
        }
        if cur_len == 0 {
            cur.push_str(word);
            cur_len = wlen;
        } else if cur_len + 1 + wlen <= cols {
            cur.push(' ');
            cur.push_str(word);
            cur_len += 1 + wlen;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_len = wlen;
        }
    }
    if cur_len > 0 {
        lines.push(cur);
    }
    lines
}

fn truncate(s: &str, cols: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= cols {
        s.to_string()
    } else if cols <= 3 {
        ".".repeat(cols.min(3))
    } else {
        let mut out: String = chars[..cols - 3].iter().collect();
        out.push_str("...");
        out
    }
}

fn fill_rect(bmp: &mut Bitmap<'_>, x: usize, y: usize, w: usize, h: usize, rgba: [u8; 4]) {
    for row in y..(y + h).min(bmp.h) {
        for col in x..(x + w).min(bmp.w) {
            let i = (row * bmp.w + col) * 4;
            bmp.pixels[i..i + 4].copy_from_slice(&rgba);
        }
    }
}

fn draw_text(
    bmp: &mut Bitmap<'_>,
    x: usize,
    y: usize,
    text: &str,
    rgba: [u8; 4],
    scale: usize,
) -> usize {
    let scale = scale.max(1);
    let mut cx = x;
    let advance = CELL_W * scale;
    for ch in text.chars() {
        draw_char(bmp, cx, y, ch, rgba, scale);
        cx += advance;
        if cx + CHAR_W * scale >= bmp.w {
            break;
        }
    }
    cx
}

fn draw_char(bmp: &mut Bitmap<'_>, x: usize, y: usize, ch: char, rgba: [u8; 4], scale: usize) {
    let scale = scale.max(1);
    let glyph = glyph_for(ch);
    for row in 0..CHAR_H {
        let bits = glyph[row];
        for col in 0..CHAR_W {
            if bits & (1 << (CHAR_W - 1 - col)) != 0 {
                fill_rect(
                    bmp,
                    x + col * scale,
                    y + row * scale,
                    scale,
                    scale,
                    rgba,
                );
            }
        }
    }
}

/// Tiny 5x7 glyphs for printable ASCII (space-tilde). Missing -> box.
fn glyph_for(ch: char) -> [u8; 7] {
    let c = if ch.is_ascii() {
        ch as u8
    } else if ch == '\u{2192}' {
        b'>'
    } else if ch == '\u{2026}' {
        b'.'
    } else {
        b'?'
    };
    match c {
        b' ' => [0, 0, 0, 0, 0, 0, 0],
        b'!' => [0x04, 0x04, 0x04, 0x04, 0x00, 0x04, 0x00],
        b'#' => [0x0A, 0x1F, 0x0A, 0x0A, 0x1F, 0x0A, 0x00],
        b'+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        b',' => [0x00, 0x00, 0x00, 0x00, 0x04, 0x08, 0x00],
        b'-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        b'.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00],
        b'/' => [0x01, 0x02, 0x04, 0x08, 0x10, 0x00, 0x00],
        b'0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x0E, 0x00],
        b'1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x0E, 0x00],
        b'2' => [0x0E, 0x11, 0x01, 0x06, 0x08, 0x1F, 0x00],
        b'3' => [0x1E, 0x01, 0x0E, 0x01, 0x01, 0x1E, 0x00],
        b'4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x00],
        b'5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x1E, 0x00],
        b'6' => [0x06, 0x08, 0x1E, 0x11, 0x11, 0x0E, 0x00],
        b'7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x00],
        b'8' => [0x0E, 0x11, 0x0E, 0x11, 0x11, 0x0E, 0x00],
        b'9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x0C, 0x00],
        b':' => [0x00, 0x04, 0x00, 0x00, 0x04, 0x00, 0x00],
        b';' => [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00],
        b'?' => [0x0E, 0x11, 0x02, 0x04, 0x00, 0x04, 0x00],
        b'A' | b'a' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x00],
        b'B' | b'b' => [0x1E, 0x11, 0x1E, 0x11, 0x11, 0x1E, 0x00],
        b'C' | b'c' => [0x0E, 0x11, 0x10, 0x10, 0x11, 0x0E, 0x00],
        b'D' | b'd' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x1E, 0x00],
        b'E' | b'e' => [0x1F, 0x10, 0x1E, 0x10, 0x10, 0x1F, 0x00],
        b'F' | b'f' => [0x1F, 0x10, 0x1E, 0x10, 0x10, 0x10, 0x00],
        b'G' | b'g' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x0F, 0x00],
        b'H' | b'h' => [0x11, 0x11, 0x1F, 0x11, 0x11, 0x11, 0x00],
        b'I' | b'i' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x0E, 0x00],
        b'J' | b'j' => [0x01, 0x01, 0x01, 0x01, 0x11, 0x0E, 0x00],
        b'K' | b'k' => [0x11, 0x12, 0x1C, 0x12, 0x11, 0x11, 0x00],
        b'L' | b'l' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x1F, 0x00],
        b'M' | b'm' => [0x11, 0x1B, 0x15, 0x11, 0x11, 0x11, 0x00],
        b'N' | b'n' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x00],
        b'O' | b'o' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x0E, 0x00],
        b'P' | b'p' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x00],
        b'Q' | b'q' => [0x0E, 0x11, 0x11, 0x15, 0x12, 0x0D, 0x00],
        b'R' | b'r' => [0x1E, 0x11, 0x11, 0x1E, 0x12, 0x11, 0x00],
        b'S' | b's' => [0x0F, 0x10, 0x0E, 0x01, 0x01, 0x1E, 0x00],
        b'T' | b't' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x00],
        b'U' | b'u' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0E, 0x00],
        b'V' | b'v' => [0x11, 0x11, 0x11, 0x11, 0x0A, 0x04, 0x00],
        b'W' | b'w' => [0x11, 0x11, 0x11, 0x15, 0x1B, 0x11, 0x00],
        b'X' | b'x' => [0x11, 0x0A, 0x04, 0x04, 0x0A, 0x11, 0x00],
        b'Y' | b'y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x00],
        b'Z' | b'z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x1F, 0x00],
        b'_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x1F, 0x00],
        b'[' => [0x0E, 0x08, 0x08, 0x08, 0x08, 0x0E, 0x00],
        b']' => [0x0E, 0x02, 0x02, 0x02, 0x02, 0x0E, 0x00],
        b'>' => [0x00, 0x04, 0x02, 0x1F, 0x02, 0x04, 0x00],
        _ => [0x1F, 0x11, 0x11, 0x11, 0x11, 0x1F, 0x00],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_collapsed_title_bar() {
        let p = HudPanel::new(800.0, 600.0);
        assert_eq!(p.mode, HudMode::Fullscreen);
        assert!(p.collapsed);
        assert_eq!(p.rect(), (0.0, 0.0, 800.0, TITLE_BAR_H));
        assert_eq!(p.pixel_size(), (800, TITLE_BAR_H.round() as u32));
    }

    #[test]
    fn collapse_button_hit_in_fullscreen() {
        let p = HudPanel::new(800.0, 600.0);
        let cy = TITLE_BAR_H * 0.5;
        let collapse_cx = 800.0 - CTRL_GAP - CTRL_SIZE - CTRL_GAP - CTRL_SIZE * 0.5;
        let fs_cx = 800.0 - CTRL_GAP - CTRL_SIZE * 0.5;
        assert_eq!(p.hit(collapse_cx, cy), HudHit::Collapse);
        assert_eq!(p.hit(fs_cx, cy), HudHit::Fullscreen);
        // Left of the controls is still the title bar, not a button.
        assert_eq!(p.hit(collapse_cx - CTRL_SIZE, cy), HudHit::Title);
    }

    #[test]
    fn hit_fullscreen_title_body_outside() {
        let mut p = HudPanel::new(800.0, 600.0);
        p.toggle_collapse();
        assert_eq!(p.hit(20.0, 8.0), HudHit::Title);
        assert_eq!(p.hit(400.0, 300.0), HudHit::Body);
        assert_eq!(p.hit(-1.0, 10.0), HudHit::Outside);
        assert_eq!(p.hit(400.0, 700.0), HudHit::Outside);
        assert_eq!(p.hit(800.0, 10.0), HudHit::Outside);
    }

    #[test]
    fn hit_collapsed_is_title_only() {
        let p = HudPanel::new(800.0, 600.0);
        assert!(p.collapsed);
        assert_eq!(p.rect(), (0.0, 0.0, 800.0, TITLE_BAR_H));
        assert_eq!(p.hit(400.0, TITLE_BAR_H * 0.5), HudHit::Title);
        assert_eq!(p.hit(400.0, TITLE_BAR_H + 8.0), HudHit::Outside);
        assert_eq!(p.hit(400.0, 300.0), HudHit::Outside);
        // Controls still work on the remaining title bar.
        let cy = TITLE_BAR_H * 0.5;
        let collapse_cx = 800.0 - CTRL_GAP - CTRL_SIZE - CTRL_GAP - CTRL_SIZE * 0.5;
        assert_eq!(p.hit(collapse_cx, cy), HudHit::Collapse);
    }

    #[test]
    fn undock_and_restore_fullscreen() {
        let mut p = HudPanel::new(800.0, 600.0);
        p.toggle_collapse();
        p.undock_and_drag(100.0, 10.0);
        assert_eq!(p.mode, HudMode::Floating);
        let (x, y, w, h) = p.rect();
        assert!(x >= 0.0 && y >= 0.0);
        assert!((w - FLOAT_W).abs() < 0.5, "float w={w}");
        assert!((h - FLOAT_H).abs() < 0.5, "float h={h}");
        assert!(w < 800.0 && h < 600.0);
        p.restore_fullscreen();
        assert_eq!(p.mode, HudMode::Fullscreen);
        assert!(!p.collapsed);
        assert_eq!(p.rect(), (0.0, 0.0, 800.0, 600.0));
    }

    #[test]
    fn hit_floating_panel_regions() {
        let mut p = HudPanel::new(800.0, 600.0);
        p.toggle_collapse();
        p.undock_and_drag(100.0, 10.0);
        let (x, y, w, h) = p.rect();
        assert_eq!(p.hit(x - 1.0, y + 4.0), HudHit::Outside);
        assert_eq!(p.hit(x + 16.0, y + 6.0), HudHit::Title);
        assert_eq!(p.hit(x + 16.0, y + TITLE_BAR_H + 12.0), HudHit::Body);
        assert_eq!(p.hit(x + w + 1.0, y + 4.0), HudHit::Outside);
        assert_eq!(p.hit(x + 16.0, y + h + 1.0), HudHit::Outside);
        let cy = y + TITLE_BAR_H * 0.5;
        let collapse_cx = x + w - CTRL_GAP - CTRL_SIZE - CTRL_GAP - CTRL_SIZE * 0.5;
        let fs_cx = x + w - CTRL_GAP - CTRL_SIZE * 0.5;
        assert_eq!(p.hit(collapse_cx, cy), HudHit::Collapse);
        assert_eq!(p.hit(fs_cx, cy), HudHit::Fullscreen);
    }

    fn sample_query_event() -> FeedEvent {
        FeedEvent {
            op: "query".into(),
            ok: true,
            elapsed_ms: 1.2,
            revision: None,
            summary: "assembly_of box-1".into(),
            hits: vec![
                "box-1".into(),
                "box-1-hat-brim".into(),
                "box-1-eye-l".into(),
                "box-1-eye-r".into(),
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
            ],
            at: Instant::now(),
        }
    }

    #[test]
    fn wrap_text_does_not_panic() {
        let _ = wrap_text("", 0);
        let _ = wrap_text("", 8);
        let _ = wrap_text("hello", 0);
        let _ = wrap_text("hello world from the feed", 1);
        let _ = wrap_text(&"x".repeat(200), 3);
        let _ = wrap_text("assembly_of box-1", 64);
        let lines = wrap_text("yellow assembly step right", 10);
        assert!(lines.len() >= 2);
        assert!(lines.iter().all(|l| l.chars().count() <= 10));
    }

    #[test]
    fn op_color_mapping() {
        assert_eq!(op_color("apply"), COL_APPLY);
        assert_eq!(op_color("query"), COL_QUERY);
        assert_eq!(op_color("status"), COL_STATUS);
        assert_eq!(op_color("inspect"), COL_INSPECT);
        assert_eq!(op_color("ping"), COL_MUTED);
        let fail_event = FeedEvent {
            op: "apply".into(),
            ok: false,
            elapsed_ms: 1.0,
            revision: None,
            summary: "nope".into(),
            hits: Vec::new(),
            at: Instant::now(),
        };
        assert_eq!(stripe_color(&fail_event), COL_FAIL);
        let ok_event = FeedEvent {
            op: "query".into(),
            ok: true,
            elapsed_ms: 1.0,
            revision: None,
            summary: "on_screen".into(),
            hits: Vec::new(),
            at: Instant::now(),
        };
        assert_eq!(stripe_color(&ok_event), COL_QUERY);
    }

    #[test]
    fn glyph_scale_doubles_when_tall() {
        assert_eq!(glyph_scale(200), 1);
        assert_eq!(glyph_scale(360), 1);
        assert_eq!(glyph_scale(361), 2);
        assert_eq!(glyph_scale(600), 2);
        assert_eq!(glyph_scale(28), 1);
    }

    #[test]
    fn assembly_card_lists_ids_without_comma_jam() {
        let event = sample_query_event();
        let lines = card_body_lines(&event, 40);
        assert_eq!(lines[0], "assembly_of box-1");
        assert!(lines.iter().any(|l| l == "box-1"));
        assert!(lines.iter().any(|l| l == "box-1-hat-brim"));
        assert!(lines.iter().any(|l| l == "box-1-eye-l"));
        for line in &lines {
            assert!(
                !line.contains("box-1,box-1-hat"),
                "comma-jammed ids: {line}"
            );
            assert_ne!(
                line,
                "assembly_of box-1 -> a,b,c,d...(8)"
            );
        }
    }

    #[test]
    fn hit_cap_adds_more_suffix() {
        let event = FeedEvent {
            op: "query".into(),
            ok: true,
            elapsed_ms: 1.0,
            revision: None,
            summary: "on_screen".into(),
            hits: (0..15).map(|i| format!("id-{i}")).collect(),
            at: Instant::now(),
        };
        let lines = card_body_lines(&event, 20);
        assert!(lines.iter().any(|l| l == "+3 more"), "{lines:?}");
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("id-")).count(),
            FEED_HIT_CAP
        );
    }

    #[test]
    fn rasterize_cards_do_not_panic() {
        let mut feed = MailboxFeed::new();
        feed.push(sample_query_event());
        feed.push(FeedEvent {
            op: "apply".into(),
            ok: true,
            elapsed_ms: 12.0,
            revision: Some(16),
            summary: "yellow assembly step right".into(),
            hits: Vec::new(),
            at: Instant::now(),
        });
        feed.push(FeedEvent {
            op: "status".into(),
            ok: true,
            elapsed_ms: 0.4,
            revision: Some(16),
            summary: "rev=16 entities=40".into(),
            hits: Vec::new(),
            at: Instant::now(),
        });
        let now = Instant::now();
        let tiny = rasterize_feed(&feed, now, 80, 50, true);
        assert_eq!(tiny.width, 80);
        let tall = rasterize_feed(&feed, now, 800, 600, false);
        assert_eq!(tall.height, 600);
        assert_eq!(glyph_scale(600), 2);
        // Scaled glyph 'Q' of QUERY (amber) should occupy a 2x2 block.
        let mut found_2x = false;
        let stride = 800usize;
        'outer: for y in 0..(600 - 1) {
            for x in (PAD_X + STRIPE_W + 4)..(800 - 1) {
                let i = (y * stride + x) * 4;
                let px = &tall.pixels[i..i + 4];
                if px == COL_QUERY {
                    let right = &tall.pixels[i + 4..i + 8];
                    let down = &tall.pixels[((y + 1) * stride + x) * 4..][..4];
                    if right == COL_QUERY && down == COL_QUERY {
                        found_2x = true;
                        break 'outer;
                    }
                }
            }
        }
        assert!(found_2x, "expected 2x2 amber query pixels on a tall panel");
    }
    #[test]
    fn floating_default_fits_a_query_card() {
        let mut p = HudPanel::new(800.0, 600.0);
        p.toggle_collapse();
        p.undock_and_drag(100.0, 10.0);
        let (_, _, w, h) = p.rect();
        assert!(h >= 300.0, "float h={h}");
        let mut feed = MailboxFeed::new();
        feed.push(sample_query_event());
        let frame = rasterize_feed(&feed, Instant::now(), w as u32, h as u32, true);
        // Body must contain query-amber pixels, not just a title bar.
        let mut found = false;
        for px in frame.pixels.chunks(4) {
            if px == COL_QUERY {
                found = true;
                break;
            }
        }
        assert!(found, "floating panel should show the query card");
    }

    #[test]
    fn fullscreen_backdrop_is_clear() {
        let feed = MailboxFeed::new();
        let frame = rasterize_feed(&feed, Instant::now(), 200, 200, false);
        // A pixel in the empty body (below title) stays fully transparent.
        let x = 40usize;
        let y = TITLE_BAR_H as usize + 20;
        let i = (y * 200 + x) * 4;
        assert_eq!(&frame.pixels[i..i + 4], [0, 0, 0, 0]);
        // Title bar stays opaque enough to read.
        let ti = (8 * 200 + 8) * 4;
        assert!(frame.pixels[ti + 3] > 200);
    }

    #[test]
    fn resize_grip_changes_float_size() {
        let mut p = HudPanel::new(800.0, 600.0);
        p.toggle_collapse();
        p.undock_and_drag(40.0, 20.0);
        let (x, y, w, h) = p.rect();
        assert_eq!(p.hit(x + w - 4.0, y + h - 4.0), HudHit::Resize);
        p.resize_to(x + 700.0, y + 500.0);
        let (_, _, w2, h2) = p.rect();
        assert!(w2 > w && h2 > h, "w {w}->{w2} h {h}->{h2}");
    }

}
