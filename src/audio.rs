use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use wasapi::{AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, Handle, StreamMode};

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
/// Returns the thread handle and the device's audio format, or the reason the
/// device isn't available. Shows no UI; the caller decides how to report it.
pub fn start_loopback(
    audio_tx: SyncSender<AudioPacket>,
    stop_flag: Arc<AtomicBool>,
    session_epoch: i64,
) -> Result<(JoinHandle<()>, AudioFormat), String> {
    start_capture_thread("Desktop audio", Direction::Render, audio_tx, stop_flag, session_epoch)
}

/// Starts the microphone capture thread.
/// Returns the thread handle and the device's audio format, or the reason the
/// device isn't available. Shows no UI; the caller decides how to report it.
pub fn start_mic(
    audio_tx: SyncSender<AudioPacket>,
    stop_flag: Arc<AtomicBool>,
    session_epoch: i64,
) -> Result<(JoinHandle<()>, AudioFormat), String> {
    start_capture_thread("Microphone", Direction::Capture, audio_tx, stop_flag, session_epoch)
}

fn start_capture_thread(
    name: &'static str,
    device_direction: Direction,
    audio_tx: SyncSender<AudioPacket>,
    stop_flag: Arc<AtomicBool>,
    session_epoch: i64,
) -> Result<(JoinHandle<()>, AudioFormat), String> {
    // The device is opened on the capture thread (WASAPI objects stay on the
    // thread that made them); it reports the format or the failure back here.
    let (fmt_tx, fmt_rx) = mpsc::channel::<Result<AudioFormat, String>>();

    let handle = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = wasapi::initialize_mta();

            match open_device(&device_direction) {
                Ok(device) => {
                    fmt_tx.send(Ok(device.format.clone())).ok();
                    if let Err(e) = run_capture(device, &audio_tx, &stop_flag, session_epoch) {
                        // Mid-recording failure (e.g. device unplugged): the mux pads
                        // the track with silence, so the recording itself carries on.
                        // Warn on a separate thread so stopping isn't held up by the dialog.
                        let text = format!(
                            "{name} stopped capturing: {e}\n\nThe recording continues without it."
                        );
                        std::thread::spawn(move || crate::output::warn(text));
                    }
                }
                Err(e) => {
                    fmt_tx.send(Err(e)).ok();
                }
            }

            wasapi::deinitialize();
        })
        .map_err(|e| e.to_string())?;

    match fmt_rx.recv() {
        Ok(Ok(format)) => Ok((handle, format)),
        Ok(Err(e)) => {
            let _ = handle.join();
            Err(e)
        }
        Err(_) => {
            let _ = handle.join();
            Err("capture thread exited unexpectedly".into())
        }
    }
}

/// An opened, started WASAPI capture stream.
struct OpenDevice {
    audio_client: AudioClient,
    capture_client: AudioCaptureClient,
    event: Handle,
    format: AudioFormat,
}

/// Opens the default device for `direction` and starts capturing from it.
/// For Render, this is a loopback capture of what the speakers are playing.
fn open_device(direction: &Direction) -> Result<OpenDevice, String> {
    let err = |e: wasapi::WasapiError| e.to_string();

    let enumerator = DeviceEnumerator::new().map_err(err)?;
    let device = enumerator.get_default_device(direction).map_err(err)?;
    let mut audio_client = device.get_iaudioclient().map_err(err)?;
    let mix_format = audio_client.get_mixformat().map_err(err)?;

    // For loopback: direction is Render, but we initialize with Direction::Capture.
    // This automatically sets AUDCLNT_STREAMFLAGS_LOOPBACK in the wasapi crate.
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 200_000, // 20ms buffer
    };
    audio_client
        .initialize_client(&mix_format, &Direction::Capture, &mode)
        .map_err(err)?;
    let event = audio_client.set_get_eventhandle().map_err(err)?;
    let capture_client = audio_client.get_audiocaptureclient().map_err(err)?;
    audio_client.start_stream().map_err(err)?;

    Ok(OpenDevice {
        audio_client,
        capture_client,
        event,
        format: AudioFormat {
            sample_rate: mix_format.get_samplespersec(),
            channels: mix_format.get_nchannels(),
            bits_per_sample: mix_format.get_bitspersample(),
            block_align: mix_format.get_blockalign(),
        },
    })
}

fn run_capture(
    device: OpenDevice,
    audio_tx: &SyncSender<AudioPacket>,
    stop_flag: &Arc<AtomicBool>,
    session_epoch: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let OpenDevice { audio_client, capture_client, event, format } = device;
    let AudioFormat { sample_rate, block_align, .. } = format;

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
