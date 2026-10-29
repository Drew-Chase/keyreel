use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use wasapi::{DeviceEnumerator, Direction, StreamMode};

/// Audio format info reported back by a capture thread at startup.
#[derive(Clone, Debug)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub block_align: u32,
}

/// A packet of audio data to send to the mux thread.
pub struct AudioPacket {
    pub data: Vec<u8>,
    pub timestamp_100ns: i64,
}

/// Starts the loopback (system audio) capture thread.
/// Returns the thread handle and the device's audio format, or None if the device isn't available.
pub fn start_loopback(
    audio_tx: SyncSender<AudioPacket>,
    stop_flag: Arc<AtomicBool>,
    session_epoch: i64,
) -> Option<(JoinHandle<()>, AudioFormat)> {
    start_capture_thread("loopback", Direction::Render, audio_tx, stop_flag, session_epoch)
}

/// Starts the microphone capture thread.
/// Returns the thread handle and the device's audio format, or None if the device isn't available.
pub fn start_mic(
    audio_tx: SyncSender<AudioPacket>,
    stop_flag: Arc<AtomicBool>,
    session_epoch: i64,
) -> Option<(JoinHandle<()>, AudioFormat)> {
    start_capture_thread("mic", Direction::Capture, audio_tx, stop_flag, session_epoch)
}

fn start_capture_thread(
    name: &'static str,
    device_direction: Direction,
    audio_tx: SyncSender<AudioPacket>,
    stop_flag: Arc<AtomicBool>,
    session_epoch: i64,
) -> Option<(JoinHandle<()>, AudioFormat)> {
    // Use a channel to get the format back from the thread (or an error)
    let (fmt_tx, fmt_rx) = mpsc::channel::<Result<AudioFormat, String>>();

    let handle = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = wasapi::initialize_mta();

            let result = run_capture(name, device_direction, &audio_tx, &stop_flag, &fmt_tx, session_epoch);
            if let Err(e) = result {
                crate::output::error(format!("{name}: capture error: {e}"));
            }

            wasapi::deinitialize();
        })
        .expect("failed to spawn audio thread");

    // Wait for the format report
    match fmt_rx.recv() {
        Ok(Ok(format)) => Some((handle, format)),
        Ok(Err(e)) => {
            crate::output::error(format!("{name}: device not available: {e}"));
            // Thread will exit on its own
            let _ = handle.join();
            None
        }
        Err(_) => {
            // Thread crashed before reporting
            let _ = handle.join();
            None
        }
    }
}

fn run_capture(
    _name: &str,
    device_direction: Direction,
    audio_tx: &SyncSender<AudioPacket>,
    stop_flag: &Arc<AtomicBool>,
    fmt_tx: &mpsc::Sender<Result<AudioFormat, String>>,
    session_epoch: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let enumerator = DeviceEnumerator::new().inspect_err(|e| {
        fmt_tx.send(Err(e.to_string())).ok();
    })?;

    let device = enumerator
        .get_default_device(&device_direction)
        .inspect_err(|e| {
            fmt_tx.send(Err(e.to_string())).ok();
        })?;

    let mut audio_client = device.get_iaudioclient().inspect_err(|e| {
        fmt_tx.send(Err(e.to_string())).ok();
    })?;

    let mix_format = audio_client.get_mixformat().inspect_err(|e| {
        fmt_tx.send(Err(e.to_string())).ok();
    })?;

    let sample_rate = mix_format.get_samplespersec();
    let channels = mix_format.get_nchannels();
    let bits = mix_format.get_bitspersample();
    let block_align = mix_format.get_blockalign();

    // For loopback: device_direction is Render, but we initialize with Direction::Capture
    // This automatically sets AUDCLNT_STREAMFLAGS_LOOPBACK in the wasapi crate
    let capture_direction = Direction::Capture;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 200_000, // 20ms buffer
    };

    audio_client
        .initialize_client(&mix_format, &capture_direction, &mode)
        .inspect_err(|e| {
            fmt_tx.send(Err(e.to_string())).ok();
        })?;

    let event = audio_client.set_get_eventhandle().inspect_err(|e| {
        fmt_tx.send(Err(e.to_string())).ok();
    })?;

    let capture_client = audio_client.get_audiocaptureclient().inspect_err(|e| {
        fmt_tx.send(Err(e.to_string())).ok();
    })?;

    // Report format back to main thread
    let format = AudioFormat {
        sample_rate,
        channels,
        bits_per_sample: bits,
        block_align,
    };
    fmt_tx.send(Ok(format.clone())).ok();

    // Start capturing
    audio_client.start_stream()?;

    let mut buf = vec![0u8; 4096 * block_align as usize];
    let epoch_100ns = crate::clock::ticks_to_100ns(session_epoch);

    // Fallback timestamp for packets where WASAPI doesn't report a QPC position.
    let mut fallback_ts_100ns: i64 = 0;

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        // Wait for data (100ms timeout to check stop flag periodically)
        let _ = event.wait_for_event(100);

        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        // Drain all available packets
        loop {
            let packet_size = match capture_client.get_next_packet_size() {
                Ok(Some(size)) if size > 0 => size,
                _ => break,
            };

            let needed = packet_size as usize * block_align as usize;
            if buf.len() < needed {
                buf.resize(needed, 0);
            }

            match capture_client.read_from_device(&mut buf) {
                Ok((frames_read, info)) => {
                    if frames_read == 0 {
                        break;
                    }

                    let byte_len = frames_read as usize * block_align as usize;
                    let dur_100ns =
                        (frames_read as i128 * 10_000_000 / sample_rate as i128) as i64;

                    // WASAPI reports the QPC time (in 100ns) at which the first frame
                    // of this packet was captured — the same clock as video and
                    // subtitles. Gaps (e.g. loopback delivers nothing during silence)
                    // and device-clock drift are corrected in the mux.
                    let ts_100ns = if info.timestamp != 0 {
                        info.timestamp as i64 - epoch_100ns
                    } else {
                        fallback_ts_100ns.max(crate::clock::ticks_to_100ns(
                            crate::clock::now_ticks() - session_epoch,
                        ) - dur_100ns)
                    };
                    fallback_ts_100ns = ts_100ns + dur_100ns;

                    // SILENT flag: the buffer contents must be treated as silence.
                    if info.flags.silent {
                        let silence = vec![0u8; byte_len];
                        let _ = audio_tx.try_send(AudioPacket {
                            data: silence,
                            timestamp_100ns: ts_100ns,
                        });
                    } else {
                        let _ = audio_tx.try_send(AudioPacket {
                            data: buf[..byte_len].to_vec(),
                            timestamp_100ns: ts_100ns,
                        });
                    }
                }
                Err(_) => break,
            }
        }
    }

    audio_client.stop_stream()?;
    Ok(())
}
