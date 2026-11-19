//! UTC instants: `timestamptz` in Postgres, RFC 3339 text in the API.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ops::{Add, Deref, Sub};
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, Month, OffsetDateTime, Time, UtcOffset};

/// A UTC instant, RFC 3339 in JSON and in query strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, sqlx::Type)]
#[sqlx(transparent)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    pub fn from_unix(secs: i64) -> Option<Self> {
        OffsetDateTime::from_unix_timestamp(secs).ok().map(Self)
    }

    /// Midnight UTC on the first day of a month.
    pub fn month_start(year: i32, month: u8) -> Option<Self> {
        let date = Date::from_calendar_date(year, Month::try_from(month).ok()?, 1).ok()?;
        Some(Self(date.midnight().assume_utc()))
    }

    /// The start of the hour this instant is in.
    pub fn hour_start(self) -> Self {
        Self(
            self.0
                .replace_time(Time::from_hms(self.0.hour(), 0, 0).unwrap_or(Time::MIDNIGHT)),
        )
    }

    pub fn to_rfc3339(self) -> String {
        self.0
            .format(&Rfc3339)
            .expect("a UTC instant with a four-digit year")
    }
}

impl Deref for Timestamp {
    type Target = OffsetDateTime;
    fn deref(&self) -> &OffsetDateTime {
        &self.0
    }
}

impl Add<Duration> for Timestamp {
    type Output = Self;
    fn add(self, rhs: Duration) -> Self {
        Self(self.0 + rhs)
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Self;
    fn sub(self, rhs: Duration) -> Self {
        Self(self.0 - rhs)
    }
}

impl Sub for Timestamp {
    type Output = Duration;
    fn sub(self, other: Self) -> Duration {
        self.0 - other.0
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        time::serde::rfc3339::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        time::serde::rfc3339::deserialize(deserializer).map(|t| Self(t.to_offset(UtcOffset::UTC)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64, nano: u32) -> Timestamp {
        Timestamp(
            OffsetDateTime::from_unix_timestamp(secs).unwrap() + Duration::nanoseconds(nano.into()),
        )
    }

    fn read(text: &str) -> Option<Timestamp> {
        serde_json::from_value(serde_json::Value::from(text)).ok()
    }

    #[test]
    fn json_and_text_are_rfc3339_in_utc() {
        // 2026-11-17T19:27:32Z
        let secs = 1_794_943_652;
        for (nano, frac) in [
            (0, ""),
            (120_000_000, ".12"),
            (123_456_000, ".123456"),
            (100, ".0000001"),
        ] {
            let instant = at(secs, nano);
            let text = format!("2026-11-17T19:27:32{frac}Z");
            assert_eq!(instant.to_rfc3339(), text);
            assert_eq!(serde_json::to_value(instant).unwrap(), text);
            assert_eq!(read(&text), Some(instant));
        }
    }

    #[test]
    fn reads_any_offset_as_the_same_instant() {
        let expected = Some(at(1_794_943_652, 500_000_000));
        for text in [
            "2026-11-17T19:27:32.5Z",
            "2026-11-17t19:27:32.5z",
            "2026-11-17T21:27:32.5+02:00",
            "2026-11-17T14:57:32.5-04:30",
        ] {
            assert_eq!(read(text), expected, "{text}");
        }
        assert_eq!(
            read("2026-11-17T21:27:32.5+02:00").map(Timestamp::to_rfc3339),
            Some("2026-11-17T19:27:32.5Z".to_string())
        );
    }

    #[test]
    fn refuses_what_is_not_rfc3339() {
        for text in [
            "",
            "2026-11-17",
            "2026-11-17T19:27:32",
            "2026-11-17T19:27:32 UTC",
            "2026-11-17T21:27:32+0200",
            "2026-1-5T7:8:9Z",
            "2026-13-17T19:27:32Z",
            "2026-02-30T19:27:32Z",
            "2026-11-17T24:00:00Z",
            "2026-11-17T19:27:32Z x",
        ] {
            assert_eq!(read(text), None, "{text}");
        }
    }
}
