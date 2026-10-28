use std::sync::OnceLock;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

static FREQUENCY: OnceLock<i64> = OnceLock::new();

/// Returns the QPC frequency (ticks per second). Cached after first call.
pub fn frequency() -> i64 {
    *FREQUENCY.get_or_init(|| {
        let mut freq = 0i64;
        unsafe { QueryPerformanceFrequency(&mut freq).unwrap() };
        freq
    })
}

/// Returns the current QPC tick count.
pub fn now_ticks() -> i64 {
    let mut ticks = 0i64;
    unsafe { QueryPerformanceCounter(&mut ticks).unwrap() };
    ticks
}

/// Converts a QPC tick delta to 100-nanosecond units (Media Foundation time base).
pub fn ticks_to_100ns(ticks: i64) -> i64 {
    // Use 128-bit math to avoid overflow: (ticks * 10_000_000) / freq
    let numerator = ticks as i128 * 10_000_000i128;
    (numerator / frequency() as i128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_is_positive() {
        assert!(frequency() > 0);
    }

    #[test]
    fn now_ticks_increases() {
        let a = now_ticks();
        let b = now_ticks();
        assert!(b >= a);
    }

    #[test]
    fn ticks_to_100ns_roundtrip() {
        let freq = frequency();
        // 1 second worth of ticks should be 10_000_000 hundred-nanosecond units
        let hns = ticks_to_100ns(freq);
        assert_eq!(hns, 10_000_000);
    }
}
