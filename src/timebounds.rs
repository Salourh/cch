//! Shared time-bound parsing for `--after` / `--before` filters.
//!
//! Bounds are normalized to `YYYY-MM-DDTHH:MM:SS` (UTC). Comparing against a
//! transcript timestamp or an mtime formatted the same way is a plain string
//! compare on the first 19 chars.

use std::time::{SystemTime, UNIX_EPOCH};

/// Parse a `--after` / `--before` bound into `YYYY-MM-DDTHH:MM:SS`.
///
/// Accepts `YYYY-MM-DD`, `...THH`, `...THH:MM`, `...THH:MM:SS`, with an
/// optional trailing `Z`. Missing parts pad with zeros — a date-only value
/// refers to the start of that day.
pub fn parse_bound(input: &str) -> anyhow::Result<String> {
    let s = input.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    if date.len() != 10
        || !date[..4].bytes().all(|b| b.is_ascii_digit())
        || date.as_bytes().get(4) != Some(&b'-')
        || date.as_bytes().get(7) != Some(&b'-')
        || !date[5..7].bytes().all(|b| b.is_ascii_digit())
        || !date[8..10].bytes().all(|b| b.is_ascii_digit())
    {
        anyhow::bail!("invalid date: expected YYYY-MM-DD, got {input:?}");
    }
    let padded = match time.len() {
        0 => "00:00:00".to_string(),
        2 => format!("{time}:00:00"),
        5 if time.as_bytes()[2] == b':' => format!("{time}:00"),
        8 if time.as_bytes()[2] == b':' && time.as_bytes()[5] == b':' => time.to_string(),
        _ => anyhow::bail!("invalid time: expected HH, HH:MM, or HH:MM:SS, got {input:?}"),
    };
    Ok(format!("{date}T{padded}"))
}

/// `after` is inclusive, `before` is exclusive. When either bound is set,
/// untimestamped events are dropped.
pub fn in_range(ts: Option<&str>, after: Option<&str>, before: Option<&str>) -> bool {
    if after.is_none() && before.is_none() {
        return true;
    }
    let Some(ts) = ts else { return false };
    let key = &ts[..ts.len().min(19)];
    if let Some(a) = after {
        if key < a {
            return false;
        }
    }
    if let Some(b) = before {
        if key >= b {
            return false;
        }
    }
    true
}

/// Format a `SystemTime` as `YYYY-MM-DDTHH:MM:SS` in UTC. Pre-epoch times
/// clamp to the epoch (good enough — no transcript is from 1969).
pub fn format_systime_utc(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, m, s) = (sod / 3600, (sod / 60) % 60, sod % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parse_bound_accepts_date_only() {
        assert_eq!(parse_bound("2026-04-23").unwrap(), "2026-04-23T00:00:00");
    }

    #[test]
    fn parse_bound_pads_partial_time_and_strips_trailing_z() {
        assert_eq!(parse_bound("2026-04-23T17").unwrap(), "2026-04-23T17:00:00");
        assert_eq!(
            parse_bound("2026-04-23T17:30").unwrap(),
            "2026-04-23T17:30:00"
        );
        assert_eq!(
            parse_bound("2026-04-23T17:30:45Z").unwrap(),
            "2026-04-23T17:30:45"
        );
    }

    #[test]
    fn parse_bound_rejects_garbage() {
        assert!(parse_bound("not-a-date").is_err());
        assert!(parse_bound("2026/04/23").is_err());
        assert!(parse_bound("2026-04-23T17:30:45:99").is_err());
    }

    #[test]
    fn in_range_no_bounds_is_open() {
        assert!(in_range(None, None, None));
        assert!(in_range(Some("2026-04-23T10:00:00Z"), None, None));
    }

    #[test]
    fn in_range_drops_untimestamped_when_bounded() {
        assert!(!in_range(None, Some("2026-04-23T00:00:00"), None));
        assert!(!in_range(None, None, Some("2026-04-23T00:00:00")));
    }

    #[test]
    fn in_range_after_inclusive_before_exclusive() {
        let ts = "2026-04-23T17:00:00.123Z";
        let bound = "2026-04-23T17:00:00";
        assert!(in_range(Some(ts), Some(bound), None));
        assert!(!in_range(Some(ts), None, Some(bound)));
        assert!(in_range(Some(ts), None, Some("2026-04-23T17:00:01")));
        assert!(!in_range(Some(ts), Some("2026-04-23T17:00:01"), None));
    }

    #[test]
    fn format_systime_utc_epoch() {
        assert_eq!(format_systime_utc(UNIX_EPOCH), "1970-01-01T00:00:00");
    }

    #[test]
    fn format_systime_utc_known_instant() {
        // 2026-04-23T17:30:45 UTC = 1 776 965 445 seconds after the epoch.
        let t = UNIX_EPOCH + Duration::from_secs(1_776_965_445);
        assert_eq!(format_systime_utc(t), "2026-04-23T17:30:45");
    }

    #[test]
    fn format_systime_utc_month_boundary() {
        // 2024-02-29T00:00:00 UTC — leap day.
        let t = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(format_systime_utc(t), "2024-02-29T00:00:00");
    }
}
