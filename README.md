# KeyReel

A tiny, portable screen recorder for Windows that captures every keystroke alongside the video and writes them out as a subtitle track.

## What it's for

KeyReel was built to help reverse-engineer legacy software by watching it being used. The original use case is a client project to remake an old MS-DOS TUI application. The app has no surviving documentation of its keybinds, so the most reliable way to learn its controls is to watch someone who uses it every day.

KeyReel runs straight from a flash drive on the client's machine. You plug it in, start a recording, and let the user walk through their normal workflow in the emulator. The result is a video in which every key press appears on screen as a subtitle, synced to the moment it happened, plus a machine-readable log of the raw key data.

## Goals

- **Portable.** A single executable with no installer, no runtime dependencies, and no external tools such as ffmpeg. It runs from removable media and writes all output next to itself.
- **Lightweight.** Capture and encoding use built-in Windows 11 APIs and are GPU-accelerated, so recording doesn't disturb the machine or the app being demonstrated.
- **Accurate.** Keys are logged at the scan-code level, including extended-key and modifier state. Distinctions that matter to DOS software, such as numpad vs. arrow keys, left vs. right Alt, and Ctrl/Alt/Shift combinations on function keys, are preserved.
- **Leaves no trace.** Nothing is installed, registered, or written to the host system.

## How it works

KeyReel runs two pieces side by side:

- **Screen capture.** It uses Windows.Graphics.Capture to record either the emulator window or the full display. Frames are encoded to H.264 MP4 with Windows' built-in Media Foundation encoder.
- **Keyboard capture.** A low-level keyboard hook records every key down/up event with a high-resolution timestamp on the same clock as the video frames, so key events and video stay in sync.

When recording stops, the video is finalized, and the key events are written out both as a subtitle file and as a raw log.

## Output

Each session produces a set of files on the flash drive:

| File                        | Contents                                                                                                                      |
|-----------------------------|-------------------------------------------------------------------------------------------------------------------------------|
| `session_<timestamp>.mp4`   | The screen recording                                                                                                          |
| `session_<timestamp>.srt`   | Human-readable key presses (e.g. `Alt+F3`, `Enter`) as subtitles, playable in any video player                                |
| `session_<timestamp>.jsonl` | Raw key events: virtual key, scan code, flags, modifiers, and timestamp, one per line, for analysis and keymap reconstruction |

Load the `.srt` alongside the `.mp4` in VLC, mpv, or most other players to watch the session with keystrokes overlaid.

## Privacy and consent

KeyReel records **all** keyboard input on the machine while it is running, not just input to the target application. Only use it with the explicit knowledge and consent of the machine's owner and the person using it. Users should not enter passwords or other sensitive information during a recording session.

Because it installs a global keyboard hook, antivirus software may flag KeyReel. This is expected behavior for any tool of this kind.

## Scope

KeyReel is a purpose-built internal tool, not a general screen recorder. It targets Windows 11 only and intentionally keeps its feature set small: record, log keys, save locally.