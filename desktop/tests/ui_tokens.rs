// Token enforcement suite: the design system in `ui/components/theme.slint`
// is the single source of truth. These tests read the sources directly:
//   - no raw 6-digit hex outside theme.slint (8-digit shadow alphas allowlisted)
//   - no raw font-size / spacing / padding literals outside theme.slint
//   - every Theme color token matches the ui_quality spec mirrors, so the
//     WCAG contrast tests transitively verify the live theme
//
// Runs on source text (no backend needed), CI-safe.

mod support;

use support::ui_quality;
use std::collections::HashMap;
use std::path::PathBuf;

fn ui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")
}

fn slint_sources() -> Vec<PathBuf> {
    let mut files = vec![ui_dir().join("hud.slint")];
    let components = std::fs::read_dir(ui_dir().join("components")).expect("components dir must exist");
    let mut rest: Vec<PathBuf> = components
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "slint"))
        .filter(|p| p.file_name().is_some_and(|n| n != "theme.slint"))
        .collect();
    rest.sort();
    files.extend(rest);
    files
}

fn read_all() -> Vec<(PathBuf, String)> {
    slint_sources()
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("must read {}", p.display()));
            (p, text)
        })
        .collect()
}

/// 8-digit shadow alphas are the only sanctioned raw colors (translucency
/// can't be expressed as an opaque token).
const ALLOWED_ALPHA_HEX: [&str; 2] = ["#00000033", "#00000066"];

#[test]
fn test_no_raw_hex_outside_theme() {
    let mut violations = Vec::new();
    for (path, text) in read_all() {
        for (lineno, line) in text.lines().enumerate() {
            let mut rest = line;
            while let Some(idx) = rest.find('#') {
                let tail = &rest[idx..];
                let hex: String = tail.chars().take(9).collect();
                if hex.len() >= 7
                    && hex[1..].chars().take(6).all(|c| c.is_ascii_hexdigit())
                    && hex[1..].chars().nth(6).is_none_or(|c| !c.is_ascii_hexdigit())
                {
                    violations.push(format!("{}:{}: raw 6-digit hex {hex}", path.display(), lineno + 1));
                } else if hex.len() == 9 && hex[1..].chars().all(|c| c.is_ascii_hexdigit()) {
                    if !ALLOWED_ALPHA_HEX.contains(&hex.as_str()) {
                        violations.push(format!(
                            "{}:{}: unlisted 8-digit hex {hex}",
                            path.display(),
                            lineno + 1
                        ));
                    }
                }
                rest = &tail[1.min(tail.len())..];
                if rest.len() < 7 {
                    // Step past this '#' to keep scanning the line.
                    rest = tail.get(1..).unwrap_or("");
                }
                if rest.is_empty() {
                    break;
                }
            }
        }
    }
    assert!(violations.is_empty(), "raw hex outside theme.slint:\n{}", violations.join("\n"));
}

#[test]
fn test_no_raw_typography_or_spacing_outside_theme() {
    let mut violations = Vec::new();
    for (path, text) in read_all() {
        for (lineno, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            for marker in ["font-size:"] {
                if let Some(i) = t.find(marker) {
                    let after = &t[i + marker.len()..];
                    if after.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        violations.push(format!(
                            "{}:{}: raw {marker} value (use Theme.type-*)",
                            path.display(),
                            lineno + 1
                        ));
                    }
                }
            }
            // NOTE: letter-spacing (0.5px micro-tracking) is intentionally
            // inline, so match spacing only when not part of letter-spacing.
            for marker in [
                "spacing:",
                "padding:",
                "padding-left:",
                "padding-right:",
                "padding-top:",
                "padding-bottom:",
            ] {
                let mut search = t;
                let mut base = 0usize;
                while let Some(rel) = search.find(marker) {
                    let i = base + rel;
                    let prev_ok = i == 0 || t.as_bytes()[i - 1] != b'-';
                    let after = &t[i + marker.len()..];
                    if prev_ok
                        && after.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit())
                    {
                        violations.push(format!(
                            "{}:{}: raw {marker} value (use Theme.space-*)",
                            path.display(),
                            lineno + 1
                        ));
                        break;
                    }
                    search = &search[rel + marker.len()..];
                    base = i + marker.len();
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "raw literals outside theme.slint:\n{}",
        violations.join("\n")
    );
}

/// Parses `out property <color> name: #hex;` tokens from theme.slint.
fn theme_colors() -> HashMap<String, String> {
    let text = std::fs::read_to_string(ui_dir().join("components").join("theme.slint"))
        .expect("theme.slint must exist");
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("out property <color>") {
            // " name: #hex;"
            let mut parts = rest.split([':', ';']);
            if let (Some(name), Some(value)) = (parts.next(), parts.next()) {
                map.insert(name.trim().to_string(), value.trim().to_lowercase());
            }
        }
    }
    map
}

#[test]
fn test_theme_tokens_match_spec() {
    let tokens = theme_colors();
    assert!(tokens.len() >= 30, "theme must declare the palette");
    // ui_quality mirrors pin the documented values; this pins the mirrors to
    // the live theme, so contrast tests verify what actually ships.
    let pairs = [
        (ui_quality::INK_PRIMARY, "ink-primary"),
        (ui_quality::INK_SECONDARY, "ink-secondary"),
        (ui_quality::INK_MUTED, "ink-muted"),
        (ui_quality::ACCENT, "accent"),
        (ui_quality::SUCCESS, "success"),
        (ui_quality::CARD_BG, "card-bg"),
        (ui_quality::CHROME_BG, "chrome-bg"),
    ];
    for (spec, token) in pairs {
        let live = tokens
            .get(token)
            .unwrap_or_else(|| panic!("theme must declare {token}"));
        assert_eq!(
            live.as_str(),
            spec.to_lowercase(),
            "spec mirror for {token} drifted from theme.slint"
        );
    }
    // The live palette itself must keep passing contrast on dark surfaces.
    let ink = &tokens["ink-primary"];
    let card = &tokens["card-bg"];
    let ratio = ui_quality::contrast_ratio(ink, card).expect("contrast computable");
    assert!(ratio >= 7.0, "live ink/card contrast must stay AAA, got {ratio:.2}");
}
