use crate::mux::VideoFrame;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::graphics_capture_picker::PickedGraphicsCaptureItem;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

/// Flags passed through the capture settings to our handler.
pub struct CaptureFlags {
    pub frame_tx: SyncSender<VideoFrame>,
    pub stop_flag: Arc<AtomicBool>,
}

pub struct Capture {
    frame_tx: SyncSender<VideoFrame>,
    stop_flag: Arc<AtomicBool>,
}

impl GraphicsCaptureApiHandler for Capture {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            frame_tx: ctx.flags.frame_tx,
            stop_flag: ctx.flags.stop_flag,
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
        let timestamp_100ns = timestamp.Duration;

        let width = frame.width();
        let height = frame.height();

        let frame_buffer = frame.buffer()?;
        let mut tmp = Vec::new();
        let bytes = frame_buffer.as_nopadding_buffer(&mut tmp);

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

/// Starts the video capture using a picker-selected item. Blocks until capture ends.
/// Run it on a dedicated thread.
pub fn start_capture(
    item: PickedGraphicsCaptureItem,
    frame_tx: SyncSender<VideoFrame>,
    stop_flag: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let flags = CaptureFlags { frame_tx, stop_flag };

    let settings = Settings::new(
        item,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Include,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );

    Capture::start(settings)?;
    Ok(())
}
