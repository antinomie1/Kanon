//! Renders Unix timestamps without pulling in a date-time dependency.
//!
//! The node needs exactly one thing from a calendar: "what time was this message, in the host's
//! own timezone". A full date library would be a large addition for that, so the host C library is
//! used directly on Unix (`localtime_r`, which honours the system timezone and daylight saving),
//! and UTC is the fallback everywhere else.

/// Formats Unix seconds as `YYYY-MM-DD HH:MM:SS` in the host's local timezone.
pub fn format_local(seconds: i64) -> String {
    #[cfg(unix)]
    if let Some(tm) = local_tm(seconds) {
        return format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        );
    }

    format_utc(seconds)
}

/// Renders a UTC offset in seconds as `+HH:MM` / `-HH:MM`.
pub fn format_offset(offset_seconds: i64) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let absolute = offset_seconds.abs();
    format!("{sign}{:02}:{:02}", absolute / 3600, (absolute % 3600) / 60)
}

/// Offset of local time from UTC at the given instant, when the platform exposes it.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "android",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub fn local_offset_seconds(seconds: i64) -> Option<i64> {
    // `tm_gmtoff` is the platform's own answer, so daylight saving is already folded in.
    local_tm(seconds).map(|tm| tm.tm_gmtoff)
}

/// Offset of local time from UTC; unavailable on platforms without a usable localtime.
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "android",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
pub fn local_offset_seconds(_seconds: i64) -> Option<i64> {
    None
}

/// Current Unix time in seconds, saturating at the epoch for a clock before 1970.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

/// Reads the local broken-down time, or `None` when the platform cannot provide one.
#[cfg(unix)]
fn local_tm(seconds: i64) -> Option<libc::tm> {
    let time = seconds as libc::time_t;
    let mut broken_down: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes into the caller-owned `broken_down` and reads no memory we own.
    let result = unsafe { libc::localtime_r(&time, &mut broken_down) };
    if result.is_null() {
        None
    } else {
        Some(broken_down)
    }
}

/// Formats Unix seconds as UTC, used when no localtime is available.
fn format_utc(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

/// Converts days since the Unix epoch into a civil `(year, month, day)`.
///
/// Hinnant's `civil_from_days`, used only by the UTC fallback.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month, day)
}
