// UI quality spec helpers: single source of testable truth for visual rules.
// Every table here mirrors `desktop/ui/hud.slint` (noted per item). If the
// .slint changes, update the mirror here too — the test suite pins both.
//
// This module is shared by two integration-test binaries, each of which uses a
// different subset, so per-binary dead-code analysis is not meaningful here.
#![allow(dead_code)]

/// Parse `#rrggbb` into linear-channel 0..1 sRGB.
pub fn parse_hex(hex: &str) -> Option<(f32, f32, f32)> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some((
        ((v >> 16) & 0xFF) as f32 / 255.0,
        ((v >> 8) & 0xFF) as f32 / 255.0,
        (v & 0xFF) as f32 / 255.0,
    ))
}

fn linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.x relative luminance for `#rrggbb`.
pub fn luminance(hex: &str) -> Option<f32> {
    let (r, g, b) = parse_hex(hex)?;
    Some(0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b))
}

/// WCAG 2.x contrast ratio (1.0–21.0). Returns None on bad input.
pub fn contrast_ratio(fg: &str, bg: &str) -> Option<f32> {
    let (l1, l2) = (luminance(fg)?, luminance(bg)?);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    Some((hi + 0.05) / (lo + 0.05))
}

// --- Palette mirror (hud.slint) ------------------------------------------------
pub const INK_PRIMARY: &str = "#fafafa";
pub const INK_BODY: &str = "#fafafa";
pub const INK_SECONDARY: &str = "#a1a1aa";
pub const INK_MUTED: &str = "#71717a";
pub const ACCENT: &str = "#38bdf8";
pub const SUCCESS: &str = "#4ade80";
pub const CARD_BG: &str = "#121215";
pub const CHROME_BG: &str = "#18181b";
pub const SAVE_BTN_BG: &str = "#fafafa";
pub const SAVE_BTN_INK: &str = "#09090b";

/// `stage-bg()` mirror: bg-mode -> canvas color.
pub const STAGE_BGS: [(i32, &str); 4] = [
    (0, "#0f1013"), // IDE
    (1, "#1e2025"), // Slate
    (2, "#000000"), // OLED
    (3, "#ffffff"), // Light
];

pub fn stage_bg(mode: i32) -> &'static str {
    match mode {
        1 => "#1e2025",
        2 => "#000000",
        3 => "#ffffff",
        _ => "#0f1013",
    }
}

// --- Pill width mirror (`pill-width()` in hud.slint:46-61) ---------------------
// Rows = active-state 0..=4, cols = option A/B/C (0/1/2).
pub const PILL_WIDTHS: [[f32; 3]; 5] = [
    [48.0, 44.0, 56.0],   // Idle: Micro Mic / Standby LED / Edge Accent
    [110.0, 84.0, 100.0], // Listening
    [72.0, 80.0, 74.0],   // Transcribe
    [60.0, 68.0, 64.0],   // Success
    [60.0, 60.0, 64.0],   // Error (falls through to default branch)
];

/// `active-option()` mirror: which opt slot drives the current state.
pub fn active_option_index(state: i32, opts: [i32; 5]) -> i32 {
    match state {
        0 => opts[0],
        1 => opts[1],
        2 => opts[2],
        3 => opts[3],
        _ => opts[4],
    }
}

// --- Validated setters (wiring in main.rs:358-412 should adopt these) ----------
pub fn clamp_state(s: i32) -> i32 {
    s.clamp(0, 4)
}

pub fn clamp_option(o: i32) -> i32 {
    o.clamp(0, 2)
}

pub fn clamp_opacity(op: f32) -> f32 {
    if !op.is_finite() {
        return 0.85;
    }
    op.clamp(0.0, 1.0)
}

pub fn clamp_bg_mode(bg: i32) -> i32 {
    bg.clamp(0, 3)
}

pub fn clamp_tab(tab: i32) -> i32 {
    tab.clamp(0, 2)
}

/// Robust `M:SS` timer text. (Wiring uses `format!("0:{:02}")`, which breaks
/// past 59s; adopt this instead.)
pub fn format_timer_text(total_secs: u32) -> String {
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}

// --- Sharpness -----------------------------------------------------------------
/// True when a logical-px length lands exactly on a physical pixel grid.
pub fn is_crisp_at_scale(logical_px: f32, scale: f32) -> bool {
    let phys = logical_px * scale;
    (phys - phys.round()).abs() < 1e-4
}

/// Minimum pointer target edge, px (WCAG 2.2 AA §2.5.8).
pub const MIN_TARGET_PX: f32 = 24.0;
/// Copy button geometry in hud.slint:1180-1181.
pub const COPY_BUTTON_W: f32 = 72.0;
pub const COPY_BUTTON_H: f32 = 24.0;
/// Body text readability floor, px (transcript is 13.5px).
pub const MIN_BODY_FONT_PX: f32 = 10.5;
