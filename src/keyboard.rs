use crate::clock;
use crate::keymap;
use serde::Serialize;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::LRESULT;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct KeyEvent {
    pub qpc: i64,
    pub vk: u32,
    pub scan: u32,
    pub extended: bool,
    pub injected: bool,
    pub down: bool,
    pub mods: Modifiers,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_exe: Option<String>,
    pub label: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Modifiers {
    pub l_shift: bool,
    pub r_shift: bool,
    pub l_ctrl: bool,
    pub r_ctrl: bool,
    pub l_alt: bool,
    pub r_alt: bool,
    pub l_win: bool,
    pub r_win: bool,
}

impl Modifiers {
    pub fn any_shift(&self) -> bool {
        self.l_shift || self.r_shift
    }
    pub fn any_ctrl(&self) -> bool {
        self.l_ctrl || self.r_ctrl
    }
    pub fn any_alt(&self) -> bool {
        self.l_alt || self.r_alt
    }
    pub fn any_win(&self) -> bool {
        self.l_win || self.r_win
    }
    pub fn any_active(&self) -> bool {
        self.any_shift() || self.any_ctrl() || self.any_alt() || self.any_win()
    }
}

// ---------------------------------------------------------------------------
// Global state for the hook callback
// ---------------------------------------------------------------------------

static KEY_SENDER: OnceLock<Mutex<Option<SyncSender<KeyEvent>>>> = OnceLock::new();
static MODIFIER_STATE: OnceLock<Mutex<Modifiers>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Starts the keyboard capture thread. Returns the join handle and the
/// thread ID (needed to post WM_QUIT for shutdown).
pub fn start(
    sender: SyncSender<KeyEvent>,
    _stop_flag: Arc<AtomicBool>,
) -> (JoinHandle<()>, u32) {
    // Store sender in global
    KEY_SENDER.get_or_init(|| Mutex::new(Some(sender)));
    MODIFIER_STATE.get_or_init(|| Mutex::new(Modifiers::default()));

    let (tid_tx, tid_rx) = std::sync::mpsc::channel::<u32>();

    let handle = thread::Builder::new()
        .name("keyboard".into())
        .spawn(move || {
            // Report our thread ID so the main thread can post WM_QUIT to us
            let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
            tid_tx.send(tid).ok();

            // Install the low-level keyboard hook
            let hook = unsafe {
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), None, 0)
            };
            let hook = match hook {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("Failed to install keyboard hook: {e}");
                    return;
                }
            };

            // Message loop (required for low-level hooks to work)
            let mut msg = MSG::default();
            loop {
                let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if ret.0 <= 0 {
                    break; // WM_QUIT or error
                }
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            unsafe { let _ = UnhookWindowsHookEx(hook); }

            // Clear the sender so the hook callback won't try to use it
            if let Some(sender_lock) = KEY_SENDER.get() {
                if let Ok(mut guard) = sender_lock.lock() {
                    *guard = None;
                }
            }
        })
        .expect("failed to spawn keyboard thread");

    let tid = tid_rx.recv().expect("keyboard thread did not report its ID");
    (handle, tid)
}

/// Posts WM_QUIT to the keyboard thread, causing it to exit its message loop.
pub fn stop(thread_id: u32) {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
            thread_id,
            WM_QUIT,
            windows::Win32::Foundation::WPARAM(0),
            windows::Win32::Foundation::LPARAM(0),
        );
    }
}

// ---------------------------------------------------------------------------
// Hook callback
// ---------------------------------------------------------------------------

unsafe extern "system" fn keyboard_hook_proc(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> LRESULT {
    if code >= 0 {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };

        let qpc = clock::now_ticks();

        let is_up = (kb.flags.0 & LLKHF_UP.0) != 0;
        let is_extended = (kb.flags.0 & LLKHF_EXTENDED.0) != 0;
        let is_injected = (kb.flags.0 & LLKHF_INJECTED.0) != 0;

        // Update modifier state
        if let Some(mods_lock) = MODIFIER_STATE.get() {
            if let Ok(mut mods) = mods_lock.lock() {
                update_modifier(&mut mods, kb.vkCode, is_up);
            }
        }

        // Read current modifier state
        let mods = MODIFIER_STATE
            .get()
            .and_then(|m| m.lock().ok())
            .map(|m| m.clone())
            .unwrap_or_default();

        let label = keymap::vk_to_label(kb.vkCode, kb.scanCode, is_extended);

        let foreground_exe = get_foreground_exe();

        let event = KeyEvent {
            qpc,
            vk: kb.vkCode,
            scan: kb.scanCode,
            extended: is_extended,
            injected: is_injected,
            down: !is_up,
            mods,
            foreground_exe,
            label,
        };

        // Non-blocking send
        if let Some(sender_lock) = KEY_SENDER.get() {
            if let Ok(guard) = sender_lock.lock() {
                if let Some(ref tx) = *guard {
                    let _ = tx.try_send(event);
                }
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Returns the filename of the foreground window's process, e.g. "notepad.exe".
/// Returns None if the foreground window can't be resolved (e.g. elevated process).
fn get_foreground_exe() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).ok()?;

        let _ = windows::Win32::Foundation::CloseHandle(handle);

        let path = String::from_utf16_lossy(&buf[..len as usize]);
        // Extract just the filename
        path.rsplit('\\').next().map(|s| s.to_string())
    }
}

fn update_modifier(mods: &mut Modifiers, vk: u32, is_up: bool) {
    let state = !is_up;
    match vk {
        x if x == VK_LSHIFT.0 as u32 => mods.l_shift = state,
        x if x == VK_RSHIFT.0 as u32 => mods.r_shift = state,
        x if x == VK_LCONTROL.0 as u32 => mods.l_ctrl = state,
        x if x == VK_RCONTROL.0 as u32 => mods.r_ctrl = state,
        x if x == VK_LMENU.0 as u32 => mods.l_alt = state,
        x if x == VK_RMENU.0 as u32 => mods.r_alt = state,
        x if x == VK_LWIN.0 as u32 => mods.l_win = state,
        x if x == VK_RWIN.0 as u32 => mods.r_win = state,
        _ => {}
    }
}
