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
use windows_capture::graphics_capture_picker::{GraphicsCapturePicker, PickedGraphicsCaptureItem};
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
};

const HOTKEY_STOP: i32 = 1;

/// Global stop flag set by the console control handler.
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
    // Register console close / Ctrl+C handler
    unsafe {
        SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
    }

    // Check privilege level
    if !is_elevated() {
        output::warn(
            "Not running as administrator -- keystrokes from elevated processes won't be captured.",
        );
    }

    // Show the system capture picker
    output::info("Select a window or display to record...");
    let picked_item = match GraphicsCapturePicker::pick_item() {
        Ok(Some(item)) => item,
        Ok(None) => {
            output::info("No capture target selected. Exiting.");
            wait_for_enter();
            return;
        }
        Err(e) => {
            output::error(format!("Capture picker failed: {e}"));
            wait_for_enter();
            return;
        }
    };

    // Start recording
    let mut recording = match start_recording(picked_item) {
        Ok(rec) => rec,
        Err(e) => {
            output::error(format!("Failed to start recording: {e}"));
            wait_for_enter();
            return;
        }
    };

    // Register stop hotkey (Ctrl+Alt+Shift+Q)
    unsafe {
        RegisterHotKey(None, HOTKEY_STOP, MOD_CONTROL | MOD_ALT | MOD_SHIFT, 'Q' as u32).unwrap();
    }

    output::info(format!(
        "[REC] {} (press Ctrl+Alt+Shift+Q to stop)",
        recording.session.name()
    ));

    // Message loop
    let mut msg = MSG::default();
    loop {
        if CTRL_STOP.load(Ordering::SeqCst) {
            break;
        }

        drain_key_events(&mut recording);

        let has_msg = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
        if !has_msg.as_bool() {
            let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if ret.0 <= 0 {
                break;
            }
        }

        if msg.message == WM_HOTKEY {
            break;
        }

        unsafe { DispatchMessageW(&msg) };
    }

    // Save file paths before stop_recording takes ownership
    let mp4 = recording.session.mp4_path();
    let srt = recording.session.srt_path();
    let jsonl = recording.session.jsonl_path();

    stop_recording(recording);

    unsafe {
        let _ = UnregisterHotKey(None, HOTKEY_STOP);
    }

    output::info("Recording saved:");
    output::info(format!("  Video:     {}", mp4.display()));
    output::info(format!("  Subtitles: {}", srt.display()));
    output::info(format!("  Key log:   {}", jsonl.display()));

    wait_for_enter();
}

fn start_recording(
    item: PickedGraphicsCaptureItem,
) -> Result<Recording, Box<dyn std::error::Error>> {
    check_disk_space();

    let session = Session::new()?;
    let stop_flag = session.running_flag();
    let keylog = KeylogWriter::new(&session.jsonl_path())?;
    let subs = SubtitleWriter::new(&session.srt_path(), session.epoch(), None)?;

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

    // Mux thread config (resolution determined from first captured frame)
    let mux_config = MuxConfig {
        output_path: session.mp4_path(),
        fps: 30,
        video_bitrate: 8_000_000,
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

    // Start video capture thread.
    // PickedGraphicsCaptureItem contains an HWND (raw pointer) that isn't Send,
    // but we're transferring sole ownership to the capture thread — safe to move.
    let video_stop = Arc::clone(&stop_flag);
    let video_handle = {
        struct SendFn(Box<dyn FnOnce() + 'static>);
        // SAFETY: The closure only captures PickedGraphicsCaptureItem (whose HWND
        // is an opaque handle safe to use from any single thread), SyncSender,
        // and Arc — all of which we transfer sole ownership of.
        unsafe impl Send for SendFn {}
        impl SendFn {
            fn call(self) { (self.0)(); }
        }

        let f = SendFn(Box::new(move || {
            if let Err(e) = video::start_capture(item, frame_tx, video_stop) {
                eprintln!("Video capture error: {e}");
            }
        }));

        std::thread::Builder::new()
            .name("video".into())
            .spawn(move || f.call())
            .expect("failed to spawn video thread")
    };

    session.start();

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
            1
        }
        _ => 0,
    }
}

fn wait_for_enter() {
    output::info("Press Enter to exit...");
    let _ = std::io::stdin().read_line(&mut String::new());
}

// ---------------------------------------------------------------------------
// Disk space check
// ---------------------------------------------------------------------------

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
            "Low disk space: only {mb} MB free. Recording may fail or be truncated.",
        ));
    }
}

// ---------------------------------------------------------------------------
// Privilege detection
// ---------------------------------------------------------------------------

fn is_elevated() -> bool {
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret_len: u32 = 0;
        let size = size_of::<TOKEN_ELEVATION>() as u32;
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
