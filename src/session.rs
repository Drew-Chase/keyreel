use crate::clock;
use std::path::PathBuf;

pub struct Session {
    name: String,
    base_path: PathBuf,
    epoch: i64,
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
