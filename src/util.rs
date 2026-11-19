use crate::env::Env;
use crate::errors::{usage_error, Result};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// `90`, `90s`, `5m`, `1h30m`, `2h`, `250ms` -> milliseconds.
pub fn parse_duration(input: &str, name: &str) -> Result<u64> {
    let s = input.trim().to_ascii_lowercase();
    let bad = || usage_error(format!("{name} must look like 90s, 5m or 1h30m."));
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse::<u64>().map(|n| n.saturating_mul(1000)).map_err(|_| bad());
    }
    let b = s.as_bytes();
    let (mut i, mut total) = (0usize, 0u64);
    if b.is_empty() {
        return Err(bad());
    }
    while i < b.len() {
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return Err(bad());
        }
        let n: u64 = s[start..i].parse().map_err(|_| bad())?;
        let unit_ms = if s[i..].starts_with("ms") {
            i += 2;
            1
        } else {
            let unit = match b.get(i) {
                Some(b's') => 1000,
                Some(b'm') => 60_000,
                Some(b'h') => 3_600_000,
                Some(b'd') => 86_400_000,
                _ => return Err(bad()),
            };
            i += 1;
            unit
        };
        total = total.saturating_add(n.saturating_mul(unit_ms));
    }
    Ok(total)
}

fn trim_zero(s: String) -> String {
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

pub fn format_bytes(n: f64) -> String {
    if !n.is_finite() || n < 0.0 {
        return "-".into();
    }
    if n < 1024.0 {
        return format!("{} B", n as u64);
    }
    let units = ["KB", "MB", "GB"];
    let mut v = n / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v < 10.0 {
        format!("{:.1} {}", v, units[i])
    } else {
        format!("{} {}", v.round() as u64, units[i])
    }
}

pub fn format_tokens(n: f64) -> String {
    if !n.is_finite() {
        return "-".into();
    }
    let a = n.abs();
    if a >= 1_000_000.0 {
        let v = n / 1_000_000.0;
        return format!("{}M", if a >= 10_000_000.0 { format!("{:.0}", v) } else { trim_zero(format!("{:.1}", v)) });
    }
    if a >= 1_000.0 {
        let v = n / 1_000.0;
        return format!("{}K", if a >= 10_000.0 { format!("{:.0}", v) } else { trim_zero(format!("{:.1}", v)) });
    }
    fmt_number(n)
}

/// A number as JavaScript would print it: no `.0` on whole numbers.
pub fn fmt_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// 1234567 -> "1,234,567".
pub fn group_thousands(n: f64) -> String {
    if !n.is_finite() {
        return "-".into();
    }
    let whole = n.trunc() as i64;
    let digits = whole.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if whole < 0 {
        out.insert(0, '-');
    }
    out
}

pub fn format_duration(ms: f64) -> String {
    let s = (ms / 1000.0).round().max(0.0) as u64;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m {}s", s % 60);
    }
    format!("{}h {}m", m / 60, m % 60)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Milliseconds since 1970 for an ISO 8601 time (`2026-11-17T11:57:00.000Z`, `+05:30` offsets, or a date).
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = s.get(from..from + len)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let (mut h, mut mi, mut sec, mut ms, mut offset_min) = (0, 0, 0, 0i64, 0i64);
    let mut i = 10;
    if i < b.len() && (b[i] == b'T' || b[i] == b't' || b[i] == b' ') {
        h = num(i + 1, 2)?;
        if b.get(i + 3) != Some(&b':') {
            return None;
        }
        mi = num(i + 4, 2)?;
        i += 6;
        if b.get(i) == Some(&b':') {
            sec = num(i + 1, 2)?;
            i += 3;
            if b.get(i) == Some(&b'.') {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                let frac = &s[start..j];
                let padded: String = format!("{:0<3}", frac).chars().take(3).collect();
                ms = padded.parse().unwrap_or(0);
                i = j;
            }
        }
        match b.get(i) {
            None => {}
            Some(b'Z') | Some(b'z') => i += 1,
            Some(&sign) if sign == b'+' || sign == b'-' => {
                let oh = num(i + 1, 2)?;
                let om = if b.get(i + 3) == Some(&b':') { num(i + 4, 2)? } else { num(i + 3, 2).unwrap_or(0) };
                offset_min = (oh * 60 + om) * if sign == b'-' { -1 } else { 1 };
                i = b.len();
            }
            _ => return None,
        }
        if i < b.len() {
            return None;
        }
    } else if i != b.len() {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(((days * 24 + h) * 60 + mi - offset_min) * 60_000 + sec * 1000 + ms)
}

/// `YYYY-MM-DD` (UTC) for a time in milliseconds.
pub fn iso_date(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// `2026-11-17T12:00:00.000Z` for a time in milliseconds.
pub fn iso_time(ms: i64) -> String {
    let day_ms = ms.rem_euclid(86_400_000);
    format!("{}T{:02}:{:02}:{:02}.{:03}Z", iso_date(ms), day_ms / 3_600_000, day_ms / 60_000 % 60, day_ms / 1000 % 60, day_ms % 1000)
}

/// "3m ago", "in 2d", or a date when it is far away.
pub fn relative_time(iso: Option<&str>, now: i64) -> String {
    let Some(t) = iso.and_then(parse_iso_ms) else { return "-".into() };
    let diff = t - now;
    let a = diff.abs() as f64;
    let fmt = |n: f64, unit: &str| if diff < 0 { format!("{}{unit} ago", n.round() as i64) } else { format!("in {}{unit}", n.round() as i64) };
    if a < 60_000.0 {
        return if diff < 0 { "just now".into() } else { "in <1m".into() };
    }
    if a < 3_600_000.0 {
        return fmt(a / 60_000.0, "m");
    }
    if a < 86_400_000.0 {
        return fmt(a / 3_600_000.0, "h");
    }
    if a < 30.0 * 86_400_000.0 {
        return fmt(a / 86_400_000.0, "d");
    }
    iso_date(t)
}

pub fn rel(v: &Value) -> String {
    relative_time(v.as_str(), now_ms())
}

/// Whitespace collapsed to single spaces, cut to `max` characters with "..." when longer.
pub fn truncate(s: &str, max: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    if max <= 3 {
        return one.chars().take(max).collect();
    }
    let mut cut: String = one.chars().take(max - 3).collect();
    cut.truncate(cut.trim_end().len());
    cut + "..."
}

/// The IANA time zone of this computer (TZ wins when it names one).
pub fn local_time_zone(env: &Env) -> Option<String> {
    if let Some(tz) = env.non_empty("TZ") {
        let tz = tz.trim_start_matches(':');
        if tz.contains('/') || tz.eq_ignore_ascii_case("UTC") {
            return Some(tz.to_string());
        }
    }
    iana_time_zone::get_timezone().ok().filter(|z| !z.is_empty())
}

/// A JSON field as display text: strings as they are, numbers without `.0`, nothing for null.
pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map(fmt_number).unwrap_or_else(|| n.to_string()),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A JSON field as text, or None when it is null, missing or empty.
pub fn opt_text(v: &Value) -> Option<String> {
    let t = text(v);
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

pub fn num(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90", "d").unwrap(), 90_000);
        assert_eq!(parse_duration("90s", "d").unwrap(), 90_000);
        assert_eq!(parse_duration("5m", "d").unwrap(), 300_000);
        assert_eq!(parse_duration("1h30m", "d").unwrap(), 5_400_000);
        assert_eq!(parse_duration("250ms", "d").unwrap(), 250);
        assert_eq!(parse_duration("2D", "d").unwrap(), 172_800_000);
        assert!(parse_duration("soon", "d").unwrap_err().message.contains("90s, 5m or 1h30m"));
        assert!(parse_duration("5m later", "d").is_err());
        assert!(parse_duration("", "d").is_err());
        assert!(parse_duration("m5", "d").is_err());
    }

    #[test]
    fn formatting() {
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1536.0), "1.5 KB");
        assert_eq!(format_bytes(50.0 * 1024.0 * 1024.0), "50 MB");
        assert_eq!(format_tokens(950.0), "950");
        assert_eq!(format_tokens(1_200_000.0), "1.2M");
        assert_eq!(format_tokens(5_000_000.0), "5M");
        assert_eq!(format_tokens(15_400.0), "15K");
        assert_eq!(group_thousands(1_200_000.0), "1,200,000");
        assert_eq!(group_thousands(999.0), "999");
        assert_eq!(format_duration(600.0), "1s");
        assert_eq!(format_duration(125_000.0), "2m 5s");
        assert_eq!(format_duration(7_500_000.0), "2h 5m");
        let now = parse_iso_ms("2026-11-17T12:00:00Z").unwrap();
        assert_eq!(relative_time(Some("2026-11-17T11:57:00Z"), now), "3m ago");
        assert_eq!(relative_time(Some("2026-11-19T12:00:00Z"), now), "in 2d");
        assert_eq!(relative_time(Some("2025-01-02T00:00:00Z"), now), "2025-01-02");
        assert_eq!(relative_time(None, now), "-");
        assert_eq!(truncate("a   b\n c", 10), "a b c");
        assert_eq!(truncate("abcdefghijkl", 7), "abcd...");
    }

    #[test]
    fn iso_times() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(parse_iso_ms("1970-01-01T05:30:00+05:30"), Some(0));
        assert_eq!(parse_iso_ms("1970-01-02"), Some(86_400_000));
        assert_eq!(parse_iso_ms("nonsense"), None);
        let t = parse_iso_ms("2026-11-17T12:34:56.789Z").unwrap();
        assert_eq!(iso_time(t), "2026-11-17T12:34:56.789Z");
        assert_eq!(iso_date(t), "2026-11-17");
    }
}
