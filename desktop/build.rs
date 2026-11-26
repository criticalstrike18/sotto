fn main() {
    // Debug info embeds element ids so tests can query arbitrary elements
    // through i_slint_backend_testing::ElementHandle (geometry, clicks).
    //
    // NOTE on "doesn't inherit Window / deprecated" warnings (one per file
    // below): each component root is intentionally compiled separately so
    // tests can instantiate it in isolation. Slint keeps the last non-Window
    // export working (compat behavior) and only warns. Verified harmless:
    // codegen succeeds and all 240+ tests pass. There is no stable-API knob
    // to silence them. If a future Slint removes this path it will fail LOUD
    // at compile time; migrate then to library mode or slint-interpreter.
    // Release builds omit Slint debug element-id tables to reduce binary size
    // and memory overhead; dev/test builds keep them enabled for ElementHandle
    // UI tests (unless SOTTO_SLINT_DEBUG_INFO is explicitly set for release tests).
    println!("cargo:rerun-if-env-changed=SOTTO_SLINT_DEBUG_INFO");
    let emit_debug_info = std::env::var("PROFILE").as_deref() != Ok("release")
        || std::env::var_os("SOTTO_SLINT_DEBUG_INFO").is_some();
    let config = || {
        slint_build::CompilerConfiguration::new().with_debug_info(emit_debug_info)
    };
    // Each ui/components/*.slint root is compiled separately so tests can
    // instantiate components in isolation. hud.slint is compiled LAST so
    // `slint::include_modules!()` (single-file include) picks up the app.
    for component in [
        "ui/components/stat_chip.slint",
        "ui/components/section_header.slint",
        "ui/components/tab_bar.slint",
        "ui/components/provider_card.slint",
        "ui/components/model_row.slint",
        "ui/components/key_field.slint",
        "ui/components/history_card.slint",
        "ui/components/switch.slint",
        "ui/components/keycap.slint",
        "ui/components/keycap_combo.slint",
    ] {
        slint_build::compile_with_config(component, config())
            .expect("Slint component compilation failed");
    }
    slint_build::compile_with_config("ui/hud.slint", config())
        .expect("Slint compilation failed");

    println!("cargo:rerun-if-changed=assets/icons/sotto.ico");
    #[cfg(target_os = "windows")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = tauri_winres::WindowsResource::new();
        res.set_icon("assets/icons/sotto.ico")
            .set("ProductName", "Sotto")
            .set("FileDescription", "Sotto — Voice Dictation & HUD")
            .set("OriginalFilename", "sotto-windows.exe");
        res.compile_for(&["sotto-windows"])
            .expect("Windows icon/version resource compilation failed");
    }
}
