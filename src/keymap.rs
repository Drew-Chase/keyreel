use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// Converts a virtual key code, scan code, and extended flag into a human-readable label.
///
/// Returns distinct labels for: left/right modifiers, numpad vs. nav keys,
/// Enter vs. NumpadEnter, F1-F24, PrintScreen, Pause, NumLock, etc.
pub fn vk_to_label(vk: u32, scan: u32, extended: bool) -> String {
    // Check our override table first
    if let Some(label) = override_label(vk, extended) {
        return label.to_string();
    }

    // Fall back to GetKeyNameTextW
    get_key_name_text(scan, extended)
}

/// Returns a static label for known keys that need special handling.
fn override_label(vk: u32, extended: bool) -> Option<&'static str> {
    // Virtual key constants as u32
    const VK_BACK_U: u32 = VK_BACK.0 as u32;
    const VK_TAB_U: u32 = VK_TAB.0 as u32;
    const VK_RETURN_U: u32 = VK_RETURN.0 as u32;
    const VK_ESCAPE_U: u32 = VK_ESCAPE.0 as u32;
    const VK_SPACE_U: u32 = VK_SPACE.0 as u32;
    const VK_PRIOR_U: u32 = VK_PRIOR.0 as u32;
    const VK_NEXT_U: u32 = VK_NEXT.0 as u32;
    const VK_END_U: u32 = VK_END.0 as u32;
    const VK_HOME_U: u32 = VK_HOME.0 as u32;
    const VK_LEFT_U: u32 = VK_LEFT.0 as u32;
    const VK_UP_U: u32 = VK_UP.0 as u32;
    const VK_RIGHT_U: u32 = VK_RIGHT.0 as u32;
    const VK_DOWN_U: u32 = VK_DOWN.0 as u32;
    const VK_SNAPSHOT_U: u32 = VK_SNAPSHOT.0 as u32;
    const VK_INSERT_U: u32 = VK_INSERT.0 as u32;
    const VK_DELETE_U: u32 = VK_DELETE.0 as u32;
    const VK_PAUSE_U: u32 = VK_PAUSE.0 as u32;
    const VK_CAPITAL_U: u32 = VK_CAPITAL.0 as u32;
    const VK_NUMLOCK_U: u32 = VK_NUMLOCK.0 as u32;
    const VK_SCROLL_U: u32 = VK_SCROLL.0 as u32;
    // Modifiers (left/right)
    const VK_LSHIFT_U: u32 = VK_LSHIFT.0 as u32;
    const VK_RSHIFT_U: u32 = VK_RSHIFT.0 as u32;
    const VK_LCONTROL_U: u32 = VK_LCONTROL.0 as u32;
    const VK_RCONTROL_U: u32 = VK_RCONTROL.0 as u32;
    const VK_LMENU_U: u32 = VK_LMENU.0 as u32;
    const VK_RMENU_U: u32 = VK_RMENU.0 as u32;
    const VK_LWIN_U: u32 = VK_LWIN.0 as u32;
    const VK_RWIN_U: u32 = VK_RWIN.0 as u32;
    // Numpad
    const VK_NUMPAD0_U: u32 = VK_NUMPAD0.0 as u32;
    const VK_NUMPAD1_U: u32 = VK_NUMPAD1.0 as u32;
    const VK_NUMPAD2_U: u32 = VK_NUMPAD2.0 as u32;
    const VK_NUMPAD3_U: u32 = VK_NUMPAD3.0 as u32;
    const VK_NUMPAD4_U: u32 = VK_NUMPAD4.0 as u32;
    const VK_NUMPAD5_U: u32 = VK_NUMPAD5.0 as u32;
    const VK_NUMPAD6_U: u32 = VK_NUMPAD6.0 as u32;
    const VK_NUMPAD7_U: u32 = VK_NUMPAD7.0 as u32;
    const VK_NUMPAD8_U: u32 = VK_NUMPAD8.0 as u32;
    const VK_NUMPAD9_U: u32 = VK_NUMPAD9.0 as u32;
    const VK_MULTIPLY_U: u32 = VK_MULTIPLY.0 as u32;
    const VK_ADD_U: u32 = VK_ADD.0 as u32;
    const VK_SUBTRACT_U: u32 = VK_SUBTRACT.0 as u32;
    const VK_DECIMAL_U: u32 = VK_DECIMAL.0 as u32;
    const VK_DIVIDE_U: u32 = VK_DIVIDE.0 as u32;
    const VK_SEPARATOR_U: u32 = VK_SEPARATOR.0 as u32;
    // Function keys
    const VK_F1_U: u32 = VK_F1.0 as u32;
    const VK_F2_U: u32 = VK_F2.0 as u32;
    const VK_F3_U: u32 = VK_F3.0 as u32;
    const VK_F4_U: u32 = VK_F4.0 as u32;
    const VK_F5_U: u32 = VK_F5.0 as u32;
    const VK_F6_U: u32 = VK_F6.0 as u32;
    const VK_F7_U: u32 = VK_F7.0 as u32;
    const VK_F8_U: u32 = VK_F8.0 as u32;
    const VK_F9_U: u32 = VK_F9.0 as u32;
    const VK_F10_U: u32 = VK_F10.0 as u32;
    const VK_F11_U: u32 = VK_F11.0 as u32;
    const VK_F12_U: u32 = VK_F12.0 as u32;
    const VK_F13_U: u32 = VK_F13.0 as u32;
    const VK_F14_U: u32 = VK_F14.0 as u32;
    const VK_F15_U: u32 = VK_F15.0 as u32;
    const VK_F16_U: u32 = VK_F16.0 as u32;
    const VK_F17_U: u32 = VK_F17.0 as u32;
    const VK_F18_U: u32 = VK_F18.0 as u32;
    const VK_F19_U: u32 = VK_F19.0 as u32;
    const VK_F20_U: u32 = VK_F20.0 as u32;
    const VK_F21_U: u32 = VK_F21.0 as u32;
    const VK_F22_U: u32 = VK_F22.0 as u32;
    const VK_F23_U: u32 = VK_F23.0 as u32;
    const VK_F24_U: u32 = VK_F24.0 as u32;

    match vk {
        // Left/right modifiers
        VK_LSHIFT_U => Some("LShift"),
        VK_RSHIFT_U => Some("RShift"),
        VK_LCONTROL_U => Some("LCtrl"),
        VK_RCONTROL_U => Some("RCtrl"),
        VK_LMENU_U => Some("LAlt"),
        VK_RMENU_U => Some("RAlt"),
        VK_LWIN_U => Some("LWin"),
        VK_RWIN_U => Some("RWin"),

        // Navigation / editing
        VK_BACK_U => Some("Backspace"),
        VK_TAB_U => Some("Tab"),
        VK_RETURN_U => {
            if extended {
                Some("NumEnter")
            } else {
                Some("Enter")
            }
        }
        VK_ESCAPE_U => Some("Escape"),
        VK_SPACE_U => Some("Space"),
        VK_PRIOR_U => Some("PgUp"),
        VK_NEXT_U => Some("PgDn"),
        VK_END_U => Some("End"),
        VK_HOME_U => Some("Home"),
        VK_LEFT_U => Some("Left"),
        VK_UP_U => Some("Up"),
        VK_RIGHT_U => Some("Right"),
        VK_DOWN_U => Some("Down"),
        VK_INSERT_U => Some("Insert"),
        VK_DELETE_U => Some("Delete"),

        // Lock / special
        VK_SNAPSHOT_U => Some("PrintScreen"),
        VK_PAUSE_U => Some("Pause"),
        VK_CAPITAL_U => Some("CapsLock"),
        VK_NUMLOCK_U => Some("NumLock"),
        VK_SCROLL_U => Some("ScrollLock"),

        // Numpad
        VK_NUMPAD0_U => Some("Num0"),
        VK_NUMPAD1_U => Some("Num1"),
        VK_NUMPAD2_U => Some("Num2"),
        VK_NUMPAD3_U => Some("Num3"),
        VK_NUMPAD4_U => Some("Num4"),
        VK_NUMPAD5_U => Some("Num5"),
        VK_NUMPAD6_U => Some("Num6"),
        VK_NUMPAD7_U => Some("Num7"),
        VK_NUMPAD8_U => Some("Num8"),
        VK_NUMPAD9_U => Some("Num9"),
        VK_MULTIPLY_U => Some("Num*"),
        VK_ADD_U => Some("Num+"),
        VK_SUBTRACT_U => Some("Num-"),
        VK_DECIMAL_U => Some("Num."),
        VK_DIVIDE_U => Some("Num/"),
        VK_SEPARATOR_U => Some("NumSep"),

        // Function keys
        VK_F1_U => Some("F1"),
        VK_F2_U => Some("F2"),
        VK_F3_U => Some("F3"),
        VK_F4_U => Some("F4"),
        VK_F5_U => Some("F5"),
        VK_F6_U => Some("F6"),
        VK_F7_U => Some("F7"),
        VK_F8_U => Some("F8"),
        VK_F9_U => Some("F9"),
        VK_F10_U => Some("F10"),
        VK_F11_U => Some("F11"),
        VK_F12_U => Some("F12"),
        VK_F13_U => Some("F13"),
        VK_F14_U => Some("F14"),
        VK_F15_U => Some("F15"),
        VK_F16_U => Some("F16"),
        VK_F17_U => Some("F17"),
        VK_F18_U => Some("F18"),
        VK_F19_U => Some("F19"),
        VK_F20_U => Some("F20"),
        VK_F21_U => Some("F21"),
        VK_F22_U => Some("F22"),
        VK_F23_U => Some("F23"),
        VK_F24_U => Some("F24"),

        _ => None,
    }
}

/// Uses GetKeyNameTextW to look up the OS name for a scan code.
fn get_key_name_text(scan: u32, extended: bool) -> String {
    // Pack scan code into the lparam format that GetKeyNameTextW expects:
    // Bits 16-23: scan code
    // Bit 24: extended-key flag
    // Bit 25: "don't distinguish" bit (0 = return specific name)
    let mut lparam = (scan & 0xFF) << 16;
    if extended {
        lparam |= 1 << 24;
    }

    let mut buf = [0u16; 64];
    let len = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyNameTextW(lparam as i32, &mut buf) };
    if len > 0 {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        format!("VK_{vk:#04X}", vk = scan)
    }
}

/// Returns true if this VK is a modifier key (shift, ctrl, alt, win).
pub fn is_modifier(vk: u32) -> bool {
    matches!(
        vk,
        x if x == VK_LSHIFT.0 as u32
            || x == VK_RSHIFT.0 as u32
            || x == VK_LCONTROL.0 as u32
            || x == VK_RCONTROL.0 as u32
            || x == VK_LMENU.0 as u32
            || x == VK_RMENU.0 as u32
            || x == VK_LWIN.0 as u32
            || x == VK_RWIN.0 as u32
            || x == VK_SHIFT.0 as u32
            || x == VK_CONTROL.0 as u32
            || x == VK_MENU.0 as u32
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_labels() {
        assert_eq!(vk_to_label(VK_LSHIFT.0 as u32, 0x2A, false), "LShift");
        assert_eq!(vk_to_label(VK_RSHIFT.0 as u32, 0x36, false), "RShift");
        assert_eq!(vk_to_label(VK_LCONTROL.0 as u32, 0x1D, false), "LCtrl");
        assert_eq!(vk_to_label(VK_RCONTROL.0 as u32, 0x1D, true), "RCtrl");
        assert_eq!(vk_to_label(VK_LMENU.0 as u32, 0x38, false), "LAlt");
        assert_eq!(vk_to_label(VK_RMENU.0 as u32, 0x38, true), "RAlt");
        assert_eq!(vk_to_label(VK_LWIN.0 as u32, 0x5B, true), "LWin");
        assert_eq!(vk_to_label(VK_RWIN.0 as u32, 0x5C, true), "RWin");
    }

    #[test]
    fn function_keys() {
        assert_eq!(vk_to_label(VK_F1.0 as u32, 0x3B, false), "F1");
        assert_eq!(vk_to_label(VK_F12.0 as u32, 0x58, false), "F12");
        assert_eq!(vk_to_label(VK_F24.0 as u32, 0, false), "F24");
    }

    #[test]
    fn enter_vs_numpad_enter() {
        assert_eq!(vk_to_label(VK_RETURN.0 as u32, 0x1C, false), "Enter");
        assert_eq!(vk_to_label(VK_RETURN.0 as u32, 0x1C, true), "NumEnter");
    }

    #[test]
    fn numpad_keys() {
        assert_eq!(vk_to_label(VK_NUMPAD0.0 as u32, 0x52, false), "Num0");
        assert_eq!(vk_to_label(VK_NUMPAD9.0 as u32, 0x49, false), "Num9");
        assert_eq!(vk_to_label(VK_ADD.0 as u32, 0x4E, false), "Num+");
        assert_eq!(vk_to_label(VK_SUBTRACT.0 as u32, 0x4A, false), "Num-");
        assert_eq!(vk_to_label(VK_MULTIPLY.0 as u32, 0x37, false), "Num*");
        assert_eq!(vk_to_label(VK_DIVIDE.0 as u32, 0x35, true), "Num/");
        assert_eq!(vk_to_label(VK_DECIMAL.0 as u32, 0x53, false), "Num.");
    }

    #[test]
    fn navigation_keys() {
        assert_eq!(vk_to_label(VK_HOME.0 as u32, 0x47, true), "Home");
        assert_eq!(vk_to_label(VK_END.0 as u32, 0x4F, true), "End");
        assert_eq!(vk_to_label(VK_PRIOR.0 as u32, 0x49, true), "PgUp");
        assert_eq!(vk_to_label(VK_NEXT.0 as u32, 0x51, true), "PgDn");
        assert_eq!(vk_to_label(VK_INSERT.0 as u32, 0x52, true), "Insert");
        assert_eq!(vk_to_label(VK_DELETE.0 as u32, 0x53, true), "Delete");
        assert_eq!(vk_to_label(VK_LEFT.0 as u32, 0x4B, true), "Left");
        assert_eq!(vk_to_label(VK_UP.0 as u32, 0x48, true), "Up");
        assert_eq!(vk_to_label(VK_RIGHT.0 as u32, 0x4D, true), "Right");
        assert_eq!(vk_to_label(VK_DOWN.0 as u32, 0x50, true), "Down");
    }

    #[test]
    fn special_keys() {
        assert_eq!(vk_to_label(VK_ESCAPE.0 as u32, 0x01, false), "Escape");
        assert_eq!(vk_to_label(VK_TAB.0 as u32, 0x0F, false), "Tab");
        assert_eq!(vk_to_label(VK_BACK.0 as u32, 0x0E, false), "Backspace");
        assert_eq!(vk_to_label(VK_SPACE.0 as u32, 0x39, false), "Space");
        assert_eq!(vk_to_label(VK_SNAPSHOT.0 as u32, 0x37, true), "PrintScreen");
        assert_eq!(vk_to_label(VK_PAUSE.0 as u32, 0x45, false), "Pause");
        assert_eq!(vk_to_label(VK_CAPITAL.0 as u32, 0x3A, false), "CapsLock");
        assert_eq!(vk_to_label(VK_NUMLOCK.0 as u32, 0x45, true), "NumLock");
        assert_eq!(vk_to_label(VK_SCROLL.0 as u32, 0x46, false), "ScrollLock");
    }

    #[test]
    fn is_modifier_check() {
        assert!(is_modifier(VK_LSHIFT.0 as u32));
        assert!(is_modifier(VK_RCONTROL.0 as u32));
        assert!(is_modifier(VK_LMENU.0 as u32));
        assert!(is_modifier(VK_RMENU.0 as u32));
        assert!(is_modifier(VK_LWIN.0 as u32));
        assert!(!is_modifier(VK_RETURN.0 as u32));
        assert!(!is_modifier(VK_F1.0 as u32));
        assert!(!is_modifier(0x41)); // 'A'
    }

    #[test]
    fn character_key_fallback() {
        // 'A' key: scan code 0x1E, not extended. Should use GetKeyNameTextW fallback.
        let label = vk_to_label(0x41, 0x1E, false);
        // GetKeyNameTextW should return something non-empty for a valid scan code
        assert!(!label.is_empty());
    }
}
