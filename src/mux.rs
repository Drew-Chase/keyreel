use crate::audio::{AudioFormat, AudioPacket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use windows::core::{GUID, HSTRING, Interface, PCWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// MF_SD_STREAM_NAME GUID — not exposed in windows 0.62, defined manually.
/// {4F1B4D83-6322-4E7E-A141-113A03D4C1A3}
const MF_SD_STREAM_NAME: GUID = GUID::from_values(
    0x4F1B4D83, 0x6322, 0x4E7E, [0xA1, 0x41, 0x11, 0x3A, 0x03, 0xD4, 0xC1, 0xA3],
);

/// A video frame received from the capture thread.
pub struct VideoFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub timestamp_100ns: i64,
}

/// Configuration for the mux thread.
pub struct MuxConfig {
    pub output_path: std::path::PathBuf,
    pub fps: u32,
    pub video_bitrate: u32,
    pub loopback_format: Option<AudioFormat>,
    pub mic_format: Option<AudioFormat>,
    /// QPC tick at session start; all incoming timestamps are relative to it.
    pub session_epoch: i64,
}

/// How long to wait for a late video frame before repeating the previous one.
const VIDEO_LATENESS_MARGIN_100NS: i64 = 150 * 10_000; // 150 ms

/// Audio timestamp error tolerated before inserting silence / trimming samples.
const AUDIO_DRIFT_TOLERANCE_100NS: i64 = 20 * 10_000; // 20 ms

/// Session-relative "now" in 100ns units.
fn now_100ns(session_epoch: i64) -> i64 {
    crate::clock::ticks_to_100ns(crate::clock::now_ticks() - session_epoch)
}

/// Constant-frame-rate video track. Output slot `k` lives at `k * frame_dur`
/// (session-relative, so slot 0 is session start) and shows the latest frame
/// captured at or before that time. Frames arriving faster than the output
/// rate are dropped; a static screen repeats the previous frame. This keeps
/// the video track's timeline locked to the capture clock.
struct VideoTrack {
    stream: u32,
    frame_dur: i64,
    next_slot: i64,
    last: Option<Vec<u8>>,
}

impl VideoTrack {
    fn new(stream: u32, fps: u32) -> Self {
        Self { stream, frame_dur: 10_000_000 / fps as i64, next_slot: 0, last: None }
    }

    /// Emits `last` for every slot strictly before `t`.
    fn fill_until(&mut self, writer: &IMFSinkWriter, t: i64) -> windows::core::Result<()> {
        if let Some(ref data) = self.last {
            while self.next_slot < t {
                write_sample(writer, self.stream, data, self.next_slot, self.frame_dur)?;
                self.next_slot += self.frame_dur;
            }
        }
        Ok(())
    }

    fn push(&mut self, writer: &IMFSinkWriter, data: Vec<u8>, ts: i64) -> windows::core::Result<()> {
        if self.last.is_none() {
            // Pad from session start with the first frame so every track starts at 0.
            self.last = Some(data);
            self.fill_until(writer, ts)
        } else {
            self.fill_until(writer, ts)?;
            self.last = Some(data);
            Ok(())
        }
    }

    /// End time of everything written so far.
    fn end(&self) -> i64 {
        self.next_slot
    }
}

/// Contiguous audio track. MP4/AAC stores audio as an unbroken run of samples,
/// so timestamp gaps are not preserved — they'd just shorten the track. Instead
/// we write samples back-to-back and compare each packet's capture timestamp
/// with our sample position: gaps (loopback goes quiet, dropped packets) are
/// filled with silence and overlap (device clock running fast) is trimmed.
struct AudioTrack {
    stream: u32,
    sample_rate: u32,
    block_align: u32,
    frames_written: u64,
    /// Packets received before the sink writer existed.
    pending: Vec<AudioPacket>,
}

impl AudioTrack {
    /// `stream` is assigned once the sink writer is created.
    fn new(fmt: &AudioFormat) -> Self {
        Self {
            stream: 0,
            sample_rate: fmt.sample_rate,
            block_align: fmt.block_align,
            frames_written: 0,
            pending: Vec::new(),
        }
    }

    fn frames_to_100ns(&self, frames: u64) -> i64 {
        (frames as i128 * 10_000_000 / self.sample_rate as i128) as i64
    }

    fn frames_for(&self, dur_100ns: i64) -> u64 {
        (dur_100ns.max(0) as i128 * self.sample_rate as i128 / 10_000_000) as u64
    }

    /// Session-relative time of the next sample to be written.
    fn position(&self) -> i64 {
        self.frames_to_100ns(self.frames_written)
    }

    fn write_frames(&mut self, writer: &IMFSinkWriter, data: &[u8]) -> windows::core::Result<()> {
        let frames = (data.len() / self.block_align as usize) as u64;
        if frames == 0 {
            return Ok(());
        }
        let ts = self.position();
        let dur = self.frames_to_100ns(self.frames_written + frames) - ts;
        write_sample(writer, self.stream, data, ts, dur)?;
        self.frames_written += frames;
        Ok(())
    }

    fn write_silence(&mut self, writer: &IMFSinkWriter, dur_100ns: i64) -> windows::core::Result<()> {
        let mut remaining = self.frames_for(dur_100ns);
        let chunk_max = self.sample_rate as u64 / 10; // 100 ms chunks
        while remaining > 0 {
            let n = remaining.min(chunk_max);
            let silence = vec![0u8; n as usize * self.block_align as usize];
            self.write_frames(writer, &silence)?;
            remaining -= n;
        }
        Ok(())
    }

    fn push(&mut self, writer: &IMFSinkWriter, pkt: &AudioPacket) -> windows::core::Result<()> {
        let drift = pkt.timestamp_100ns - self.position();
        let mut data = &pkt.data[..];
        if drift > AUDIO_DRIFT_TOLERANCE_100NS {
            self.write_silence(writer, drift)?;
        } else if drift < -AUDIO_DRIFT_TOLERANCE_100NS {
            let skip = self.frames_for(-drift) as usize * self.block_align as usize;
            if skip >= data.len() {
                return Ok(());
            }
            data = &data[skip..];
        }
        self.write_frames(writer, data)
    }

    /// Receives packets from the channel; buffers them until the writer exists.
    fn drain(&mut self, rx: &Receiver<AudioPacket>, writer: Option<&IMFSinkWriter>) -> windows::core::Result<()> {
        while let Ok(pkt) = rx.try_recv() {
            self.pending.push(pkt);
        }
        if let Some(writer) = writer {
            for pkt in std::mem::take(&mut self.pending) {
                self.push(writer, &pkt)?;
            }
        }
        Ok(())
    }
}

/// Packs two u32 values into one u64 (emulates MFSetAttributeSize / MFSetAttributeRatio).
fn pack_u32_pair(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | (low as u64)
}

/// Stream indices for the sink writer.
struct StreamIndices {
    video: u32,
    loopback: Option<u32>,
    mic: Option<u32>,
}

/// Sets up the IMFSinkWriter with video and optional audio streams.
/// Width/height come from the first captured frame so they always match.
fn create_sink_writer(config: &MuxConfig, width: u32, height: u32) -> windows::core::Result<(IMFSinkWriter, StreamIndices)> {
    let mut attrs: Option<IMFAttributes> = None;
    unsafe { MFCreateAttributes(&mut attrs, 2)? };
    let attrs = attrs.unwrap();
    unsafe {
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
    }

    let path_wide: Vec<u16> = config
        .output_path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let writer = unsafe { MFCreateSinkWriterFromURL(PCWSTR(path_wide.as_ptr()), None, &attrs)? };

    // --- Video output type (H.264) ---
    let out_type = unsafe { MFCreateMediaType()? };
    unsafe {
        out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        out_type.SetUINT32(&MF_MT_AVG_BITRATE, config.video_bitrate)?;
        out_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(width, height))?;
        out_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(config.fps, 1))?;
        out_type.SetUINT32(&MF_MT_INTERLACE_MODE, 2)?; // Progressive
    }
    let video_idx = unsafe { writer.AddStream(&out_type)? };

    // --- Video input type (BGRA) ---
    let in_type = unsafe { MFCreateMediaType()? };
    unsafe {
        in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
        in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(width, height))?;
        in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(config.fps, 1))?;
        in_type.SetUINT32(&MF_MT_INTERLACE_MODE, 2)?;
    }
    unsafe { writer.SetInputMediaType(video_idx, &in_type, None)? };

    // --- Audio streams ---
    let loopback_idx = if let Some(ref fmt) = config.loopback_format {
        Some(add_audio_stream(&writer, fmt, "Desktop")?)
    } else {
        None
    };

    let mic_idx = if let Some(ref fmt) = config.mic_format {
        Some(add_audio_stream(&writer, fmt, "Microphone")?)
    } else {
        None
    };

    unsafe { writer.BeginWriting()? };

    Ok((
        writer,
        StreamIndices {
            video: video_idx,
            loopback: loopback_idx,
            mic: mic_idx,
        },
    ))
}

/// Adds an AAC audio output stream with PCM input to the sink writer.
fn add_audio_stream(
    writer: &IMFSinkWriter,
    fmt: &AudioFormat,
    name: &str,
) -> windows::core::Result<u32> {
    // Output type: AAC
    let out_type = unsafe { MFCreateMediaType()? };
    unsafe {
        out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        out_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
        out_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, fmt.sample_rate)?;
        out_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, fmt.channels as u32)?;
        out_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        out_type.SetUINT32(&MF_MT_AVG_BITRATE, 192_000)?; // 192 kbps
    }

    let stream_idx = unsafe { writer.AddStream(&out_type)? };

    // Try to label the track ("Desktop" / "Microphone") via the underlying stream
    // descriptor. If the MPEG-4 sink doesn't support this, we silently skip.
    let _ = set_stream_name(writer, stream_idx, name);

    // Input type: PCM (or float, depending on WASAPI format)
    let in_type = unsafe { MFCreateMediaType()? };
    unsafe {
        in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        // WASAPI with autoconvert gives us float32 typically, but the AAC encoder
        // accepts both PCM and float. Use the actual format from WASAPI.
        if fmt.bits_per_sample == 32 {
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)?;
        } else {
            in_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        }
        in_type.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, fmt.sample_rate)?;
        in_type.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, fmt.channels as u32)?;
        in_type.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, fmt.bits_per_sample as u32)?;
        in_type.SetUINT32(
            &MF_MT_AUDIO_BLOCK_ALIGNMENT,
            fmt.block_align,
        )?;
        in_type.SetUINT32(
            &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            fmt.sample_rate * fmt.block_align,
        )?;
    }

    unsafe { writer.SetInputMediaType(stream_idx, &in_type, None)? };

    Ok(stream_idx)
}

/// Attempts to set a human-readable name on a stream via its stream descriptor.
fn set_stream_name(writer: &IMFSinkWriter, stream_idx: u32, name: &str) -> windows::core::Result<()> {
    unsafe {
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        writer.GetServiceForStream(
            stream_idx,
            &GUID::zeroed(),
            &IMFAttributes::IID,
            &mut ptr,
        )?;
        let attrs: IMFAttributes = IMFAttributes::from_raw(ptr);
        attrs.SetString(&MF_SD_STREAM_NAME, &HSTRING::from(name))?;
    }
    Ok(())
}

/// Writes a sample (video or audio) to the sink writer.
fn write_sample(
    writer: &IMFSinkWriter,
    stream_idx: u32,
    data: &[u8],
    timestamp_100ns: i64,
    duration_100ns: i64,
) -> windows::core::Result<()> {
    let buffer = unsafe { MFCreateMemoryBuffer(data.len() as u32)? };

    let mut buf_ptr: *mut u8 = std::ptr::null_mut();
    let mut max_len: u32 = 0;
    let mut cur_len: u32 = 0;
    unsafe {
        buffer.Lock(&mut buf_ptr, Some(&mut max_len), Some(&mut cur_len))?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), buf_ptr, data.len());
        buffer.SetCurrentLength(data.len() as u32)?;
        buffer.Unlock()?;
    }

    let sample = unsafe { MFCreateSample()? };
    unsafe {
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(timestamp_100ns)?;
        sample.SetSampleDuration(duration_100ns)?;
    }

    unsafe { writer.WriteSample(stream_idx, &sample)? };

    Ok(())
}

/// Runs the mux thread.
pub fn run(
    config: MuxConfig,
    video_rx: Receiver<VideoFrame>,
    loopback_rx: Option<Receiver<AudioPacket>>,
    mic_rx: Option<Receiver<AudioPacket>>,
    stop_flag: Arc<AtomicBool>,
) {
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() {
        crate::output::error(format!("mux: CoInitializeEx failed: {hr:?}"));
        return;
    }
    if let Err(e) = unsafe { MFStartup(MF_VERSION, 0) } {
        crate::output::error(format!("mux: MFStartup failed: {e}"));
        return;
    }

    let result = run_inner(&config, &video_rx, &loopback_rx, &mic_rx, &stop_flag);
    if let Err(e) = result {
        crate::output::error(format!("mux: error: {e}"));
    }

    unsafe {
        let _ = MFShutdown();
        windows::Win32::System::Com::CoUninitialize();
    }
}

fn run_inner(
    config: &MuxConfig,
    video_rx: &Receiver<VideoFrame>,
    loopback_rx: &Option<Receiver<AudioPacket>>,
    mic_rx: &Option<Receiver<AudioPacket>>,
    stop_flag: &Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let recv_timeout = std::time::Duration::from_millis(1000 / config.fps as u64);

    // Sink writer is created lazily on the first frame so we know the real resolution.
    let mut writer: Option<IMFSinkWriter> = None;
    let mut video: Option<VideoTrack> = None;
    let mut loopback = config.loopback_format.as_ref().map(AudioTrack::new);
    let mut mic = config.mic_format.as_ref().map(AudioTrack::new);

    // Latest capture timestamp seen, so the end of the video covers it.
    let mut last_frame_ts: i64 = 0;

    let drain_audio = |writer: Option<&IMFSinkWriter>,
                       loopback: &mut Option<AudioTrack>,
                       mic: &mut Option<AudioTrack>|
     -> windows::core::Result<()> {
        if let (Some(rx), Some(track)) = (loopback_rx, loopback.as_mut()) {
            track.drain(rx, writer)?;
        }
        if let (Some(rx), Some(track)) = (mic_rx, mic.as_mut()) {
            track.drain(rx, writer)?;
        }
        Ok(())
    };

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        drain_audio(writer.as_ref(), &mut loopback, &mut mic)?;

        match video_rx.recv_timeout(recv_timeout) {
            Ok(frame) => {
                if writer.is_none() {
                    let (w, h) = (frame.width, frame.height);
                    eprintln!("mux: first frame received, resolution {w}x{h}");
                    let (wr, indices) = create_sink_writer(config, w, h)?;
                    video = Some(VideoTrack::new(indices.video, config.fps));
                    if let (Some(t), Some(i)) = (loopback.as_mut(), indices.loopback) {
                        t.stream = i;
                    }
                    if let (Some(t), Some(i)) = (mic.as_mut(), indices.mic) {
                        t.stream = i;
                    }
                    writer = Some(wr);
                }
                last_frame_ts = last_frame_ts.max(frame.timestamp_100ns);
                video
                    .as_mut()
                    .unwrap()
                    .push(writer.as_ref().unwrap(), frame.data, frame.timestamp_100ns)?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        // Static screen: WGC delivers nothing, so keep repeating the last frame
        // up to (now - margin), leaving room for frames still in flight.
        if let (Some(wr), Some(v)) = (writer.as_ref(), video.as_mut()) {
            v.fill_until(wr, now_100ns(config.session_epoch) - VIDEO_LATENESS_MARGIN_100NS)?;
        }
    }

    let Some(writer) = writer else {
        return Ok(());
    };
    let video = video.as_mut().unwrap();

    // Extend video to the stop time (or past the last frame for synthetic input).
    let end = now_100ns(config.session_epoch).max(last_frame_ts + video.frame_dur);
    video.fill_until(&writer, end)?;
    let end = video.end();

    // Flush remaining audio, then pad each audio track with silence to the same
    // end time so all tracks have identical length.
    drain_audio(Some(&writer), &mut loopback, &mut mic)?;
    for track in [loopback.as_mut(), mic.as_mut()].into_iter().flatten() {
        let pos = track.position();
        if pos < end {
            track.write_silence(&writer, end - pos)?;
        }
    }

    unsafe { writer.Finalize()? };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioFormat, AudioPacket};
    use std::sync::mpsc;

    /// Helper: a solid-color BGRA frame (blue).
    fn fake_frame(width: u32, height: u32, timestamp_100ns: i64) -> VideoFrame {
        let pixel_count = (width * height) as usize;
        let mut data = Vec::with_capacity(pixel_count * 4);
        for _ in 0..pixel_count {
            data.extend_from_slice(&[255, 0, 0, 255]); // BGRA blue
        }
        VideoFrame { data, width, height, timestamp_100ns }
    }

    /// Helper: a silent audio packet (zeroes).
    fn fake_audio(timestamp_100ns: i64, duration_100ns: i64, block_align: u32, sample_rate: u32) -> AudioPacket {
        let frames = (duration_100ns as u64 * sample_rate as u64 / 10_000_000) as usize;
        let data = vec![0u8; frames * block_align as usize];
        AudioPacket { data, timestamp_100ns }
    }

    /// Reads an MP4 back and returns, per stream index, (sample count, end time in 100ns).
    fn read_stream_extents(
        path: &std::path::Path,
        stream_count: usize,
    ) -> std::collections::BTreeMap<u32, (u32, i64)> {
        let mut extents = std::collections::BTreeMap::new();
        let mut ended = std::collections::HashSet::new();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, 0).unwrap();
            let url = HSTRING::from(path.to_string_lossy().as_ref());
            let reader = MFCreateSourceReaderFromURL(&url, None).unwrap();
            // Select every stream (video + both audio tracks).
            reader.SetStreamSelection(0xFFFF_FFFE, true).unwrap(); // MF_SOURCE_READER_ALL_STREAMS
            loop {
                let mut stream = 0u32;
                let mut flags = 0u32;
                let mut ts = 0i64;
                let mut sample: Option<IMFSample> = None;
                reader
                    .ReadSample(
                        MF_SOURCE_READER_ANY_STREAM.0 as u32,
                        0,
                        Some(&mut stream),
                        Some(&mut flags),
                        Some(&mut ts),
                        Some(&mut sample),
                    )
                    .unwrap();
                if let Some(s) = sample {
                    let dur = s.GetSampleDuration().unwrap_or(0);
                    let e = extents.entry(stream).or_insert((0u32, 0i64));
                    e.0 += 1;
                    e.1 = e.1.max(ts + dur);
                }
                // ANY_STREAM reports end-of-stream once per stream.
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    ended.insert(stream);
                    if ended.len() >= stream_count {
                        break;
                    }
                }
            }
            drop(reader);
            let _ = MFShutdown();
        }
        extents
    }

    /// Returns the reader's stream index of the (first) video stream in an MP4.
    fn find_video_stream(path: &std::path::Path) -> u32 {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, 0).unwrap();
            let url = HSTRING::from(path.to_string_lossy().as_ref());
            let reader = MFCreateSourceReaderFromURL(&url, None).unwrap();
            let mut i = 0;
            let idx = loop {
                let mt = reader.GetNativeMediaType(i, 0).expect("no video stream found");
                if mt.GetMajorType().unwrap() == MFMediaType_Video {
                    break i;
                }
                i += 1;
            };
            drop(reader);
            let _ = MFShutdown();
            idx
        }
    }

    /// Regression test for A/V drift: WGC delivering 60 fps must not stretch the
    /// 30 fps video, and loopback going quiet (no packets) must not shorten the
    /// audio. Every track must span the same real-time duration.
    #[test]
    fn test_mux_tracks_stay_in_sync() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_mux_sync.mp4");
        let _ = std::fs::remove_file(&path);

        let audio_fmt = audio_format_48k_stereo_f32();
        let config = MuxConfig {
            output_path: path.clone(),
            fps: 30,
            video_bitrate: 1_000_000,
            loopback_format: Some(audio_fmt.clone()),
            mic_format: Some(audio_fmt.clone()),
            session_epoch: crate::clock::now_ticks(),
        };

        let (frame_tx, frame_rx) = mpsc::sync_channel::<VideoFrame>(256);
        let (loopback_tx, loopback_rx) = mpsc::sync_channel::<AudioPacket>(512);
        let (mic_tx, mic_rx) = mpsc::sync_channel::<AudioPacket>(512);
        let stop = Arc::new(AtomicBool::new(false));

        const SECONDS: i64 = 3;
        // 3 s of video at 60 fps (twice the output rate).
        let frame_dur_60 = 10_000_000i64 / 60;
        for i in 0..(SECONDS * 60) {
            frame_tx.send(fake_frame(320, 240, i * frame_dur_60)).unwrap();
        }

        // 10 ms audio packets. Loopback is missing 1.0–2.0 s entirely (silence).
        let chunk = 100_000i64;
        for i in 0..(SECONDS * 100) {
            let ts = i * chunk;
            mic_tx.send(fake_audio(ts, chunk, audio_fmt.block_align, audio_fmt.sample_rate)).unwrap();
            if !(10_000_000..20_000_000).contains(&ts) {
                loopback_tx
                    .send(fake_audio(ts, chunk, audio_fmt.block_align, audio_fmt.sample_rate))
                    .unwrap();
            }
        }

        drop(frame_tx);
        drop(loopback_tx);
        drop(mic_tx);

        run(config, frame_rx, Some(loopback_rx), Some(mic_rx), stop);
        assert!(path.exists(), "MP4 file should exist");

        let extents = read_stream_extents(&path, 3);
        assert_eq!(extents.len(), 3, "expected video + 2 audio streams, got {extents:?}");

        // Tolerance covers one video frame (33 ms) plus AAC's 1024-sample
        // frame granularity (21 ms at 48 kHz).
        let expected = SECONDS * 10_000_000;
        let tolerance = 60 * 10_000;
        for (stream, (_, end)) in &extents {
            assert!(
                (end - expected).abs() <= tolerance,
                "stream {stream} ends at {:.3}s, expected {:.3}s: {extents:?}",
                *end as f64 / 1e7,
                expected as f64 / 1e7,
            );
        }

        // 60 fps input resampled to 30 fps: ~90 frames, not 180.
        let video_stream = find_video_stream(&path);
        let (video_frames, _) = extents[&video_stream];
        assert!((89..=92).contains(&video_frames), "expected ~90 video frames, got {video_frames}");

        std::fs::remove_file(&path).ok();
    }

    fn audio_format_48k_stereo_f32() -> AudioFormat {
        AudioFormat {
            sample_rate: 48000,
            channels: 2,
            bits_per_sample: 32,
            block_align: 8, // 2ch * 4 bytes
        }
    }

    #[test]
    fn test_mux_video_only() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_mux_video_only.mp4");
        let _ = std::fs::remove_file(&path);

        let config = MuxConfig {
            output_path: path.clone(),
            fps: 30,
            video_bitrate: 1_000_000,
            loopback_format: None,
            mic_format: None,
            session_epoch: crate::clock::now_ticks(),
        };

        // Buffer must hold all frames since run() hasn't started consuming yet
        let (frame_tx, frame_rx) = mpsc::sync_channel::<VideoFrame>(32);
        let stop = Arc::new(AtomicBool::new(false));

        // Send 30 frames (1 second at 30fps), timestamps in 100ns units
        let frame_dur = 10_000_000i64 / 30; // ~333333
        for i in 0..30 {
            frame_tx.send(fake_frame(320, 240, i * frame_dur)).unwrap();
        }
        drop(frame_tx); // signal end

        run(config, frame_rx, None, None, stop);

        assert!(path.exists(), "MP4 file should exist");
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 1000, "MP4 should be larger than 1KB, got {size}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_mux_video_plus_audio() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_mux_video_audio.mp4");
        let _ = std::fs::remove_file(&path);

        let audio_fmt = audio_format_48k_stereo_f32();
        let config = MuxConfig {
            output_path: path.clone(),
            fps: 30,
            video_bitrate: 1_000_000,
            loopback_format: Some(audio_fmt.clone()),
            mic_format: Some(audio_fmt.clone()),
            session_epoch: crate::clock::now_ticks(),
        };

        // Buffer must hold all items since run() hasn't started consuming yet
        let (frame_tx, frame_rx) = mpsc::sync_channel::<VideoFrame>(32);
        let (loopback_tx, loopback_rx) = mpsc::sync_channel::<AudioPacket>(64);
        let (mic_tx, mic_rx) = mpsc::sync_channel::<AudioPacket>(64);
        let stop = Arc::new(AtomicBool::new(false));

        let frame_dur = 10_000_000i64 / 30;
        // Send 30 video frames (1 second)
        for i in 0..30 {
            frame_tx.send(fake_frame(320, 240, i * frame_dur)).unwrap();
        }

        // Send ~1 second of audio in 20ms chunks for both loopback and mic
        let chunk_dur_100ns = 200_000i64; // 20ms
        for i in 0..50 {
            let ts = i * chunk_dur_100ns;
            loopback_tx.send(fake_audio(ts, chunk_dur_100ns, audio_fmt.block_align, audio_fmt.sample_rate)).unwrap();
            mic_tx.send(fake_audio(ts, chunk_dur_100ns, audio_fmt.block_align, audio_fmt.sample_rate)).unwrap();
        }

        drop(frame_tx);
        drop(loopback_tx);
        drop(mic_tx);

        run(config, frame_rx, Some(loopback_rx), Some(mic_rx), stop);

        assert!(path.exists(), "MP4 file should exist");
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 1000, "MP4 with audio should be larger than 1KB, got {size}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_mux_no_frames_produces_no_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_mux_empty.mp4");
        let _ = std::fs::remove_file(&path);

        let config = MuxConfig {
            output_path: path.clone(),
            fps: 30,
            video_bitrate: 1_000_000,
            loopback_format: None,
            mic_format: None,
            session_epoch: crate::clock::now_ticks(),
        };

        let (_frame_tx, frame_rx) = mpsc::sync_channel::<VideoFrame>(4);
        let stop = Arc::new(AtomicBool::new(true)); // immediately stop

        run(config, frame_rx, None, None, stop);

        // No frames → sink writer never created → no file
        assert!(!path.exists(), "MP4 should not exist when no frames were sent");
    }
}
