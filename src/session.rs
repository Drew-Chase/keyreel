use crate::clock;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Session {
    name: String,
    base_path: PathBuf,
    epoch: i64,
    running: Arc<AtomicBool>,
}

impl Session {
    /// Creates a new session. The epoch is set to the current QPC tick.
    /// Output files are placed next to the running executable.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let exe = std::env::current_exe()?;
        let base_path = exe
            .parent()
            .ok_or("cannot determine exe directory")?
            .to_path_buf();

        let now = chrono_free_timestamp();
        let name = format!("session_{now}");

        Ok(Self {
            name,
            base_path,
            epoch: clock::now_ticks(),
            running: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn epoch(&self) -> i64 {
        self.epoch
    }

    pub fn mp4_path(&self) -> PathBuf {
        self.base_path.join(format!("{}.mp4", self.name))
    }

    pub fn srt_path(&self) -> PathBuf {
        self.base_path.join(format!("{}.srt", self.name))
    }

    pub fn jsonl_path(&self) -> PathBuf {
        self.base_path.join(format!("{}.jsonl", self.name))
    }

    pub fn start(&self) {
        self.running.store(true, Ordering::SeqCst);
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Returns a clone of the running flag for sharing with other threads.
    pub fn running_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }
}

/// Generates a `YYYYmmdd_HHMMSS` timestamp without pulling in the `chrono` crate.
/// Uses the Win32 `GetLocalTime` API.
fn chrono_free_timestamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond,
    )
}
