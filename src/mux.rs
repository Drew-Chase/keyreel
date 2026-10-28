use crate::audio::{AudioFormat, AudioPacket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use windows::core::PCWSTR;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// A video frame received from the capture thread.
pub struct VideoFrame {
    pub data: Vec<u8>,
    pub timestamp_100ns: i64,
}

/// Configuration for the mux thread.
pub struct MuxConfig {
    pub output_path: std::path::PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_bitrate: u32,
    pub loopback_format: Option<AudioFormat>,
    pub mic_format: Option<AudioFormat>,
}

impl Default for MuxConfig {
    fn default() -> Self {
        Self {
            output_path: std::path::PathBuf::new(),
            width: 1920,
            height: 1080,
            fps: 30,
            video_bitrate: 3_000_000,
            loopback_format: None,
            mic_format: None,
        }
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
fn create_sink_writer(config: &MuxConfig) -> windows::core::Result<(IMFSinkWriter, StreamIndices)> {
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
        out_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(config.width, config.height))?;
        out_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(config.fps, 1))?;
        out_type.SetUINT32(&MF_MT_INTERLACE_MODE, 2)?; // Progressive
    }
    let video_idx = unsafe { writer.AddStream(&out_type)? };

    // --- Video input type (BGRA) ---
    let in_type = unsafe { MFCreateMediaType()? };
    unsafe {
        in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
        in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u32_pair(config.width, config.height))?;
        in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u32_pair(config.fps, 1))?;
        in_type.SetUINT32(&MF_MT_INTERLACE_MODE, 2)?;
    }
    unsafe { writer.SetInputMediaType(video_idx, &in_type, None)? };

    // --- Audio streams ---
    let loopback_idx = if let Some(ref fmt) = config.loopback_format {
        Some(add_audio_stream(&writer, fmt)?)
    } else {
        None
    };

    let mic_idx = if let Some(ref fmt) = config.mic_format {
        Some(add_audio_stream(&writer, fmt)?)
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
        eprintln!("mux: CoInitializeEx failed: {hr:?}");
        return;
    }
    if let Err(e) = unsafe { MFStartup(MF_VERSION, 0) } {
        eprintln!("mux: MFStartup failed: {e}");
        return;
    }

    let result = run_inner(&config, &video_rx, &loopback_rx, &mic_rx, &stop_flag);
    if let Err(e) = result {
        eprintln!("mux: error: {e}");
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
    let (writer, indices) = create_sink_writer(config)?;

    let frame_duration_100ns = 10_000_000i64 / config.fps as i64;
    let mut next_frame_time: i64 = 0;
    let mut last_frame: Option<Vec<u8>> = None;
    let mut first_frame_offset: Option<i64> = None;
    let recv_timeout = std::time::Duration::from_millis(1000 / config.fps as u64);

    // Per-stream last timestamp for monotonicity check
    let mut last_loopback_ts: i64 = -1;
    let mut last_mic_ts: i64 = -1;

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        // --- Drain audio channels (non-blocking) ---
        if let (Some(rx), Some(idx)) = (loopback_rx, indices.loopback) {
            while let Ok(pkt) = rx.try_recv() {
                if pkt.timestamp_100ns > last_loopback_ts {
                    let _ = write_sample(&writer, idx, &pkt.data, pkt.timestamp_100ns, pkt.duration_100ns);
                    last_loopback_ts = pkt.timestamp_100ns;
                }
            }
        }
        if let (Some(rx), Some(idx)) = (mic_rx, indices.mic) {
            while let Ok(pkt) = rx.try_recv() {
                if pkt.timestamp_100ns > last_mic_ts {
                    let _ = write_sample(&writer, idx, &pkt.data, pkt.timestamp_100ns, pkt.duration_100ns);
                    last_mic_ts = pkt.timestamp_100ns;
                }
            }
        }

        // --- Video frame with timeout for frame duplication ---
        match video_rx.recv_timeout(recv_timeout) {
            Ok(frame) => {
                let ts = if let Some(offset) = first_frame_offset {
                    frame.timestamp_100ns - offset
                } else {
                    first_frame_offset = Some(frame.timestamp_100ns);
                    0
                };

                // Duplicate last frame to catch up
                if let Some(ref prev_data) = last_frame {
                    while next_frame_time + frame_duration_100ns <= ts {
                        let _ = write_sample(
                            &writer,
                            indices.video,
                            prev_data,
                            next_frame_time,
                            frame_duration_100ns,
                        );
                        next_frame_time += frame_duration_100ns;
                    }
                }

                let write_ts = next_frame_time.max(ts);
                write_sample(
                    &writer,
                    indices.video,
                    &frame.data,
                    write_ts,
                    frame_duration_100ns,
                )?;
                next_frame_time = write_ts + frame_duration_100ns;
                last_frame = Some(frame.data);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(ref prev_data) = last_frame {
                    let data_copy = prev_data.clone();
                    let _ = write_sample(
                        &writer,
                        indices.video,
                        &data_copy,
                        next_frame_time,
                        frame_duration_100ns,
                    );
                    next_frame_time += frame_duration_100ns;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }

    // Drain any remaining audio
    if let (Some(rx), Some(idx)) = (loopback_rx, indices.loopback) {
        while let Ok(pkt) = rx.try_recv() {
            if pkt.timestamp_100ns > last_loopback_ts {
                let _ = write_sample(&writer, idx, &pkt.data, pkt.timestamp_100ns, pkt.duration_100ns);
                last_loopback_ts = pkt.timestamp_100ns;
            }
        }
    }
    if let (Some(rx), Some(idx)) = (mic_rx, indices.mic) {
        while let Ok(pkt) = rx.try_recv() {
            if pkt.timestamp_100ns > last_mic_ts {
                let _ = write_sample(&writer, idx, &pkt.data, pkt.timestamp_100ns, pkt.duration_100ns);
                last_mic_ts = pkt.timestamp_100ns;
            }
        }
    }

    unsafe { writer.Finalize()? };
    Ok(())
}
