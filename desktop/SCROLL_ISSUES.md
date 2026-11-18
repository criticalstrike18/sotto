# Scroll Issues — Studio History Vault (complete audit)

> **Status (2026-10-25): historical audit.** The dead-code prune removed the
> engine surface this pass treated as optional: the window/edge-refill helpers
> (`window_for`, `window_needs_edge_refill`), the non-production `on_wheel`
> path, `velocity`/`direction` accessors, lock (`stop`/`start`), wheel
> multiplier, `is_at_top`/`is_at_bottom`, `can_scroll`, `reset`, and the
> `LenisScroller` alias. Production wiring only uses the remaining API.
> Test counts and line references below describe the fix pass, not the
> current tree.

Scope: `desktop/src/smooth_scroll.rs` (engine), `desktop/src/main.rs:48-126`
(WndProc hook), `desktop/src/main.rs:363-527` (timer + virtualization wiring),
`desktop/ui/hud.slint:538-545,1049-1075,1222-1274` (list view).

Reference: Lenis (Darkroom Engineering) — rAF-driven, GPU-composited transform,
measured content, windowed rendering.

## Part A — Already fixed (verified in current code)

- Engine: `DEFAULT_DECAY = 8.0` (`smooth_scroll.rs:5`), `tick()` owns timekeeping
  (`on_wheel` no longer touches `last_tick`, `smooth_scroll.rs:97-99`), velocity /
  direction / progress / limit / edge helpers, `stop()/start()`,
  `wheel_multiplier`, `scroll_to/scroll_by/page/home/end`, jitter deadband,
  resize re-arm in `set_dimensions` (`smooth_scroll.rs:62-66`).
- Wiring: 16ms timer (`main.rs:380`), DPI-aware visible height (`main.rs:399-404`),
  per-item estimated heights + prefix sums (`main.rs:412-430`), physical-pixel
  snapping (`main.rs:449-456`), one-way `content-y` binding (`hud.slint:1056`),
  virtualized window + spacers (`main.rs:458-494`), throttled aux writes
  (`main.rs:496-510`), settle sync frame (`main.rs:511-521`), tab-gated and
  modifier-aware wheel hook (`main.rs:63-75`), keyboard nav (`main.rs:76-115`).
- View: custom scrollbar + vignettes driven by progress flags
  (`hud.slint:1241-1274`).
- R1/R16: bottom spacer moved INSIDE the Flickable, both spacers unconditional
  (`hud.slint:1063-1066,1237-1240`); container kept alive via `visible:` instead
  of conditional (`hud.slint:1057`, also fixes R14).
- R2/R3: window refill runs on dim-change even at rest (`main.rs:needs_refill`
  path, independent of `tick()`); empty-list reset clears the window model.
- R4/R5/R6/R9: shared `estimate_card_height()` + `chars_per_line_for()`
  (`smooth_scroll.rs`), `ROW_SPACING = 12.0`, `CHAR_ADVANCE_PX = 8.1`,
  double-width CJK/emoji counting; dim pass and insert anchor use the same
  function so they cannot diverge.
- R7: refill hysteresis + 48ms rate limit + edge-proximity forced refill
  (`window_for`, `window_needs_edge_refill` — both unit-tested).
- R8: prepend anchor uses the new card's real estimated height and writes the
  viewport immediately (no rest-mismatch jump on next input).
- R10 (partial): WM_GESTURE pan feeds the engine target (`main.rs`
  `handle_touch_pan_gesture` + `gesture_pan_delta`); mapping unit-tested but
  NOT yet verified on physical touch hardware.
- R11: tilt absorbed safely (still not mapped — no horizontal content exists).
- R13: `STUDIO_HISTORY_GEN` content counter in `dim_key` (bumped on
  insert/clear/init).
- R15: timer gated on `window().is_visible()` + History tab.

## Part B — Remaining issues

> Status pass: R1–R9, R13–R16 are FIXED (see Part A). What is genuinely left
> is below. Nothing else in the scroll/content path is known-broken.

### Leftover MEDIUM — correctness, input, power

**R10 (remainder). Touch flick/inertia + scrollbar drag.** Pan packets now
drive the target, but release has no fling inertia beyond the exponential
tail, and the scrollbar thumb is still display-only. Needs hardware verify +
a `TouchArea` on the thumb mapping drag-y → `scroll_to()`.
**R12. Keys swallowed globally on History tab** (`main.rs:112-114`) — unchanged;
needs a focus model before it can be scoped safely. No action taken.
**R11. Tilt absorbed, not mapped** — no horizontal content exists, so there is
nothing to map it to. Closed as won't-fix unless horizontal content appears.

### Fresh notes from this fix pass (no action needed unless noted)

- Residual estimator error is now ~2px at the very bottom (trailing-pitch
  convention), down from ~200px progressive drift. Accept.
- `shift_anchor` deliberately does not set `is_animating`; the insert path now
  writes the viewport synchronously so engine and display never disagree.
- Disk pressure (D: full) blocked verification linking during this pass;
  `target/release` cleaned (3.4GB). Consider moving `CARGO_TARGET_DIR` or
  pruning `target/debug/deps` before the next full rebuild.
- Untouched by design: Part E/F endgame items (animate-driven migration,
  ListView probe) — still the recommended next leap, not a bug fix.

### MINOR — dead code / nits

- `velocity` computed, never consumed; `stop()/start()/set_wheel_multiplier`
  never called from wiring — decide if they're API surface or delete.
- `tick()` emits one redundant setter on the snap frame (value == last).
- Always 120px/notch ignores the Windows "lines per scroll" setting.
- Icon extraction is cache-warmed off-UI (`main.rs:986`) — keep it that way;
  a cache miss inside the UI callback (`main.rs:1004`) would hitch scrolling.

## Part C — Test coverage map

- `tests/scroll_smoothness.rs` (20): engine curve — monotonic, bounded steps,
  settle budget, clamps, bursts, reversal, stall clamp.
- `tests/scroll_wiring.rs` (25): frame-rate independence, zero work at rest,
  height helpers, shrink re-clamp, one-write-per-tick, large-list budget,
  anchor shift, hysteresis gating, plus NEW: `estimate_card_height` table,
  `chars_per_line_for`, `window_for` semantics, `window_needs_edge_refill`
  guard, `gesture_pan_delta` conventions.
- Remaining gaps: physical touch-hardware verify (R10), ListView probe result
  (Part E2), 120Hz re-verify (Part F5).

## Part D — Next steps (all R-items fixed; these are enhancements, not bugs)

1. Verify touch pan on physical hardware (R10 remainder).
2. Draggable scrollbar thumb → `scroll_to()` (F1).
3. ListView probe + animate-driven migration (Part E) — the next leap.

## Part E — Endgame: animate-driven scrolling + ListView (2026 standard)

Source finding: Slint 1.18's own `Flickable`
(`i-slint-core/.../items/flickable.rs`) already animates `content_x/_y` with a
built-in physics simulation (`set_physic_animation_value`), driven by Slint's
animation clock on the render loop (vsync-aligned, follows 60Hz/120Hz+). The
Rust 60Hz timer + per-frame setter fights that system instead of using it:
each tick forces a UI-thread evaluation unsynced to vsync. The 2026-standard
architecture is **animate-driven**: inputs write only a *target*; Slint glides
`content-y` to it. Zero timer wakeups, zero per-frame Rust work.

**E1. Migrate timer → `animate` (works with current windowing).**
`.slint`: `animate content-y { duration: 280ms; easing: ease-out; }` on
`history-flick`. Rust: delete the 16ms loop; WndProc wheel / keyboard /
gesture only accumulate the engine target, snap it once, write
`history-viewport-y` once per input event; refill the window on target change.
Bursts retarget mid-flight from the current animated value (same chase as the
Lenis curve). Keep pixel-snapping — apply it to the TARGET, not every frame.

**E2. `ListView` evaluation (kills R4–R6 + touch in one move).**
`ListView` (std-widgets) virtualizes natively — only visible items exist, with
real measured layout (no estimator, no drift) — plus native touch-drag and
flick physics. Wheel interception stays regardless (native wheel step is the
original jerky jump). Probe first (5 min compile test):
```slint
import { ListView } from "std-widgets.slint";
export component Probe inherits Window {
    in-out property <length> pos;
    ListView {
        viewport-y: root.pos;
        for i in 200 : Text { text: "row " + i; }
    }
}
```
- Compiles + drives → migrate to `ListView` + `animate viewport-y` +
  intercepted wheel → target. Keep scrollbar/progress UI unchanged.
- Doesn't → keep manual windowing + E1 (keeps ~95% of the benefit).

**E3. Traps (from Slint's source — will silently break the migration).**
- Never `animate` a property also written at 60Hz from Rust: every write
  restarts the animation (worse stutter than today). Animate-driven means
  writes happen only on input events.
- Never set `interactive: true` while `content-y` is bound: Slint's native
  drag path calls `remove_binding()` on first touch-drag — one finger-drag
  permanently severs the engine binding with no error. Touch input must go
  through the same target property (WM_GESTURE/pan in WndProc → engine target
  → animated `content-y`). One driver, all inputs.

## Part F — 2026 polish checklist (after motion is solved)

- **F1. Draggable scrollbar thumb.** `TouchArea` on the existing thumb
  (`hud.slint:1270-1274`) mapping drag-y → `scroll_to()`. Cheap, huge
  perceived win; fixes display-only scrollbar (R10 partial).
- **F2. Overscroll language.** Small rubber-band at edges for wheel-target
  (clamped overshoot + settle) mirroring Slint's native touch physics.
- **F3. Insert animation.** Fade/slide for prepended dictations — masks R8
  shift, reads as modern/alive.
- **F4. Pin the renderer.** Log active Slint backend at startup; software
  fallback redefines the frame floor — verify FemtoVG/Skia on target machines.
- **F5. 120Hz+.** Free with E1 (render clock follows display); impossible with
  a 16ms timer. Re-verify feel on 120Hz panels after migrating.
- **F6. Measure once.** `SLINT_DEBUG_PERFORMANCE=1` overlays frame stats;
  gate "done" on p95 frame time < 8ms with 200 items.
- Test gap update: E1 needs a target-only write test (assert ≤1 viewport
  write per *input event*, none per frame); E2 probe result decides whether
  estimator-drift tests (R4–R6) become obsolete.
