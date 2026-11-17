//! UTC instants: `timestamptz` in Postgres, RFC 3339 text in the API.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt::Write;
use std::ops::{Add, Deref, Sub};
use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time};

/// A UTC instant. JSON is RFC 3339 with `Z` and 0, 3, 6 or 9 fractional digits, whichever is the
/// shortest that loses nothing; [`Timestamp::to_rfc3339`] is the same with `+00:00`. Both are what
/// clients have always been sent, so the digits are written here and not left to a formatter.
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

    /// RFC 3339 with a `+00:00` offset.
    pub fn to_rfc3339(self) -> String {
        self.write("+00:00")
    }

    fn write(self, offset: &str) -> String {
        let d = self.0;
        let mut s = String::with_capacity(35);
        let year = d.year();
        let _ = if (0..=9999).contains(&year) {
            write!(s, "{year:04}")
        } else {
            write!(s, "{year:+05}")
        };
        let _ = write!(
            s,
            "-{:02}-{:02}T{:02}:{:02}:{:02}",
            u8::from(d.month()),
            d.day(),
            d.hour(),
            d.minute(),
            d.second()
        );
        let nano = d.nanosecond();
        let _ = if nano == 0 {
            Ok(())
        } else if nano.is_multiple_of(1_000_000) {
            write!(s, ".{:03}", nano / 1_000_000)
        } else if nano.is_multiple_of(1_000) {
            write!(s, ".{:06}", nano / 1_000)
        } else {
            write!(s, ".{nano:09}")
        };
        s.push_str(offset);
        s
    }

    /// Reads RFC 3339 and the looser spellings requests have always been allowed: a space or `t`
    /// for `T`, unpadded numbers, spaces between the parts, `UTC` for the offset, an offset
    /// without the colon.
    pub fn parse(s: &str) -> Option<Self> {
        let (year, s) = year(s.trim_start())?;
        let (month, s) = number(s.trim_start().strip_prefix('-')?, 2)?;
        let (day, s) = number(s.trim_start().strip_prefix('-')?, 2)?;
        let s = s.strip_prefix(['t', 'T', ' '])?;
        let (hour, s) = number(s, 2)?;
        let (minute, s) = number(s.trim_start().strip_prefix(':')?, 2)?;
        let (second, s) = number(s.trim_start().strip_prefix(':')?, 2)?;
        let (nano, s) = match s.strip_prefix('.') {
            Some(frac) => {
                let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
                let kept = digits.min(9);
                let n: u32 = frac.get(..kept)?.parse().ok()?;
                (n * 10u32.pow(9 - kept as u32), &frac[digits..])
            }
            None => (0, s),
        };
        let (offset, s) = offset(s.trim_start())?;
        if !s.trim_start().is_empty() || second > 60 {
            return None;
        }
        let date =
            Date::from_calendar_date(year, Month::try_from(month as u8).ok()?, day as u8).ok()?;
        // A leap second is the last instant of second 59 plus one second.
        let time =
            Time::from_hms_nano(hour as u8, minute as u8, second.min(59) as u8, nano).ok()?;
        let leap = i64::from(second == 60);
        PrimitiveDateTime::new(date, time)
            .checked_add(Duration::seconds(leap - i64::from(offset)))
            .map(|t| Self(t.assume_utc()))
    }
}

fn digits(s: &str, max: usize) -> Option<(u32, &str)> {
    let n = s.bytes().take(max).take_while(u8::is_ascii_digit).count();
    Some((s.get(..n)?.parse().ok()?, &s[n..]))
}

fn number(s: &str, max: usize) -> Option<(u32, &str)> {
    digits(s.trim_start(), max)
}

/// Four digits at most unless a sign comes first.
fn year(s: &str) -> Option<(i32, &str)> {
    let (sign, max, s) = match s.as_bytes().first()? {
        b'-' => (-1, 6, &s[1..]),
        b'+' => (1, 6, &s[1..]),
        _ => (1, 4, s),
    };
    let (y, rest) = digits(s, max)?;
    if rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some((sign * i32::try_from(y).ok()?, rest))
}

/// Seconds east of UTC: `Z`, `UTC`, `+hh:mm` or `+hhmm`, less than a day.
fn offset(s: &str) -> Option<(i32, &str)> {
    if s.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("UTC")) {
        return Some((0, &s[3..]));
    }
    let mut chars = s.chars();
    let sign = match chars.next()? {
        'Z' | 'z' => return Some((0, chars.as_str())),
        '+' => 1,
        '-' | '\u{2212}' => -1,
        _ => return None,
    };
    let s = chars.as_str();
    let two = |s: &str| match s.as_bytes() {
        [a @ b'0'..=b'9', b @ b'0'..=b'9', ..] => Some(i32::from((a - b'0') * 10 + (b - b'0'))),
        _ => None,
    };
    let hours = two(s)?;
    let s = s[2..].trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let minutes = two(s).filter(|m| *m < 60)?;
    let secs = hours * 3600 + minutes * 60;
    (secs < 86_400).then_some((sign * secs, &s[2..]))
}

impl Deref for Timestamp {
    type Target = OffsetDateTime;
    fn deref(&self) -> &OffsetDateTime {
        &self.0
    }
}

impl Add<Duration> for Timestamp {
    type Output = Self;
    fn add(self, d: Duration) -> Self {
        Self(self.0 + d)
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Self;
    fn sub(self, d: Duration) -> Self {
        Self(self.0 - d)
    }
}

impl Sub for Timestamp {
    type Output = Duration;
    fn sub(self, other: Self) -> Duration {
        self.0 - other.0
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.write("Z"))
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = std::borrow::Cow::<str>::deserialize(d)?;
        Self::parse(&s)
            .ok_or_else(|| serde::de::Error::custom("expected an RFC 3339 date and time"))
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

    #[test]
    fn json_and_rfc3339_keep_their_digits() {
        // 2026-11-17T19:27:32Z
        let secs = 1_794_943_652;
        for (nano, frac) in [
            (0, ""),
            (120_000_000, ".120"),
            (123_000_000, ".123"),
            (123_400_000, ".123400"),
            (123_456_000, ".123456"),
            (100, ".000000100"),
            (123_456_789, ".123456789"),
        ] {
            let t = at(secs, nano);
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"2026-11-17T19:27:32{frac}Z\""));
            assert_eq!(t.to_rfc3339(), format!("2026-11-17T19:27:32{frac}+00:00"));
            assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), t);
            assert_eq!(Timestamp::parse(&t.to_rfc3339()), Some(t));
        }
        assert_eq!(
            serde_json::to_string(&at(0, 0)).unwrap(),
            "\"1970-01-01T00:00:00Z\""
        );
        assert_eq!(
            at(-62_135_596_800, 0).to_rfc3339(),
            "0001-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn parse_takes_the_loose_spellings() {
        let t = Some(at(1_794_943_652, 500_000_000));
        for s in [
            "2026-11-17T19:27:32.5Z",
            "2026-11-17t19:27:32.5z",
            "2026-11-17 19:27:32.5 UTC",
            "2026-11-17T19:27:32.500000000123+00:00",
            "2026-11-17T21:27:32.5+0200",
            "2026-11-17T14:57:32.5-04:30",
            "2026-11-17T14:57:32.5\u{2212}04 30",
            " 2026 - 11 - 17T 19 : 27 : 32.5 Z ",
        ] {
            assert_eq!(Timestamp::parse(s), t, "{s}");
        }
        assert_eq!(
            Timestamp::parse("2026-1-5 7:8:9Z"),
            Some(at(1_767_596_889, 0))
        );
        assert_eq!(
            Timestamp::parse("2016-12-31T23:59:60Z"),
            Some(at(1_483_228_800, 0))
        );
        for s in [
            "",
            "2026-11-17",
            "2026-11-17T19:27:32",
            "2026-11-17T19:27:32.Z",
            "2026-11-17T19:27Z",
            "2026-13-17T19:27:32Z",
            "2026-02-30T19:27:32Z",
            "2026-11-17T24:00:00Z",
            "2026-11-17T19:27:61Z",
            "2026-11-17T19:27:32+24:00",
            "2026-11-17T19:27:32+02",
            "2026-11-17T19:27:32+02:60",
            "20261-11-17T19:27:32Z",
            "2026-11-17T19:27:32Z x",
        ] {
            assert_eq!(Timestamp::parse(s), None, "{s}");
        }
    }
}
