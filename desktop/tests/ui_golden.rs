// Golden snapshot suite: pixel regression testing on the software renderer.
// One StudioWindow, five sequential snapshots (winit-software without an
// event loop supports a single shown window per process): dictations,
// dictations-bottom, providers, customization, narrow viewport.
//
// Note: On macOS, `winit::EventLoop` must execute on thread #1 (Cocoa main thread),
// whereas `cargo test` runs tests on worker threads; CI covers macOS rendering on the
// main thread via `sotto-windows --snapshot-studio` with `SOTTO_EXPORT_PREVIEW_DIR`.
#![cfg(not(target_os = "macos"))]
//
// Protocol:
//   - Deterministic fixtures only (fixed strings, placeholder icons, embedded
//     Inter). Never the user's real database.
//   - `UPDATE_GOLDENS=1 cargo test --test ui_golden` re-baselines masters
//     after an intentional design change. Review the images before committing.
//   - Comparison budget: per-pixel channel sum > 36 counts as different;
//     more than 0.3% differing pixels fails. Scattered AA noise passes;
//     shifted blocks fail.
//   - Own test binary => own backend process (never conflicts with the
//     headless testing backend used elsewhere).
//
// Backend: winit-software (offscreen pixels, CI-safe).

use sotto_desktop::{app_icon, HistoryItem, StudioWindow};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;

const GOLDEN_DIR: &str = "tests/golden";
const DIFF_BUDGET_PCT: f64 = 0.3;
const CHANNEL_TOLERANCE: i32 = 36;

fn ensure_software_backend() {
    // Must precede any backend initialization; this binary never inits another.
    std::env::set_var("SLINT_BACKEND", "winit-software");
}

fn fixture_item(id: i32, transcript: &str, app: &str, duration: &str) -> HistoryItem {
    let formatted = app_icon::format_app_name(app);
    HistoryItem {
        id,
        timestamp_str: "Oct 12, 11:56 AM".into(),
        transcript: transcript.into(),
        duration_ms: 3200,
        duration_str: duration.into(),
        target_app: formatted.into(),
        // Placeholder icon path: deterministic on every machine (no exe lookup).
        app_icon: slint::Image::default(),
        has_icon: false,
    }
}

fn fixture_list() -> Vec<HistoryItem> {
    let bodies = [
        "The first thing I'm going to do is to make a video of the video. I'm going to show you how to use the video.",
        "It is different than working. Yeah, I think it's working.",
        "Is the trigger working? Yeah, I think it's working.",
        "What is the issue?",
        "If it's not working that well, why?",
        "And I want a loadable hotkey activation that instead of giving the actual hotkey, you give two options.",
        "Another thing is what you can do is the customization option should get like a save button.",
        "The second issue that I think is that the overall UI is not good for scrolling as well.",
        "So few questions right now before proceeding further. First is that when is the like let's say.",
        "What does it mean with this like will the data save, one updates and all that?",
        "And again I make another short bust.",
        "And again I make another shot. Bust.",
        "And again, I make another shot first.",
        "What if I make like short bus again and again?",
        "What if I'm using it in the Chrome, what will happen right now with this?",
        "What happens if I do another one of this? Let's see how much is the memory spike as well.",
        "日本語のテスト mixed with emoji 🚀 and “quotes” — plus ellipsis…",
        "Short.",
        "A medium length dictation about reviewing pull requests for high-DPI awareness on Windows displays.",
        "Final fixture note at the bottom of the list for scroll reach.",
        "Extra record twenty-one for guaranteed scrollable content height.",
        "Extra record twenty-two keeps the scrollbar visible in every golden.",
        "Extra record twenty-three with a somewhat longer body to vary card heights naturally.",
        "Extra record twenty-four. Almost done.",
        "Extra record twenty-five, the very last one.",
    ];
    bodies
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let app = if i % 3 == 0 {
                "T3 Code (Alpha).exe"
            } else {
                "chrome.exe"
            };
            fixture_item(i as i32, b, app, "3.2s")
        })
        .collect()
}

fn str_model(items: Vec<&str>) -> ModelRc<SharedString> {
    ModelRc::from(Rc::new(VecModel::from(
        items.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    )))
}

fn snapshot_pixels(studio: &StudioWindow) -> (u32, u32, Vec<u8>) {
    let buffer = studio.window().take_snapshot().expect("snapshot must succeed");
    let (w, h) = (buffer.width(), buffer.height());
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for p in buffer.as_slice() {
        rgba.extend_from_slice(&[p.r, p.g, p.b, p.a]);
    }
    (w, h, rgba)
}

fn write_bmp(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
    assert_eq!(rgba.len(), (w * h * 4) as usize);
    let file_size = 14 + 40 + rgba.len();
    let mut out = Vec::with_capacity(file_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(14u32 + 40u32).to_le_bytes());
    out.extend_from_slice(&(40u32).to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&(1u16).to_le_bytes());
    out.extend_from_slice(&(32u16).to_le_bytes());
    out.extend_from_slice(&(0u32).to_le_bytes());
    out.extend_from_slice(&(rgba.len() as u32).to_le_bytes());
    out.extend_from_slice(&(2835u32).to_le_bytes());
    out.extend_from_slice(&(2835u32).to_le_bytes());
    out.extend_from_slice(&(0u32).to_le_bytes());
    out.extend_from_slice(&(0u32).to_le_bytes());
    // Negative height = top-down DIB: rows stored in display order, no flip.
    for row in 0..h {
        let start = (row * w * 4) as usize;
        for x in 0..w {
            let i = start + (x * 4) as usize;
            out.push(rgba[i + 2]);
            out.push(rgba[i + 1]);
            out.push(rgba[i]);
            out.push(rgba[i + 3]);
        }
    }
    std::fs::create_dir_all(path.parent().unwrap_or_else(|| std::path::Path::new(GOLDEN_DIR)))
        .expect("golden parent dir must exist");
    std::fs::write(path, out).expect("golden must be writable");
}

fn read_bmp(path: &std::path::Path) -> (u32, u32, Vec<u8>) {
    let data = std::fs::read(path).expect("golden master must exist (run with UPDATE_GOLDENS=1 to create)");
    assert!(data.len() > 54 && &data[0..2] == b"BM", "golden must be a BMP file");
    let w = u32::from_le_bytes(data[18..22].try_into().unwrap());
    let h = (-(i32::from_le_bytes(data[22..26].try_into().unwrap()))) as u32;
    let off = u32::from_le_bytes(data[10..14].try_into().unwrap()) as usize;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    // Negative height = top-down DIB: rows stored in display order, no flip.
    for row in 0..h {
        for x in 0..w {
            let src = off + ((row * w + x) * 4) as usize;
            let dst = ((row * w + x) * 4) as usize;
            rgba[dst] = data[src + 2];
            rgba[dst + 1] = data[src + 1];
            rgba[dst + 2] = data[src];
            rgba[dst + 3] = data[src + 3];
        }
    }
    (w, h, rgba)
}

/// Compare or (re)baseline. Returns diff percent.
fn check_golden(name: &str, w: u32, h: u32, rgba: &[u8]) -> f64 {
    if let Ok(preview_dir) = std::env::var("SOTTO_EXPORT_PREVIEW_DIR") {
        if !preview_dir.trim().is_empty() {
            let preview_path = std::path::PathBuf::from(preview_dir).join(format!("{name}.bmp"));
            write_bmp(&preview_path, w, h, rgba);
        }
    }
    if std::env::var("SOTTO_PREVIEW_ONLY").is_ok() {
        assert!(w > 0 && h > 0, "snapshot {name} must have non-zero size");
        assert!(
            rgba.chunks_exact(4).any(|px| px[0] > 0 || px[1] > 0 || px[2] > 0),
            "snapshot {name} rendered blank image"
        );
        return 0.0;
    }

    let path = std::path::PathBuf::from(GOLDEN_DIR).join(format!("{name}.bmp"));
    if std::env::var("UPDATE_GOLDENS").is_ok() || !path.exists() {
        write_bmp(&path, w, h, rgba);
        eprintln!("[golden] wrote master {name}.bmp ({w}x{h}) — review before committing");
        return 0.0;
    }
    let (gw, gh, gold) = read_bmp(&path);
    assert_eq!((gw, gh), (w, h), "golden size changed for {name}");
    assert_eq!(gold.len(), rgba.len());
    let mut diff = 0usize;
    for (a, b) in gold.chunks_exact(4).zip(rgba.chunks_exact(4)) {
        let d = (a[0] as i32 - b[0] as i32).abs()
            + (a[1] as i32 - b[1] as i32).abs()
            + (a[2] as i32 - b[2] as i32).abs();
        if d > CHANNEL_TOLERANCE {
            diff += 1;
        }
    }
    100.0 * diff as f64 / (w as f64 * h as f64)
}

fn check_budget(name: &str, pct: f64) {
    assert!(
        pct <= DIFF_BUDGET_PCT,
        "{name} regressed: {pct:.2}% pixels differ (budget {DIFF_BUDGET_PCT}%)"
    );
}

#[test]
fn test_golden_masters() {
    ensure_software_backend();
    let studio = StudioWindow::new().expect("StudioWindow must instantiate");
    studio.window().set_size(slint::PhysicalSize::new(1040, 840));
    studio.show().expect("Studio must show headless");

    // Phase 1: dictations tab with fixture history.
    let items = fixture_list();
    let count = items.len() as i32;
    studio.set_history_items(ModelRc::from(Rc::new(VecModel::from(items))));
    studio.set_history_count(count);
    studio.set_active_tab(0);
    let (w, h, rgba) = snapshot_pixels(&studio);
    check_budget("dictations", check_golden("dictations", w, h, &rgba));

    // Phase 2: scrolled to the very bottom (production height formula).
    let bodies = fixture_list();
    let cpl = sotto_desktop::smooth_scroll::chars_per_line_for(1040, 1.0);
    let heights: Vec<f32> = bodies
        .iter()
        .map(|r| sotto_desktop::smooth_scroll::estimate_card_height(&r.transcript, cpl))
        .collect();
    let total = sotto_desktop::smooth_scroll::compute_content_height(
        &heights,
        sotto_desktop::smooth_scroll::ROW_SPACING,
    ) + sotto_desktop::smooth_scroll::LIST_BOTTOM_PADDING
        + sotto_desktop::smooth_scroll::LIST_OVERSCROLL_PX;
    let view_h = studio.get_history_viewport_height();
    studio.set_history_content_height(total);
    studio.set_history_viewport_y(-(total - view_h));
    let (w, h, rgba) = snapshot_pixels(&studio);
    check_budget("dictations_bottom", check_golden("dictations_bottom", w, h, &rgba));

    // Phase 3: providers tab.
    studio.set_provider_id("groq".into());
    studio.set_groq_key("gsk_test_golden_key_12345".into());
    studio.set_gemini_key("AIza_test_golden_key_67890".into());
    studio.set_groq_models(str_model(vec!["whisper-large-v3-turbo", "whisper-large-v3"]));
    studio.set_gemini_models(str_model(vec!["gemini-3.5-transcribe", "gemini-2.5-flash"]));
    studio.set_groq_model_index(0);
    studio.set_gemini_model_index(0);
    studio.set_provider_chip_text("Groq whisper-large-v3-turbo".into());
    studio.set_active_tab(1);
    let (w, h, rgba) = snapshot_pixels(&studio);
    check_budget("providers", check_golden("providers", w, h, &rgba));

    // Phase 4: customization tab.
    studio.set_active_tab(2);
    let (w, h, rgba) = snapshot_pixels(&studio);
    check_budget("customization", check_golden("customization", w, h, &rgba));

    // Phase 5: narrow viewport dictations.
    studio.set_active_tab(0);
    studio.window().set_size(slint::PhysicalSize::new(760, 600));
    let (w, h, rgba) = snapshot_pixels(&studio);
    check_budget("dictations_narrow", check_golden("dictations_narrow", w, h, &rgba));
}
