use crate::mux::VideoFrame;
use crate::output;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

/// Flags passed through the capture settings to our handler.
pub struct CaptureFlags {
    pub frame_tx: SyncSender<VideoFrame>,
    pub stop_flag: Arc<AtomicBool>,
    pub session_epoch: i64,
}

pub struct Capture {
    frame_tx: SyncSender<VideoFrame>,
    stop_flag: Arc<AtomicBool>,
    session_epoch: i64,
}

impl GraphicsCaptureApiHandler for Capture {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            frame_tx: ctx.flags.frame_tx,
            stop_flag: ctx.flags.stop_flag,
            session_epoch: ctx.flags.session_epoch,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.stop_flag.load(Ordering::Relaxed) {
            capture_control.stop();
            return Ok(());
        }

        let timestamp = frame.timestamp()?;
        // TimeSpan.Duration is in 100ns units (system relative time from QPC)
        let timestamp_100ns = timestamp.Duration;

        let width = frame.width();
        let height = frame.height();

        let frame_buffer = frame.buffer()?;
        let mut tmp = Vec::new();
        let bytes = frame_buffer.as_nopadding_buffer(&mut tmp);

        // Send the raw BGRA data to the mux thread
        let video_frame = VideoFrame {
            data: bytes.to_vec(),
            width,
            height,
            timestamp_100ns,
        };

        // If the channel is full, drop the frame rather than blocking the capture thread
        let _ = self.frame_tx.try_send(video_frame);

        Ok(())
    }
}

/// Starts the video capture. This function blocks until capture ends.
/// Run it on a dedicated thread.
pub fn start_capture(
    window_title: Option<&str>,
    no_cursor: bool,
    frame_tx: SyncSender<VideoFrame>,
    stop_flag: Arc<AtomicBool>,
    session_epoch: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cursor = if no_cursor {
        CursorCaptureSettings::WithoutCursor
    } else {
        CursorCaptureSettings::WithCursor
    };

    let flags = CaptureFlags {
        frame_tx,
        stop_flag,
        session_epoch,
    };

    if let Some(title) = window_title {
        // Capture a specific window
        match Window::from_contains_name(title) {
            Ok(window) => {
                let settings = Settings::new(
                    window,
                    cursor,
                    DrawBorderSettings::WithoutBorder,
                    SecondaryWindowSettings::Include,
                    MinimumUpdateIntervalSettings::Default,
                    DirtyRegionSettings::Default,
                    ColorFormat::Bgra8,
                    flags,
                );
                Capture::start(settings)?;
            }
            Err(e) => {
                output::error(format!("Could not find window \"{title}\": {e}"));
                return Err(format!("Window not found: {title}").into());
            }
        }
    } else {
        // Capture primary monitor
        match Monitor::primary() {
            Ok(monitor) => {
                let settings = Settings::new(
                    monitor,
                    cursor,
                    DrawBorderSettings::WithoutBorder,
                    SecondaryWindowSettings::Include,
                    MinimumUpdateIntervalSettings::Default,
                    DirtyRegionSettings::Default,
                    ColorFormat::Bgra8,
                    flags,
                );
                Capture::start(settings)?;
            }
            Err(e) => {
                output::error(format!("Could not get primary monitor: {e}"));
                return Err(format!("Primary monitor error: {e}").into());
            }
        }
    }

    Ok(())
}
