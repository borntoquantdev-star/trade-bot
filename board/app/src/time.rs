//! Time on the board, in Thailand's.
//!
//! Every timestamp the bot and the exchange write is UTC (`16:47:11+00:00`),
//! and a room of students in Bangkok reading `16:47` for a trade they
//! watched happen at a quarter to midnight is a room that stops trusting
//! the clock column. So everything a card prints goes through here first and
//! comes out seven hours later.
//!
//! **Thailand keeps no daylight saving**, which is what makes a fixed offset
//! honest rather than a shortcut: +07:00 all year, with no table of rules to
//! get wrong.
//!
//! Written by hand rather than with a date library because the browser build
//! is WebAssembly and the whole job is "add seven hours to a string" - the
//! only hard part is the day rolling over, and that is twenty lines.

/// Thailand's offset from UTC, in hours.
pub const OFFSET_HOURS: i64 = 7;

/// What the board calls its timezone, wherever a label is wanted.
pub const ZONE: &str = "เวลาไทย";

/// A moment in Thai time, broken into the parts a card prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Local {
    /// `2026-11-13`
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `23:47:11`
    pub fn clock(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// `23:47`
    pub fn hhmm(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }
}

/// Read an ISO 8601 timestamp and move it into Thai time.
///
/// Accepts what this project's writers produce: `2026-11-13T16:47:11+00:00`,
/// with or without fractions, with `Z`, with any `+hh:mm`/`-hh:mm` offset,
/// and with none at all - which is taken to mean UTC, because every writer
/// here that omits it is writing UTC. Anything else is `None`, and callers
/// print the original text rather than a wrong time.
pub fn local(iso: &str) -> Option<Local> {
    Some(from_seconds(utc_seconds(iso)? + OFFSET_HOURS * 3600))
}

/// Seconds since 1970-01-01 UTC, for putting two timestamps in order
/// whatever offset each was written with.
pub fn utc_seconds(iso: &str) -> Option<i64> {
    let iso = iso.trim();
    let (date, rest) = iso.split_once('T').or_else(|| iso.split_once(' '))?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Split the clock from the offset that follows it, if there is one.
    let (clock, offset_minutes) = split_offset(rest)?;
    let clock = clock.split('.').next()?;
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next().unwrap_or("0").parse().ok()?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    Some(seconds - offset_minutes * 60)
}

/// Like [`utc_seconds`], keeping the fraction of a second: the bot writes
/// microseconds, and "the exchange answered in 0.38 seconds" needs them.
pub fn unix_of(iso: &str) -> Option<f64> {
    let whole = utc_seconds(iso)? as f64;
    let clock = iso
        .trim()
        .split_once('T')
        .or_else(|| iso.trim().split_once(' '))?
        .1;
    let fraction = match clock.split_once('.') {
        Some((_, rest)) => {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                0.0
            } else {
                format!("0.{digits}").parse::<f64>().ok()?
            }
        }
        None => 0.0,
    };
    Some(whole + fraction)
}

/// Unix seconds back to an ISO-8601 UTC timestamp with microseconds - the
/// shape the bot writes, for tests that need "five seconds ago".
pub fn clock_iso(unix: f64) -> String {
    let whole = unix.floor();
    let parts = from_seconds(whole as i64);
    let micros = (((unix - whole) * 1e6).round() as u32).min(999_999);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{micros:06}+00:00",
        parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second
    )
}

/// Unix seconds as a Thai wall clock to a tenth of a second, for timings.
pub fn clock_tenths(unix: f64) -> String {
    let whole = unix.floor();
    let local = from_seconds(whole as i64 + OFFSET_HOURS * 3600);
    let tenth = (((unix - whole) * 10.0).floor() as u32).min(9);
    format!(
        "{:02}:{:02}:{:02}.{tenth}",
        local.hour, local.minute, local.second
    )
}

/// `16:47:11+00:00` -> (`16:47:11`, 0). `Z` and a missing offset are both 0.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(clock) = rest.strip_suffix('Z') {
        return Some((clock, 0));
    }
    // The offset's sign is the last `+` or `-` in the string; the clock
    // itself contains neither.
    match rest.rfind(['+', '-']) {
        Some(at) => {
            let (clock, offset) = rest.split_at(at);
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let digits = &offset[1..];
            let (hours, minutes) = match digits.split_once(':') {
                Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
                None if digits.len() == 4 => (digits[..2].parse().ok()?, digits[2..].parse().ok()?),
                None => (digits.parse().ok()?, 0),
            };
            Some((clock, sign * (hours * 60 + minutes)))
        }
        None => Some((rest, 0)),
    }
}

/// Seconds since 1970-01-01 in Thai time, broken back into parts.
fn from_seconds(seconds: i64) -> Local {
    let days = seconds.div_euclid(86_400);
    let within = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    Local {
        year,
        month,
        day,
        hour: (within / 3600) as u32,
        minute: (within % 3600 / 60) as u32,
        second: (within % 60) as u32,
    }
}

/// Days since 1970-01-01 for a calendar date. Howard Hinnant's algorithm:
/// exact for every date in the proleptic Gregorian calendar, no tables.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The calendar date `days` after 1970-01-01. The inverse of the above.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// `23:47:11` for an ISO timestamp, or the text itself if it is not one -
/// so a sentence that arrived in a time column is still readable rather
/// than cut in half.
pub fn clock(iso: &str) -> String {
    local(iso)
        .map(|at| at.clock())
        .unwrap_or_else(|| iso.to_owned())
}

/// `2026-11-16 12:18:06` in Thai time for an ISO timestamp, or the text
/// itself if it is not one. For history: a list that reaches back past
/// midnight needs the day as well as the time, or 23:59 yesterday reads as
/// later than 00:01 today. The same shape GRVT's own history tables print.
pub fn stamp(iso: &str) -> String {
    local(iso)
        .map(|at| format!("{} {}", at.date(), at.clock()))
        .unwrap_or_else(|| iso.to_owned())
}

/// `23:47` for an ISO timestamp, or an empty string.
pub fn hhmm(iso: &str) -> String {
    local(iso).map(|at| at.hhmm()).unwrap_or_default()
}

/// `2026-11-13` in Thai time, or an empty string.
pub fn date(iso: &str) -> String {
    local(iso).map(|at| at.date()).unwrap_or_default()
}

/// The time now, in seconds since 1970, from whichever clock this build has.
///
/// `None` only if the platform will not say, in which case the board simply
/// shows no wall clock.
pub fn now_unix() -> Option<f64> {
    #[cfg(target_arch = "wasm32")]
    {
        Some(js_sys::Date::now() / 1000.0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|elapsed| elapsed.as_secs_f64())
    }
}

/// The wall clock in Thai time, for the top bar.
pub fn now_local() -> Option<Local> {
    let seconds = now_unix()? as i64 + OFFSET_HOURS * 3600;
    Some(from_seconds(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fraction_of_a_second_survives_for_timings() {
        let at = unix_of("2026-11-13T19:20:02.761786+00:00").unwrap();
        let whole = utc_seconds("2026-11-13T19:20:02+00:00").unwrap() as f64;
        assert!((at - whole - 0.761786).abs() < 1e-6);
        assert_eq!(unix_of("2026-11-13T19:20:02Z"), Some(whole));
    }

    #[test]
    fn a_timing_clock_is_thai_and_shows_tenths() {
        let at = unix_of("2026-11-13T19:20:02.761Z").unwrap();
        assert_eq!(clock_tenths(at), "02:20:02.7");
    }

    #[test]
    fn a_utc_timestamp_comes_out_seven_hours_later() {
        let at = local("2026-11-13T16:47:11.971602+00:00").unwrap();
        assert_eq!(at.clock(), "23:47:11");
        assert_eq!(at.date(), "2026-11-13");
    }

    /// The case that matters most and is easiest to get wrong: five past
    /// five in the evening UTC is already tomorrow in Bangkok.
    #[test]
    fn the_evening_in_utc_is_already_tomorrow_in_thailand() {
        let at = local("2026-11-13T17:05:00+00:00").unwrap();
        assert_eq!(at.date(), "2026-11-14");
        assert_eq!(at.clock(), "00:05:00");
    }

    #[test]
    fn the_last_day_of_a_month_and_of_a_year_roll_over_too() {
        assert_eq!(date("2026-11-30T20:00:00Z"), "2026-12-01");
        assert_eq!(date("2026-12-31T23:00:00+00:00"), "2027-01-01");
        assert_eq!(date("2028-02-28T20:00:00+00:00"), "2028-02-29", "leap day");
    }

    #[test]
    fn a_timestamp_already_in_thai_time_is_left_where_it_is() {
        assert_eq!(clock("2026-11-13T23:47:11+07:00"), "23:47:11");
    }

    #[test]
    fn every_offset_spelling_this_project_writes_is_understood() {
        let want = "23:47:11";
        for spelling in [
            "2026-11-13T16:47:11Z",
            "2026-11-13T16:47:11+00:00",
            "2026-11-13T16:47:11+0000",
            "2026-11-13T16:47:11",
            "2026-11-13 16:47:11",
            "2026-11-13T11:47:11-05:00",
        ] {
            assert_eq!(clock(spelling), want, "{spelling}");
        }
    }

    #[test]
    fn something_that_is_not_a_time_is_printed_as_it_came() {
        assert_eq!(clock("ยังไม่มี"), "ยังไม่มี");
        assert_eq!(clock(""), "");
        assert_eq!(hhmm("nonsense"), "");
        assert_eq!(date("2026-13-40T00:00:00Z"), "", "no thirteenth month");
    }

    #[test]
    fn the_calendar_arithmetic_round_trips_over_centuries() {
        for days in [-800_000, -1, 0, 1, 19_000, 20_405, 365 * 400] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn two_spellings_of_one_moment_are_the_same_second() {
        assert_eq!(
            utc_seconds("2026-11-13T16:47:11+00:00"),
            utc_seconds("2026-11-13T23:47:11+07:00")
        );
        assert_eq!(utc_seconds("2026-11-13T16:47:11Z"), Some(1_794_588_431));
    }

    #[test]
    fn the_wall_clock_is_somewhere_reasonable() {
        let now = now_local().expect("this platform has a clock");
        assert!(now.year >= 2026);
    }
}
