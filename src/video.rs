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

/// Maximum plausible delay between a frame's capture and its delivery to us.
const MAX_DELIVERY_LAG_100NS: i64 = 100 * 10_000; // 100 ms

/// Flags passed through the capture settings to our handler.
pub struct CaptureFlags {
    pub frame_tx: SyncSender<VideoFrame>,
    pub stop_flag: Arc<AtomicBool>,
    pub session_epoch: i64,
}

pub struct Capture {
    frame_tx: SyncSender<VideoFrame>,
    stop_flag: Arc<AtomicBool>,
    session_epoch: i64, // raw QPC ticks
    /// Calibration offset computed on the first frame to map WGC timestamps
    /// (which use a different epoch than QPC) into QPC-relative 100ns values.
    wgc_to_qpc_offset: Option<i64>,
}

impl GraphicsCaptureApiHandler for Capture {
    type Flags = CaptureFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            frame_tx: ctx.flags.frame_tx,
            stop_flag: ctx.flags.stop_flag,
            session_epoch: ctx.flags.session_epoch,
            wgc_to_qpc_offset: None,
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

        // WGC's SystemRelativeTime uses a different epoch than QPC, so we
        // calibrate on the first frame: compute the offset between QPC-relative
        // session time and the WGC timestamp, then apply it to all frames.
        // This anchors WGC to the same QPC clock that audio and subtitles use,
        // while preserving WGC's precise inter-frame deltas (no callback jitter).
        // A capture can't happen after "now", and delivery shouldn't lag by more
        // than MAX_DELIVERY_LAG; if either is violated the clocks have drifted
        // apart, so recalibrate rather than let video slide out of sync.
        let wgc_ts = frame.timestamp()?.Duration;
        let qpc_now_100ns =
            crate::clock::ticks_to_100ns(crate::clock::now_ticks() - self.session_epoch);

        let mut timestamp_100ns = wgc_ts + *self.wgc_to_qpc_offset.get_or_insert(qpc_now_100ns - wgc_ts);
        if timestamp_100ns > qpc_now_100ns || qpc_now_100ns - timestamp_100ns > MAX_DELIVERY_LAG_100NS {
            self.wgc_to_qpc_offset = Some(qpc_now_100ns - wgc_ts);
            timestamp_100ns = qpc_now_100ns;
        }

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
    session_epoch: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let flags = CaptureFlags { frame_tx, stop_flag, session_epoch };

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
