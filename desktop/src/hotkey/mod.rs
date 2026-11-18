// desktop/src/hotkey/mod.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyMode {
    /// 1st press starts recording, 2nd press stops and transcribes.
    Toggle = 0,
    /// Press & hold starts recording, releasing the key stops and transcribes.
    HoldToTalk = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    StartListening,
    StopListening,
    CancelListening,
}

/// One trigger slot: a keyboard combo and/or a mouse button that each fire it.
/// - `mods`: Win32 MOD_* bitmask (0x1 Alt, 0x2 Ctrl, 0x4 Shift, 0x8 Win).
/// - `key`: non-modifier virtual-key, or None. Modifier-only triggers live in `mods``.
/// - `mouse`: None, or Some(1 = Back, 2 = Forward, 3 = Middle).
/// Invariants (see `is_valid`): non-empty, at most 5 keys total, key never a
/// bare modifier (use `normalized` to fold those into `mods`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TriggerBinding {
    pub mods: u32,
    pub key: Option<u32>,
    pub mouse: Option<i32>,
}

impl Default for TriggerBinding {
    fn default() -> Self {
        Self::empty()
    }
}

impl TriggerBinding {
    pub fn empty() -> Self {
        Self { mods: 0, key: None, mouse: None }
    }

    /// Classic F8 single-key trigger.
    pub fn f8() -> Self {
        Self { mods: 0, key: Some(0x77), mouse: None }
    }

    /// Classic Escape trigger for cancel slot.
    pub fn escape() -> Self {
        Self { mods: 0, key: Some(0x1B), mouse: None }
    }

    /// Ctrl + Shift + Space power combination.
    pub fn ctrl_shift_space() -> Self {
        Self { mods: 0x0002 | 0x0004, key: Some(0x20), mouse: None }
    }

    /// Ctrl + Space trigger.
    pub fn ctrl_space() -> Self {
        Self { mods: 0x0002, key: Some(0x20), mouse: None }
    }

    /// Mouse Back button (XBUTTON1).
    pub fn mouse_back() -> Self {
        Self { mods: 0, key: None, mouse: Some(1) }
    }

    /// Mouse Forward button (XBUTTON2).
    pub fn mouse_forward() -> Self {
        Self { mods: 0, key: None, mouse: Some(2) }
    }

    /// Mouse Middle button (MBUTTON).
    pub fn mouse_middle() -> Self {
        Self { mods: 0, key: None, mouse: Some(3) }
    }

    /// Build from raw recorder parts, folding bare-modifier keys into `mods`
    /// (e.g. standalone Ctrl press becomes mods=Ctrl, key=None).
    pub fn normalized(mods: u32, key: Option<u32>, mouse: Option<i32>) -> Self {
        let mut mods = mods & 0x000F;
        let mut key = key;
        if let Some(vk) = key {
            if is_modifier_vk(vk) {
                mods |= mod_bit_for_vk(vk);
                key = None;
            }
        }
        let mouse = match mouse {
            Some(b) if (1..=3).contains(&b) => Some(b),
            _ => None,
        };
        Self { mods, key, mouse }
    }

    pub fn is_empty(&self) -> bool {
        self.key.is_none() && self.mouse.is_none() && self.mods & 0x000F == 0
    }

    /// Total keys involved: modifiers + main key + mouse button (cap: 5).
    pub fn key_count(&self) -> usize {
        self.mods.count_ones() as usize
            + usize::from(self.key.is_some())
            + usize::from(self.mouse.is_some())
    }

    pub fn is_valid(&self) -> bool {
        !self.is_empty()
            && self.key_count() <= 5
            && self.key.is_none_or(|vk| !is_modifier_vk(vk))
            && self.mouse.is_none_or(|b| (1..=3).contains(&b))
            && self.mods & !0x000F == 0
    }

    /// Human-readable form: "F8", "Ctrl + Shift + Space", "Mouse Back",
    /// "Ctrl + Mouse Back", "Ctrl" (modifier-only). Empty renders "Not set".
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.mods & 0x0002 != 0 {
            parts.push("Ctrl".to_string());
        }
        if self.mods & 0x0001 != 0 {
            parts.push("Alt".to_string());
        }
        if self.mods & 0x0004 != 0 {
            parts.push("Shift".to_string());
        }
        if self.mods & 0x0008 != 0 {
            parts.push("Win".to_string());
        }
        if let Some(vk) = self.key {
            parts.push(vk_to_string(vk));
        }
        if let Some(b) = self.mouse {
            parts.push(mouse_trigger_display(b));
        }
        if parts.is_empty() {
            return "Not set".to_string();
        }
        parts.join(" + ")
    }
}

/// True for standalone modifier keys (Ctrl/Shift/Alt/Win/Fn incl. L/R variants).
pub fn is_modifier_vk(vk: u32) -> bool {
    matches!(
        vk,
        0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5 | 0xFF | 0x88 | 0xDF
    )
}

/// MOD_* bit for a modifier virtual-key (0 when not a modifier).
pub fn mod_bit_for_vk(vk: u32) -> u32 {
    match vk {
        0x11 | 0xA2 | 0xA3 => 0x0002, // Ctrl
        0x12 | 0xA4 | 0xA5 => 0x0001, // Alt
        0x10 | 0xA0 | 0xA1 => 0x0004, // Shift
        0x5B | 0x5C => 0x0008,        // Win
        _ => 0,
    }
}

/// Representative generic VKs for each set MOD_* bit (for watcher polling).
pub fn mod_vks_in_mask(mask: u32) -> Vec<u32> {
    let mut out = Vec::new();
    if mask & 0x0002 != 0 {
        out.push(0x11);
    }
    if mask & 0x0001 != 0 {
        out.push(0x12);
    }
    if mask & 0x0004 != 0 {
        out.push(0x10);
    }
    if mask & 0x0008 != 0 {
        out.push(0x5B);
    }
    out
}

/// How a trigger binding maps into engine subsystems:
/// - `keyboard`: (mods, vk) for `RegisterHotKey` (or None if no key).
/// - `standalone`: generic VKs for the modifier watcher (non-empty only
///   when the binding is modifier-only, e.g. standalone Ctrl press).
/// - `mouse`: (button, required-mods-mask) for the mouse hook matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingPlan {
    pub keyboard: Option<(u32, u32)>,
    pub standalone: Vec<u32>,
    pub mouse: Option<(i32, u32)>,
}

/// Pure plan-derivation: transforms one `TriggerBinding` into the exact
/// parameters expected by the hotkey subsystems.
pub fn plan_binding(b: &TriggerBinding) -> BindingPlan {
    let keyboard = b.key.map(|vk| (b.mods, vk));
    // A mouse chord must NOT also arm its modifiers standalone.
    let standalone = if b.key.is_none() && b.mouse.is_none() {
        mod_vks_in_mask(b.mods)
    } else {
        Vec::new()
    };
    let mouse = b.mouse.map(|btn| (btn, b.mods));
    BindingPlan { keyboard, standalone, mouse }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_binding_is_not_valid() {
        assert!(!TriggerBinding::empty().is_valid());
    }

    #[test]
    fn test_f8_is_valid() {
        let f8 = TriggerBinding::f8();
        assert!(f8.is_valid());
        assert_eq!(f8.display(), "F8");
        let p = plan_binding(&f8);
        assert_eq!(p.keyboard, Some((0, 0x77)));
        assert!(p.standalone.is_empty());
        assert_eq!(p.mouse, None);
    }

    #[test]
    fn test_escape_is_valid() {
        let esc = TriggerBinding::escape();
        assert!(esc.is_valid());
        assert_eq!(esc.display(), "Esc");
        let p = plan_binding(&esc);
        assert_eq!(p.keyboard, Some((0, 0x1B)));
        assert!(p.standalone.is_empty());
        assert_eq!(p.mouse, None);
    }

    #[test]
    fn test_ctrl_shift_space_plan() {
        let b = TriggerBinding::ctrl_shift_space();
        assert!(b.is_valid());
        assert_eq!(b.display(), "Ctrl + Shift + Space");
        let p = plan_binding(&b);
        assert_eq!(p.keyboard, Some((0x0002 | 0x0004, 0x20)));
        assert!(p.standalone.is_empty());
        assert_eq!(p.mouse, None);
    }

    #[test]
    fn test_standalone_ctrl_plan() {
        // Standalone Ctrl press (mods=0, vk=Ctrl) becomes mods-only.
        let b = TriggerBinding::normalized(0, Some(0x11), None);
        assert!(b.is_valid());
        assert_eq!(b.display(), "Ctrl");
        assert_eq!(b.mods, 0x0002);
        assert_eq!(b.key, None);
        let p = plan_binding(&b);
        assert_eq!(p.keyboard, None);
        assert_eq!(p.standalone, vec![0x11]);
        assert_eq!(p.mouse, None);
    }

    #[test]
    fn test_mouse_back_plan() {
        let b = TriggerBinding { mods: 0, key: None, mouse: Some(1) };
        assert!(b.is_valid());
        assert_eq!(b.display(), "Mouse Back");
        let p = plan_binding(&b);
        assert_eq!(p.keyboard, None);
        assert!(p.standalone.is_empty());
        assert_eq!(p.mouse, Some((1, 0)));
    }

    #[test]
    fn test_ctrl_mouse_back_chord_plan() {
        let b = TriggerBinding { mods: 0x0002, key: None, mouse: Some(1) };
        assert!(b.is_valid());
        assert_eq!(b.display(), "Ctrl + Mouse Back");
        let p = plan_binding(&b);
        // Mouse-only -> hook matcher only.
        assert_eq!(p.keyboard, None);
        // Chord -> hook matcher gated on mods, no OS hotkey.
        assert_eq!(p.mouse, Some((1, 0x0002)));
        // Must NOT also register Ctrl as a standalone modifier!
        assert!(p.standalone.is_empty());
    }

    #[test]
    fn test_ctrl_shift_standalone_plan() {
        let b = TriggerBinding { mods: 0x0002 | 0x0004, key: None, mouse: None };
        assert!(b.is_valid());
        assert_eq!(b.display(), "Ctrl + Shift");
        let p = plan_binding(&b);
        assert_eq!(p.keyboard, None);
        assert_eq!(p.standalone, vec![0x11, 0x10]);
        assert_eq!(p.mouse, None);
    }

    #[test]
    fn test_key_count_cap() {
        // 4 mods + key = 5 (allowed)
        let b = TriggerBinding { mods: 0x000F, key: Some(0x20), mouse: None };
        assert_eq!(b.key_count(), 5);
        assert!(b.is_valid());
        // 4 mods + key + mouse = 6 (disallowed)
        let b = TriggerBinding { mods: 0x000F, key: Some(0x20), mouse: Some(1) };
        assert_eq!(b.key_count(), 6);
        assert!(!b.is_valid());
    }

    #[test]
    fn test_bare_modifier_in_key_rejected_by_is_valid() {
        // Raw un-normalized struct with Ctrl in `key` is invalid.
        let b = TriggerBinding { mods: 0, key: Some(0x11), mouse: None };
        assert!(!b.is_valid());
        // Normalized version is valid.
        assert!(TriggerBinding::normalized(0, Some(0x11), None).is_valid());
    }
}

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(target_os = "windows"))]
pub struct HotkeyManager;

#[cfg(not(target_os = "windows"))]
impl HotkeyManager {
    pub fn new<F>(_mode: HotkeyMode, _callback: F) -> Result<Self, String>
    where
        F: Fn(HotkeyEvent) + Send + Sync + 'static,
    {
        Err("Global hotkeys not yet implemented for non-Windows platforms".to_string())
    }

    pub fn new_with_bindings<F>(
        _mode: HotkeyMode,
        _primary: &TriggerBinding,
        _secondary: &Option<TriggerBinding>,
        _cancel_primary: &Option<TriggerBinding>,
        _cancel_secondary: &Option<TriggerBinding>,
        _callback: F,
    ) -> Result<Self, String>
    where
        F: Fn(HotkeyEvent) + Send + Sync + 'static,
    {
        Err("Global hotkeys not yet implemented for non-Windows platforms".to_string())
    }

    pub fn set_mode(&self, _mode: HotkeyMode) {}
    pub fn set_recording(&self, _recording: bool) {}
    pub fn apply_bindings(
        &self,
        _primary: &TriggerBinding,
        _secondary: &Option<TriggerBinding>,
        _cancel_primary: &Option<TriggerBinding>,
        _cancel_secondary: &Option<TriggerBinding>,
    ) {}
    pub fn disarm_hotkeys(&self) {}
    pub fn mode(&self) -> HotkeyMode { HotkeyMode::Toggle }

    pub fn set_record_hook(_f: Option<std::sync::Arc<dyn Fn(u32, Option<u32>, i32) + Send + Sync + 'static>>) {}
    pub fn current_mod_mask() -> u32 { 0 }
}

#[cfg(not(target_os = "windows"))]
pub fn mouse_hook_status() -> i32 { 0 }

#[cfg(not(target_os = "windows"))]
pub fn mouse_trigger_display(button: i32) -> String {
    match button {
        1 => "Mouse Back".to_string(),
        2 => "Mouse Forward".to_string(),
        3 => "Mouse Middle".to_string(),
        _ => "Off".to_string(),
    }
}

#[cfg(not(target_os = "windows"))]
pub fn mouse_trigger_vk(button: i32) -> Option<u32> {
    match button {
        1 => Some(0x05),
        2 => Some(0x06),
        3 => Some(0x04),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
pub fn vk_to_string(vk: u32) -> String {
    match vk {
        0x11 | 0xA2 | 0xA3 => "Ctrl".to_string(),
        0x10 | 0xA0 | 0xA1 => "Shift".to_string(),
        0x12 | 0xA4 | 0xA5 => "Alt".to_string(),
        0x5B | 0x5C => "Win".to_string(),
        0x5D => "Menu".to_string(),
        0x14 => "Caps Lock".to_string(),
        0x90 => "Num Lock".to_string(),
        0x91 => "Scroll Lock".to_string(),
        0x2C => "Print Screen".to_string(),
        0x13 => "Pause".to_string(),
        0xFF | 0x88 | 0xDF => "Fn".to_string(),
        0x20 => "Space".to_string(),
        0x1B => "Esc".to_string(),
        0x7C..=0x87 => format!("F{}", 13 + (vk - 0x7C)),
        0x77 => "F8".to_string(),
        _ => format!("Key 0x{:02X}", vk),
    }
}

#[cfg(not(target_os = "windows"))]
pub fn format_hotkey(_modifiers: u32, _vk: u32) -> String {
    "F8".to_string()
}

#[cfg(not(target_os = "windows"))]
pub fn is_key_down(_vk: u16) -> bool {
    false
}
