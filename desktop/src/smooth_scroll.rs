use std::cell::RefCell;
use std::time::Instant;

/// Default decay factor for Lenis-like smooth glide (~800–1200ms settle time).
pub const DEFAULT_DECAY: f32 = 14.0;

/// Deadband threshold in pixels to reject trackpad micro-jitter / palm noise.
pub const JITTER_DEADBAND_PX: f32 = 0.25;

/// High-performance inertial smooth scroll physics engine.
/// Uses the Lenis exponential-decay formula for wheel and keyboard scrolling.
pub struct SmoothScrollEngine {
    /// Desired target scroll position in pixels (<= 0.0)
    target_y: f32,
    /// Current animated scroll position in pixels (<= 0.0)
    current_y: f32,
    /// Total scrollable content height
    content_height: f32,
    /// Visible container viewport height
    visible_height: f32,
    /// Whether the animation loop is currently in motion
    is_animating: bool,
    /// Timestamp of last tick for delta time calculation
    last_tick: Option<Instant>,
    /// Last wheel event timestamp for velocity momentum acceleration
    last_wheel_time: Option<Instant>,
    /// Accumulated velocity multiplier for high-speed flicks
    wheel_momentum: f32,
}

impl SmoothScrollEngine {
    pub fn new() -> Self {
        Self {
            target_y: 0.0,
            current_y: 0.0,
            content_height: 0.0,
            visible_height: 600.0,
            is_animating: false,
            last_tick: None,
            last_wheel_time: None,
            wheel_momentum: 1.0,
        }
    }

    pub fn set_dimensions(&mut self, content_h: f32, visible_h: f32) {
        if content_h > 0.0 {
            self.content_height = content_h;
        }
        if visible_h > 0.0 {
            self.visible_height = visible_h;
        }
        self.clamp_target();
        // If a resize/content change left the current position out of range
        // (e.g. items deleted while scrolled to the bottom), re-arm the
        // animation so the view glides back instead of sticking out of bounds.
        let max_s = self.max_scroll();
        if self.current_y > 0.0 || self.current_y < -max_s {
            self.is_animating = true;
            self.last_tick = None;
        }
    }

    fn max_scroll(&self) -> f32 {
        (self.content_height - self.visible_height).max(0.0)
    }

    fn clamp_target(&mut self) {
        let max_s = self.max_scroll();
        // y is 0.0 at top, -max_s at bottom
        if self.target_y > 0.0 {
            self.target_y = 0.0;
        } else if self.target_y < -max_s {
            self.target_y = -max_s;
        }
    }

    /// Handles a raw Windows WM_MOUSEWHEEL delta (typically +/- 120 per notch)
    /// Handles Windows WM_MOUSEWHEEL with dynamic momentum acceleration for rapid flicks.
    /// Single notches (>140ms apart) move crisp 120px; rapid continuous spinning accumulates
    /// momentum up to 3.8x, enabling effortless high-speed gliding across dozens of cards.
    pub fn on_wheel_accelerated(&mut self, delta: f32) {
        let now = Instant::now();
        let dt = self.last_wheel_time.map(|t| (now - t).as_secs_f32()).unwrap_or(1.0);
        self.last_wheel_time = Some(now);
        self.on_wheel_accelerated_dt(delta, dt);
    }

    pub fn on_wheel_accelerated_dt(&mut self, delta: f32, dt: f32) {
        if delta.abs() < JITTER_DEADBAND_PX {
            return;
        }

        // Pure continuous momentum function based on instantaneous wheel frequency (f = 1/dt).
        // Zero discrete thresholds, zero checkpoints, zero stall-then-surge cliffs.
        if dt > 0.0001 && dt < 0.25 {
            let freq = (1.0 / dt).clamp(0.0, 30.0);
            if freq > 4.0 {
                let boost = ((freq - 4.0) / 10.0).clamp(0.0, 2.0);
                self.wheel_momentum = (1.0 + boost).min(3.0);
            } else {
                self.wheel_momentum = 1.0;
            }
        } else {
            self.wheel_momentum = 1.0;
        }

        // 1 wheel notch (120 units) = 120px of travel before momentum, matching
        // the macOS/Chrome web scroll distance.
        let pixels = delta * self.wheel_momentum;
        self.target_y += pixels;
        self.clamp_target();
        self.is_animating = true;
    }

    /// Smoothly scrolls by a relative pixel delta (e.g. keyboard navigation).
    pub fn scroll_by(&mut self, delta_y: f32) {
        self.target_y += delta_y;
        self.clamp_target();
        self.is_animating = true;
    }

    /// Immediately shifts scroll position without triggering animation (used for anchor scrolling, Fix R8).
    pub fn shift_anchor(&mut self, delta_y: f32) {
        self.target_y += delta_y;
        self.current_y += delta_y;
        self.clamp_target();
        let max_s = self.max_scroll();
        if self.current_y > 0.0 {
            self.current_y = 0.0;
        } else if self.current_y < -max_s {
            self.current_y = -max_s;
        }
    }

    /// Immediately jumps to an absolute position without animation lag (used for 1:1 scrollbar thumb drag).
    pub fn jump_to(&mut self, target_y: f32) {
        self.target_y = target_y;
        self.clamp_target();
        self.current_y = self.target_y;
        self.is_animating = false;
        self.last_tick = None;
    }

    /// Smoothly glides to an absolute target position (matches Lenis scrollTo()).
    pub fn scroll_to(&mut self, target_y: f32) {
        self.target_y = target_y;
        self.clamp_target();
        self.is_animating = true;
    }

    /// Smoothly glides to the top of the viewport (0.0).
    pub fn scroll_to_top(&mut self) {
        self.scroll_to(0.0);
    }

    /// Smoothly glides to the bottom of the viewport (-max_scroll).
    pub fn scroll_to_bottom(&mut self) {
        let max_s = self.max_scroll();
        self.scroll_to(-max_s);
    }

    /// Smoothly scrolls down by one page (~80% of visible viewport).
    pub fn scroll_page_down(&mut self) {
        let page_delta = self.visible_height * 0.8;
        self.scroll_by(-page_delta);
    }

    /// Smoothly scrolls up by one page (~80% of visible viewport).
    pub fn scroll_page_up(&mut self) {
        let page_delta = self.visible_height * 0.8;
        self.scroll_by(page_delta);
    }

    /// Ticks the physics simulation. Returns Some(new_y) if updated, or None if settled.
    pub fn tick(&mut self) -> Option<f32> {
        let now = Instant::now();
        let dt = match self.last_tick {
            Some(prev) => (now - prev).as_secs_f32().min(0.05),
            None => 0.016,
        };
        self.last_tick = Some(now);
        self.tick_dt(dt)
    }

    pub fn tick_dt(&mut self, dt: f32) -> Option<f32> {
        if !self.is_animating {
            return None;
        }

        let diff = self.target_y - self.current_y;

        if diff.abs() < 0.08 {
            // Snapped to rest (omit redundant setter if value already matched)
            let changed = (self.current_y - self.target_y).abs() > 0.001;
            self.current_y = self.target_y;
            self.is_animating = false;
            self.last_tick = None;
            if changed {
                Some(self.current_y)
            } else {
                None
            }
        } else {
            // Exponential decay / spring lerp (Lenis formula: 1.0 - exp(-decay * dt))
            let lerp_factor = 1.0 - (-DEFAULT_DECAY * dt).exp();
            self.current_y += diff * lerp_factor;
            Some(self.current_y)
        }
    }

    pub fn current_y(&self) -> f32 {
        self.current_y
    }

    pub fn target_y(&self) -> f32 {
        self.target_y
    }

    pub fn is_animating(&self) -> bool {
        self.is_animating
    }

    /// Normalized scroll progress from 0.0 (top) to 1.0 (bottom) (matches lenis.progress).
    pub fn progress(&self) -> f32 {
        let max_s = self.max_scroll();
        if max_s <= 0.0 {
            0.0
        } else {
            (self.current_y.abs() / max_s).clamp(0.0, 1.0)
        }
    }

    /// Maximum scrollable distance in pixels (matches lenis.limit).
    pub fn max_scroll_limit(&self) -> f32 {
        self.max_scroll()
    }
}

/// Recommended frame interval for the scroll driver loop.
/// Lenis tracks the display vsync (~60Hz / 16.6ms). The wiring in
/// `main.rs` previously polled every 8ms (~125Hz) on the Slint UI thread,
/// which oversampled, burned CPU, and jittered under layout load.
pub const RECOMMENDED_FRAME_INTERVAL_MS: u64 = 16;

/// Standard row pitch spacing in pixels (8px layout spacing, Fixes R4 & R9)
pub const ROW_SPACING: f32 = 8.0;

/// Bottom overscroll padding in pixels to ensure the final dictation card and its borders/shadows
/// are completely visible with comfortable breathing room above the viewport edge across all window resizes.
pub const LIST_BOTTOM_PADDING: f32 = 16.0;

/// Chrome reserved above the history list: title row + tab bar + stats bar +
/// gaps, measured from an offscreen render (list starts 132px below the window
/// top at any scale; logical px are DPI-independent). The old 156px value
/// overstated chrome by ~26px, parking max scroll that far past the content
/// and leaving a permanent end-of-list void. Keep in sync with the
/// `history-viewport-height` formula in hud.slint.
pub const CHROME_RESERVED_PX: f32 = 132.0;

/// Extra end-of-list overscroll in pixels. The estimator is calibrated to err
/// on the high side (observed range 0..+17 over 50 real records at 5 widths;
/// never negative), so a small slack suffices as cutoff insurance while
/// keeping the resting end gap at the 48px design standard (16 pad + 16 here).
pub const LIST_OVERSCROLL_PX: f32 = 16.0;

/// Mean character advance in pixels for 14px Inter transcript text.
/// Validated headless: wrap counts from cpl match laid-out line counts
/// exactly, so the advance is correct (not merely conservative).
pub const CHAR_ADVANCE_PX: f32 = 6.4;

/// Base height of a card: 24px vertical padding (Theme.space-12 * 2) + 26px header row
/// (defined by the 26px copy button) + 8px header/body spacing (Theme.space-8) + one 19px text line.
/// Calibrated against laid-out cards: single-line cards measure exactly 77px on the standardized grid.
pub const CARD_BASE_HEIGHT: f32 = 77.0;

/// Line height in pixels for 14px DM Sans transcript text. Calibrated against layout:
/// a 3-line card measures exactly base + 2 lines (113 = 77 + 2*18).
pub const LINE_HEIGHT_PX: f32 = 18.0;

/// Minimum visible history-list height in logical pixels.
pub const MIN_VISIBLE_PX: f32 = 100.0;

/// Correct content-height computation: sum of measured card heights plus
/// inter-card spacing. Replaces the old fixed-height estimate in wiring.
pub fn compute_content_height(item_heights: &[f32], spacing: f32) -> f32 {
    if item_heights.is_empty() {
        return 0.0;
    }
    let gaps = (item_heights.len() - 1) as f32 * spacing.max(0.0);
    item_heights.iter().sum::<f32>() + gaps
}

/// Correct visible-height computation from the *logical* window height
/// (physical px divided by scale factor) minus reserved chrome.
pub fn compute_visible_height(window_logical_h: f32, chrome_reserved: f32, min_h: f32) -> f32 {
    (window_logical_h - chrome_reserved).max(min_h)
}

/// Wide-character threshold: codepoints above this count double width (CJK/emoji, Fix R6).
pub const WIDE_CHAR_THRESHOLD: u32 = 0x2E80;

/// Text column count of a line, counting wide chars double.
pub fn line_columns(line: &str) -> usize {
    line.chars()
        .map(|c| if (c as u32) > WIDE_CHAR_THRESHOLD { 2 } else { 1 })
        .sum()
}

/// Estimated card height for a transcript given chars-per-line (Fixes R4–R6, R8).
/// Single source of truth shared by the dim pass and the insert anchor, so the
/// anchor shift exactly matches the height the dim pass will compute.
pub fn estimate_card_height(transcript: &str, chars_per_line: usize) -> f32 {
    let cpl = chars_per_line.max(1);
    let lines = transcript
        .lines()
        .map(|line| {
            let cols = line_columns(line).max(1);
            let cpl_lines = ((cols + cpl - 1) / cpl).max(1);
            if line.contains(' ') {
                let mut word_lines = 1;
                let mut current_col = 0;
                for word in line.split_whitespace() {
                    let w_cols = line_columns(word);
                    if current_col == 0 {
                        current_col = w_cols;
                    } else if current_col + 1 + w_cols <= cpl {
                        current_col += 1 + w_cols;
                    } else {
                        word_lines += 1;
                        current_col = w_cols;
                    }
                    while current_col > cpl {
                        word_lines += 1;
                        current_col = current_col.saturating_sub(cpl);
                    }
                }
                cpl_lines.max(word_lines)
            } else {
                cpl_lines
            }
        })
        .sum::<usize>()
        .max(1);
    CARD_BASE_HEIGHT + ((lines.saturating_sub(1)) as f32 * LINE_HEIGHT_PX)
}

/// chars-per-line for a window of given physical width and scale factor.
/// Mirrors the dim-pass inset chain: 60px window chrome, then the card text
/// inset (32px card padding + 12px list padding + 22px scrollbar gutter).
pub fn chars_per_line_for(window_phys_w: u32, scale: f32) -> usize {
    let scale = scale.max(1.0);
    let card_w = (window_phys_w as f32 / scale - 60.0).max(200.0);
    let text_w = (card_w - 63.0).max(160.0);
    ((text_w / CHAR_ADVANCE_PX) as usize).max(15)
}

/// Touch-pan bookkeeping: converts cumulative GID_PAN totals into an incremental
/// scroll delta. `is_begin` resets the reference (no motion on the first packet).
/// Returns (dx, dy) in pixels; positive dy = finger moved down = scroll up.
pub fn gesture_pan_delta(
    new_total: (i32, i32),
    last_total: &mut (i32, i32),
    is_begin: bool,
) -> (f32, f32) {
    if is_begin {
        *last_total = new_total;
        return (0.0, 0.0);
    }
    let dx = (new_total.0 - last_total.0) as f32;
    let dy = (new_total.1 - last_total.1) as f32;
    *last_total = new_total;
    (dx, dy)
}

thread_local! {
    pub static STUDIO_SCROLLER: RefCell<SmoothScrollEngine> = RefCell::new(SmoothScrollEngine::new());
    pub static PROVIDERS_SCROLLER: RefCell<SmoothScrollEngine> = RefCell::new(SmoothScrollEngine::new());
    pub static CUSTOM_SCROLLER: RefCell<SmoothScrollEngine> = RefCell::new(SmoothScrollEngine::new());
}
