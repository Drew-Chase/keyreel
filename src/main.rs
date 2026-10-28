mod audio;
mod clock;
mod keyboard;
mod keylog;
mod keymap;
mod mux;
mod output;
mod session;
mod subtitles;
mod video;

use keylog::KeylogWriter;
use mux::{MuxConfig, VideoFrame};
use session::Session;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use subtitles::SubtitleWriter;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_SHIFT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PeekMessageW, MSG, PM_REMOVE, WM_HOTKEY,
};
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
};

const HOTKEY_TOGGLE: i32 = 1;
const HOTKEY_QUIT: i32 = 2;

/// Global stop flag set by the console control handler.
/// Shared with all worker threads via Arc clones.
static CTRL_STOP: AtomicBool = AtomicBool::new(false);

/// Active recording state, bundled for clean start/stop.
struct Recording {
    session: Session,
    stop_flag: Arc<AtomicBool>,
    keylog: KeylogWriter,
    subs: SubtitleWriter,
    kb_handle: JoinHandle<()>,
    kb_thread_id: u32,
    key_rx: mpsc::Receiver<keyboard::KeyEvent>,
    video_handle: JoinHandle<()>,
    mux_handle: JoinHandle<()>,
    loopback_handle: Option<JoinHandle<()>>,
    mic_handle: Option<JoinHandle<()>>,
}

fn main() {
    let args = parse_args();

    if args.help {
        print_help();
        return;
    }

    // Register console close / Ctrl+C handler
    unsafe {
        SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
    }

    // Register global hotkeys
    unsafe {
        RegisterHotKey(None, HOTKEY_TOGGLE, args.rec_hotkey.0, args.rec_hotkey.1).unwrap();
        RegisterHotKey(None, HOTKEY_QUIT, args.quit_hotkey.0, args.quit_hotkey.1).unwrap();
    }

    // Check privilege level
    if !is_elevated() {
        output::warn("Not running as administrator -- keystrokes from elevated processes won't be captured.");
    }

    output::info(format!(
        "KeyReel ready. Press {} to start/stop recording, {} to quit.",
        args.rec_hotkey_label, args.quit_hotkey_label
    ));

    let mut recording: Option<Recording> = None;

    // Message loop
    let mut msg = MSG::default();
    loop {
        // Check Ctrl+C / console close
        if CTRL_STOP.load(Ordering::SeqCst) {
            if let Some(rec) = recording.take() {
                stop_recording(rec);
            }
            break;
        }

        // Drain key events if recording
        if let Some(ref mut rec) = recording {
            drain_key_events(rec);
        }

        // Non-blocking peek so we can keep draining key events between messages.
        let has_msg = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
        if !has_msg.as_bool() {
            // Brief blocking wait for next message
            let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if ret.0 <= 0 {
                if let Some(rec) = recording.take() {
                    stop_recording(rec);
                }
                break;
            }
        }

        if msg.message == WM_HOTKEY {
            let id = msg.wParam.0 as i32;
            match id {
                HOTKEY_TOGGLE => {
                    if let Some(rec) = recording.take() {
                        stop_recording(rec);
                    } else {
                        match start_recording(&args) {
                            Ok(rec) => recording = Some(rec),
                            Err(e) => output::error(format!("Failed to start recording: {e}")),
                        }
                    }
                }
                HOTKEY_QUIT => {
                    if let Some(rec) = recording.take() {
                        stop_recording(rec);
                    }
                    break;
                }
                _ => {}
            }
        }

        unsafe { DispatchMessageW(&msg) };
    }

    // Unregister hotkeys
    unsafe {
        let _ = UnregisterHotKey(None, HOTKEY_TOGGLE);
        let _ = UnregisterHotKey(None, HOTKEY_QUIT);
    }

    output::info("KeyReel exited.");
}

fn start_recording(args: &Args) -> Result<Recording, Box<dyn std::error::Error>> {
    // Check available disk space
    check_disk_space();

    let session = Session::new()?;
    let stop_flag = session.running_flag();
    let keylog = KeylogWriter::new(&session.jsonl_path())?;
    let subs = SubtitleWriter::new(&session.srt_path(), session.epoch(), args.only_exe.clone())?;

    // Keyboard capture
    let (key_tx, key_rx) = mpsc::sync_channel::<keyboard::KeyEvent>(1024);
    let (kb_handle, kb_thread_id) = keyboard::start(key_tx, Arc::clone(&stop_flag));

    // Audio capture (loopback + mic) -- start before mux so we know the formats
    let (loopback_tx, loopback_rx) = mpsc::sync_channel::<audio::AudioPacket>(256);
    let (mic_tx, mic_rx) = mpsc::sync_channel::<audio::AudioPacket>(256);

    let loopback_result = audio::start_loopback(loopback_tx, Arc::clone(&stop_flag));
    let mic_result = audio::start_mic(mic_tx, Arc::clone(&stop_flag));

    let (loopback_handle, loopback_format) = match loopback_result {
        Some((h, f)) => (Some(h), Some(f)),
        None => {
            output::warn("System audio (loopback) not available -- recording without it");
            (None, None)
        }
    };
    let (mic_handle, mic_format) = match mic_result {
        Some((h, f)) => (Some(h), Some(f)),
        None => {
            output::warn("Microphone not available -- recording without it");
            (None, None)
        }
    };

    // Video -> Mux channel
    let (frame_tx, frame_rx) = mpsc::sync_channel::<VideoFrame>(4);

    // Mux thread config
    let mux_config = MuxConfig {
        output_path: session.mp4_path(),
        width: 1920,
        height: 1080,
        fps: 30,
        video_bitrate: 3_000_000,
        loopback_format,
        mic_format,
    };

    // Only pass rx channels to mux if the corresponding audio thread started
    let mux_loopback_rx = if loopback_handle.is_some() {
        Some(loopback_rx)
    } else {
        None
    };
    let mux_mic_rx = if mic_handle.is_some() {
        Some(mic_rx)
    } else {
        None
    };

    // Start mux thread
    let mux_stop = Arc::clone(&stop_flag);
    let mux_handle = std::thread::Builder::new()
        .name("mux".into())
        .spawn(move || {
            mux::run(mux_config, frame_rx, mux_loopback_rx, mux_mic_rx, mux_stop);
        })
        .expect("failed to spawn mux thread");

    // Start video capture thread
    let video_stop = Arc::clone(&stop_flag);
    let window_title = args.window.clone();
    let no_cursor = args.no_cursor;
    let epoch = session.epoch();
    let video_handle = std::thread::Builder::new()
        .name("video".into())
        .spawn(move || {
            if let Err(e) =
                video::start_capture(window_title.as_deref(), no_cursor, frame_tx, video_stop, epoch)
            {
                eprintln!("Video capture error: {e}");
            }
        })
        .expect("failed to spawn video thread");

    session.start();
    output::info(format!(
        "[REC] {} (all keystrokes and microphone are being recorded)",
        session.name()
    ));

    Ok(Recording {
        session,
        stop_flag,
        keylog,
        subs,
        kb_handle,
        kb_thread_id,
        key_rx,
        video_handle,
        mux_handle,
        loopback_handle,
        mic_handle,
    })
}

fn stop_recording(mut rec: Recording) {
    // Signal all threads to stop
    rec.stop_flag.store(true, Ordering::SeqCst);
    rec.session.stop();

    // Drain remaining key events
    drain_key_events(&mut rec);

    // Stop keyboard thread
    keyboard::stop(rec.kb_thread_id);
    let _ = rec.kb_handle.join();

    // Wait for audio threads
    if let Some(h) = rec.loopback_handle {
        let _ = h.join();
    }
    if let Some(h) = rec.mic_handle {
        let _ = h.join();
    }

    // Wait for video capture to finish (it will drop frame_tx, signaling mux)
    let _ = rec.video_handle.join();

    // Wait for mux to finalize the MP4
    let _ = rec.mux_handle.join();

    // Finalize subtitles
    if let Err(e) = rec.subs.finish() {
        output::error(format!("Failed to finalize subtitles: {e}"));
    }

    output::info(format!("[STOP] {}", rec.session.name()));
}

fn drain_key_events(rec: &mut Recording) {
    while let Ok(event) = rec.key_rx.try_recv() {
        if let Err(e) = rec.keylog.write(&event) {
            output::error(format!("Failed to write keylog: {e}"));
        }
        if let Err(e) = rec.subs.feed(&event) {
            output::error(format!("Failed to write subtitle: {e}"));
        }
    }
}

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
    match ctrl_type {
        x if x == CTRL_C_EVENT || x == CTRL_CLOSE_EVENT => {
            CTRL_STOP.store(true, Ordering::SeqCst);
            1 // TRUE
        }
        _ => 0, // FALSE
    }
}

// ---------------------------------------------------------------------------
// Disk space check
// ---------------------------------------------------------------------------

/// Warns if the output directory has less than 1 GB free.
fn check_disk_space() {
    let exe_dir = match std::env::current_exe() {
        Ok(p) => match p.parent() {
            Some(dir) => dir.to_path_buf(),
            None => return,
        },
        Err(_) => return,
    };

    let dir_wide: Vec<u16> = exe_dir
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut free_bytes: u64 = 0;
    let ok = unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            windows::core::PCWSTR(dir_wide.as_ptr()),
            Some(&mut free_bytes),
            None,
            None,
        )
    };

    if ok.is_ok() && free_bytes < 1_073_741_824 {
        let mb = free_bytes / (1024 * 1024);
        output::warn(format!(
            "Low disk space: only {mb} MB free. Recording may fail or be truncated."
        ));
    }
}

// ---------------------------------------------------------------------------
// Privilege detection
// ---------------------------------------------------------------------------

/// Returns true if the current process is running elevated (admin).
fn is_elevated() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret_len: u32 = 0;
        let size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            size,
            &mut ret_len,
        );

        let _ = windows::Win32::Foundation::CloseHandle(token);

        ok.is_ok() && elevation.TokenIsElevated != 0
    }
}

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

pub struct Args {
    help: bool,
    pub window: Option<String>,
    pub no_cursor: bool,
    pub only_exe: Option<String>,
    rec_hotkey: (windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS, u32),
    rec_hotkey_label: String,
    quit_hotkey: (windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS, u32),
    quit_hotkey_label: String,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut result = Args {
        help: false,
        window: None,
        no_cursor: false,
        only_exe: None,
        rec_hotkey: (MOD_CONTROL | MOD_ALT | MOD_SHIFT, 'R' as u32),
        rec_hotkey_label: "Ctrl+Alt+Shift+R".to_string(),
        quit_hotkey: (MOD_CONTROL | MOD_ALT | MOD_SHIFT, 'Q' as u32),
        quit_hotkey_label: "Ctrl+Alt+Shift+Q".to_string(),
    };

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => result.help = true,
            "--window" => {
                i += 1;
                if i < args.len() {
                    result.window = Some(args[i].clone());
                }
            }
            "--no-cursor" => result.no_cursor = true,
            "--only-exe" => {
                i += 1;
                if i < args.len() {
                    result.only_exe = Some(args[i].clone());
                }
            }
            "--rec-hotkey" => {
                i += 1;
                if i < args.len() {
                    if let Some(parsed) = parse_hotkey_combo(&args[i]) {
                        result.rec_hotkey = parsed;
                        result.rec_hotkey_label = args[i].clone();
                    } else {
                        output::error(format!("Invalid hotkey combo: {}", args[i]));
                    }
                }
            }
            "--quit-hotkey" => {
                i += 1;
                if i < args.len() {
                    if let Some(parsed) = parse_hotkey_combo(&args[i]) {
                        result.quit_hotkey = parsed;
                        result.quit_hotkey_label = args[i].clone();
                    } else {
                        output::error(format!("Invalid hotkey combo: {}", args[i]));
                    }
                }
            }
            other => {
                output::warn(format!("Unknown argument: {other}"));
            }
        }
        i += 1;
    }

    result
}

/// Parses a hotkey combo string like "Ctrl+Alt+Shift+R" into (modifiers, vk).
fn parse_hotkey_combo(
    s: &str,
) -> Option<(windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS, u32)> {
    use windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS;

    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut vk: Option<u32> = None;

    for part in s.split('+') {
        let part = part.trim();
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            _ => {
                if part.len() == 1 {
                    vk = Some(part.to_ascii_uppercase().as_bytes()[0] as u32);
                } else if let Some(stripped) =
                    part.strip_prefix('F').or_else(|| part.strip_prefix('f'))
                {
                    if let Ok(n) = stripped.parse::<u32>() {
                        if (1..=24).contains(&n) {
                            vk = Some(0x70 + n - 1);
                        }
                    }
                }
            }
        }
    }

    vk.map(|k| (mods, k))
}

fn print_help() {
    let help = r#"KeyReel - Portable Windows screen recorder with keystroke logging

USAGE:
    key_reel.exe [OPTIONS]

OPTIONS:
    --help, -h              Print this help message
    --window <SUBSTRING>    Capture a specific window (title substring match)
                            Default: primary monitor
    --no-cursor             Hide the cursor in the recording
    --only-exe <NAME>       Filter subtitles to only show keys when this
                            executable is in the foreground (e.g. dosbox.exe)
    --rec-hotkey <COMBO>    Hotkey to toggle recording (default: Ctrl+Alt+Shift+R)
    --quit-hotkey <COMBO>   Hotkey to quit (default: Ctrl+Alt+Shift+Q)

HOTKEY FORMAT:
    Modifier+...+Key  (e.g. Ctrl+Alt+Shift+R, Ctrl+F12)
    Modifiers: Ctrl, Alt, Shift
    Keys: A-Z, 0-9, F1-F24

OUTPUT FILES:
    session_<timestamp>.mp4    Video + audio recording
    session_<timestamp>.srt    Keystroke subtitles
    session_<timestamp>.jsonl  Raw keystroke log (one JSON object per line)

    All files are written next to the executable.

EXAMPLES:
    key_reel.exe
    key_reel.exe --window "DOSBox" --only-exe dosbox.exe
    key_reel.exe --rec-hotkey Ctrl+F9 --quit-hotkey Ctrl+F10
"#;
    print!("{help}");
}
