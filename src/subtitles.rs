use crate::clock;
use crate::keyboard::{KeyEvent, Modifiers};
use crate::keymap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Maximum gap (in 100ns units) between key-downs to group them into one cue.
const GROUP_GAP_100NS: i64 = 300 * 10_000; // 300 ms

/// Maximum characters in a grouped cue before forcing a new one.
const GROUP_MAX_CHARS: usize = 40;

/// Maximum cue duration (in 100ns units) if no next cue follows.
const MAX_CUE_DURATION_100NS: i64 = 1_500 * 10_000; // 1.5 s

pub struct SubtitleWriter {
    file: BufWriter<File>,
    sequence: u32,
    session_epoch: i64, // QPC ticks
    // Pending cue state
    pending_text: String,
    pending_start_100ns: i64,
    pending_end_100ns: i64,
    // Track modifier-only presses
    pending_mods_down: Vec<(i64, String)>, // (qpc, label) for mods pressed without a following key
    // Optional exe filter
    only_exe: Option<String>,
}

impl SubtitleWriter {
    pub fn new(
        path: &Path,
        session_epoch: i64,
        only_exe: Option<String>,
    ) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            file: BufWriter::new(file),
            sequence: 0,
            session_epoch,
            pending_text: String::new(),
            pending_start_100ns: 0,
            pending_end_100ns: 0,
            pending_mods_down: Vec::new(),
            only_exe,
        })
    }

    /// Feed a key event into the subtitle writer. Only key-down events are processed.
    pub fn feed(&mut self, event: &KeyEvent) -> io::Result<()> {
        if !event.down {
            return Ok(());
        }

        // Apply exe filter
        if let Some(ref filter) = self.only_exe {
            if let Some(ref exe) = event.foreground_exe {
                if !exe.eq_ignore_ascii_case(filter) {
                    return Ok(());
                }
            } else {
                // No foreground_exe info yet; skip if filter is active
                return Ok(());
            }
        }

        let event_100ns = clock::ticks_to_100ns(event.qpc - self.session_epoch);

        // If this is a modifier-only key, track it but don't emit yet
        if keymap::is_modifier(event.vk) {
            self.pending_mods_down.push((event_100ns, event.label.clone()));
            return Ok(());
        }

        // Non-modifier key arrived. Build the label with modifier prefix.
        let label = build_label(&event.label, &event.mods);

        // Is this a "typeable" single character (for grouping purposes)?
        let is_single_char = label.len() == 1 && label.chars().next().map_or(false, |c| !c.is_control());

        // Clear any pending modifier-only keys (they got consumed by this key)
        self.pending_mods_down.clear();

        // Grouping logic
        if !self.pending_text.is_empty() {
            let gap = event_100ns - self.pending_end_100ns;
            if is_single_char && gap < GROUP_GAP_100NS && self.pending_text.len() < GROUP_MAX_CHARS {
                // Append to existing cue
                self.pending_text.push(' ');
                self.pending_text.push_str(&label);
                self.pending_end_100ns = event_100ns;
                return Ok(());
            }
            // Flush the previous cue (its end time is the start of this new cue)
            let end = event_100ns.min(self.pending_start_100ns + MAX_CUE_DURATION_100NS);
            self.flush_cue(end)?;
        }

        // Start a new cue
        self.pending_text = if is_single_char {
            label
        } else {
            format_named_key(&label)
        };
        self.pending_start_100ns = event_100ns;
        self.pending_end_100ns = event_100ns;

        Ok(())
    }

    /// Flushes any remaining modifier-only keys that were pressed and released
    /// without a following non-modifier key.
    pub fn flush_pending_modifiers(&mut self) -> io::Result<()> {
        for (time_100ns, label) in std::mem::take(&mut self.pending_mods_down) {
            if !self.pending_text.is_empty() {
                let end = time_100ns.min(self.pending_start_100ns + MAX_CUE_DURATION_100NS);
                self.flush_cue(end)?;
            }
            self.pending_text = format_named_key(&label);
            self.pending_start_100ns = time_100ns;
            self.pending_end_100ns = time_100ns;
        }
        Ok(())
    }

    /// Finalize the SRT file: flush any pending cue.
    pub fn finish(&mut self) -> io::Result<()> {
        self.flush_pending_modifiers()?;
        if !self.pending_text.is_empty() {
            let end = self.pending_start_100ns + MAX_CUE_DURATION_100NS;
            self.flush_cue(end)?;
        }
        self.file.flush()
    }

    fn flush_cue(&mut self, end_100ns: i64) -> io::Result<()> {
        if self.pending_text.is_empty() {
            return Ok(());
        }

        self.sequence += 1;
        let start = format_srt_time(self.pending_start_100ns);
        let end = format_srt_time(end_100ns);
        let text = std::mem::take(&mut self.pending_text);

        write!(self.file, "{}\n{} --> {}\n{}\n\n", self.sequence, start, end, text)?;
        self.file.flush()
    }
}

/// Builds a display label with modifier prefix for non-modifier keys.
fn build_label(key_label: &str, mods: &Modifiers) -> String {
    let mut parts = Vec::new();

    if mods.any_ctrl() {
        parts.push("Ctrl");
    }
    if mods.any_alt() {
        parts.push("Alt");
    }
    // Only add Shift prefix for non-printable / named keys, not single characters
    // that already reflect shift (e.g., 'A' instead of 'a')
    let is_single_printable = key_label.len() == 1;
    if mods.any_shift() && !is_single_printable {
        parts.push("Shift");
    }
    if mods.any_win() {
        parts.push("Win");
    }

    if parts.is_empty() {
        key_label.to_string()
    } else {
        parts.push(key_label);
        parts.join("+")
    }
}

/// Wraps a named key in brackets for SRT display: "Enter" -> "[Enter]"
fn format_named_key(label: &str) -> String {
    // Single printable characters don't get brackets
    if label.len() == 1 {
        label.to_string()
    } else {
        format!("[{label}]")
    }
}

/// Formats a 100ns offset as SRT timestamp: HH:MM:SS,mmm
fn format_srt_time(hns: i64) -> String {
    let total_ms = hns / 10_000;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::Modifiers;

    #[test]
    fn test_srt_time_format() {
        assert_eq!(format_srt_time(0), "00:00:00,000");
        assert_eq!(format_srt_time(10_000_000), "00:00:01,000"); // 1 second
        assert_eq!(format_srt_time(12_345 * 10_000), "00:00:12,345");
        // 1h 23m 45s 678ms
        let hns = ((1 * 3600 + 23 * 60 + 45) * 1000 + 678) as i64 * 10_000;
        assert_eq!(format_srt_time(hns), "01:23:45,678");
    }

    #[test]
    fn test_build_label_no_mods() {
        let mods = Modifiers::default();
        assert_eq!(build_label("A", &mods), "A");
        assert_eq!(build_label("Enter", &mods), "Enter");
    }

    #[test]
    fn test_build_label_ctrl_alt_f3() {
        let mods = Modifiers {
            l_ctrl: true,
            l_alt: true,
            ..Default::default()
        };
        assert_eq!(build_label("F3", &mods), "Ctrl+Alt+F3");
    }

    #[test]
    fn test_build_label_shift_suppressed_for_single_char() {
        let mods = Modifiers {
            l_shift: true,
            ..Default::default()
        };
        // Shift + single char: shift prefix is suppressed (the char is already uppercase)
        assert_eq!(build_label("A", &mods), "A");
        // Shift + named key: shift prefix is kept
        assert_eq!(build_label("Tab", &mods), "Shift+Tab");
    }

    #[test]
    fn test_format_named_key() {
        assert_eq!(format_named_key("Enter"), "[Enter]");
        assert_eq!(format_named_key("Tab"), "[Tab]");
        assert_eq!(format_named_key("A"), "A");
    }

    fn make_event(qpc: i64, vk: u32, label: &str, mods: Modifiers) -> KeyEvent {
        KeyEvent {
            qpc,
            vk,
            scan: 0,
            extended: false,
            injected: false,
            down: true,
            mods,
            foreground_exe: None,
            label: label.to_string(),
        }
    }

    #[test]
    fn test_grouping_rapid_typing() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_grouping.srt");
        let mut w = SubtitleWriter::new(&path, 0, None).unwrap();

        let freq = clock::frequency();
        // Simulate typing "hello" with 100ms gaps (well within 300ms threshold)
        let gap_ticks = freq / 10; // 100ms in ticks
        let mods = Modifiers::default();

        for (i, ch) in "hello".chars().enumerate() {
            let qpc = gap_ticks * i as i64;
            let event = make_event(qpc, ch as u32, &ch.to_string(), mods.clone());
            w.feed(&event).unwrap();
        }
        w.finish().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        // Should be one cue with all characters grouped
        assert!(content.contains("h e l l o"), "Expected grouped text, got: {content}");
        // Should have only sequence number 1
        assert!(content.starts_with("1\n"));
        assert!(!content.contains("\n2\n"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_grouping_timeout_splits() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_timeout.srt");
        let mut w = SubtitleWriter::new(&path, 0, None).unwrap();

        let freq = clock::frequency();
        let mods = Modifiers::default();

        // Type "ab" then wait 500ms (> 300ms threshold) then "cd"
        let event_a = make_event(0, 'a' as u32, "a", mods.clone());
        let event_b = make_event(freq / 10, 'b' as u32, "b", mods.clone()); // +100ms
        let event_c = make_event(freq / 2, 'c' as u32, "c", mods.clone()); // +500ms from a
        let event_d = make_event(freq / 2 + freq / 10, 'd' as u32, "d", mods.clone());

        w.feed(&event_a).unwrap();
        w.feed(&event_b).unwrap();
        w.feed(&event_c).unwrap();
        w.feed(&event_d).unwrap();
        w.finish().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        // Should have two cues
        assert!(content.contains("a b"), "Expected 'a b', got: {content}");
        assert!(content.contains("c d"), "Expected 'c d', got: {content}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_named_key_gets_brackets() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_brackets.srt");
        let mut w = SubtitleWriter::new(&path, 0, None).unwrap();

        let mods = Modifiers::default();
        // VK_RETURN = 0x0D
        let event = make_event(0, 0x0D, "Enter", mods);
        w.feed(&event).unwrap();
        w.finish().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[Enter]"), "Expected [Enter], got: {content}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_modifier_prefix_in_subtitle() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_modprefix.srt");
        let mut w = SubtitleWriter::new(&path, 0, None).unwrap();

        let mods = Modifiers {
            l_ctrl: true,
            l_alt: true,
            ..Default::default()
        };
        // F3 key
        let event = make_event(0, 0x72, "F3", mods);
        w.feed(&event).unwrap();
        w.finish().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[Ctrl+Alt+F3]"), "Expected [Ctrl+Alt+F3], got: {content}");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_command_key_does_not_group_with_chars() {
        let dir = std::env::temp_dir();
        let path = dir.join("test_nogroup.srt");
        let mut w = SubtitleWriter::new(&path, 0, None).unwrap();

        let freq = clock::frequency();
        let mods = Modifiers::default();

        // Type "a" then Enter quickly
        let event_a = make_event(0, 'a' as u32, "a", mods.clone());
        let event_enter = make_event(freq / 20, 0x0D, "Enter", mods); // 50ms later

        w.feed(&event_a).unwrap();
        w.feed(&event_enter).unwrap();
        w.finish().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        // "a" and "[Enter]" should be separate cues even though they're within 300ms
        // because Enter is a named key, not a single printable char
        assert!(content.contains("1\n"), "Should have cue 1");
        assert!(content.contains("2\n"), "Should have cue 2, got: {content}");
        assert!(content.contains("[Enter]"), "Should contain [Enter]");

        std::fs::remove_file(&path).ok();
    }
}
