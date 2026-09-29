//! Unified time handling (FEATURES.md §12 "时间"): UTC timestamps, conversion
//! helpers for platform-specific representations (OOPZ microsecond times,
//! TS3 relative times).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// UTC timestamp wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp {
    /// Seconds since the unix epoch.
    pub unix_secs: i64,
}

impl Timestamp {
    pub fn now() -> Self {
        Self::from_unix_secs(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        )
    }

    pub fn from_unix_secs(secs: i64) -> Self {
        Self { unix_secs: secs }
    }

    pub fn from_micros(micros: i64) -> Self {
        Self {
            unix_secs: micros.div_euclid(1_000_000),
        }
    }

    pub fn unix_secs(&self) -> i64 {
        self.unix_secs
    }

    pub fn elapsed(&self) -> Option<Duration> {
        Timestamp::now()
            .unix_secs
            .checked_sub(self.unix_secs)
            .and_then(|d| u64::try_from(d).ok())
            .map(Duration::from_secs)
    }

    /// TS3 `connection_connected_time` style values (milliseconds).
    pub fn from_millis(ms: i64) -> Self {
        Self {
            unix_secs: ms.div_euclid(1000),
        }
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Days/seconds civil conversion (Howard Hinnant's algorithm).
        let secs = self.unix_secs;
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        write!(
            f,
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_rfc3339() {
        assert_eq!(Timestamp::from_unix_secs(0).to_string(), "1970-01-01T00:00:00Z");
        // 2026-09-29T00:00:00Z = 1790640000
        assert_eq!(
            Timestamp::from_unix_secs(1_790_640_000).to_string(),
            "2026-09-29T00:00:00Z"
        );
        assert_eq!(
            Timestamp::from_unix_secs(951_827_696).to_string(),
            "2000-02-29T12:34:56Z"
        );
    }

    #[test]
    fn conversions() {
        assert_eq!(Timestamp::from_micros(1_500_000).unix_secs(), 1);
        assert_eq!(Timestamp::from_millis(123_456).unix_secs(), 123);
        assert!(Timestamp::from_unix_secs(0).elapsed().is_some());
    }
}
