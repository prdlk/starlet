//! Timestamp encoding.
//!
//! Every timestamp column is RFC 3339 in UTC. Encoding is explicit rather than
//! delegated to a driver mapping so the on-disk representation cannot change
//! under a dependency bump.

use chrono::{DateTime, SecondsFormat, Utc};

/// Encode for storage. Always UTC, always second precision with a `Z` suffix.
pub fn format_ts(ts: &DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Decode a stored timestamp. Unparseable values read back as `None` rather
/// than failing the whole row: a bad timestamp costs a sort key, not a repo.
pub fn parse_ts(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn round_trips_to_the_second() {
        let ts = Utc.with_ymd_and_hms(2026, 3, 14, 1, 59, 26).unwrap();
        let encoded = format_ts(&ts);
        assert_eq!(encoded, "2026-03-14T01:59:26Z");
        assert_eq!(parse_ts(Some(&encoded)), Some(ts));
    }

    #[test]
    fn accepts_githubs_offset_form() {
        assert_eq!(
            parse_ts(Some("2026-03-14T03:59:26+02:00")),
            Some(Utc.with_ymd_and_hms(2026, 3, 14, 1, 59, 26).unwrap())
        );
    }

    #[test]
    fn garbage_is_none_not_an_error() {
        assert_eq!(parse_ts(Some("yesterday")), None);
        assert_eq!(parse_ts(None), None);
    }
}
