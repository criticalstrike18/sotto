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
    let config = || {
        slint_build::CompilerConfiguration::new().with_debug_info(true)
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
    ] {
        slint_build::compile_with_config(component, config())
            .expect("Slint component compilation failed");
    }
    slint_build::compile_with_config("ui/hud.slint", config())
        .expect("Slint compilation failed");
}
