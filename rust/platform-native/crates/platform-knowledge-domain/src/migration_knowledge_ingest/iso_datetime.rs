//! The metadata contract uses datetime.fromisoformat, not just RFC3339.
//! Accept calendar/basic/week dates, a single Unicode date/time separator,
//! reduced/basic times, decimal-comma fractions and sub-minute UTC offsets.
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
#[derive(Clone, Copy)]
pub(super) enum Error {
    Invalid,
    Naive,
}
fn number(raw: &[u8]) -> Option<u32> {
    if raw.is_empty() || !raw.iter().all(u8::is_ascii_digit) {
        return None;
    }
    raw.iter().try_fold(0u32, |n, d| {
        n.checked_mul(10)?.checked_add((d - b'0') as u32)
    })
}
fn date_end(raw: &[char]) -> Option<usize> {
    if raw.len() < 7 {
        return None;
    }
    if raw.len() == 7 {
        return Some(7);
    }
    Some(if raw[4] == '-' {
        if raw[5] == 'W' {
            if raw.len() > 8 && raw[8] == '-' {
                if raw.len() == 9 {
                    return None;
                }
                if raw.get(10).is_some_and(char::is_ascii_digit) {
                    8
                } else {
                    10
                }
            } else {
                8
            }
        } else {
            10
        }
    } else if raw[4] == 'W' {
        let mut end = 7;
        while raw.get(end).is_some_and(char::is_ascii_digit) {
            end += 1;
        }
        if end < 9 {
            end
        } else if end % 2 == 0 {
            7
        } else {
            8
        }
    } else {
        8
    })
}
fn date(raw: &str) -> Option<NaiveDate> {
    let b = raw.as_bytes();
    if ![7, 8, 10].contains(&b.len()) {
        return None;
    }
    let year = number(&b[..4])? as i32;
    if !(1..=9999).contains(&year) {
        return None;
    }
    let dashed = b[4] == b'-';
    let mut at = 4 + usize::from(dashed);
    let date = if b.get(at) == Some(&b'W') {
        at += 1;
        let week = number(b.get(at..at + 2)?)?;
        at += 2;
        let day = if at == b.len() {
            1
        } else {
            if dashed {
                if b.get(at) != Some(&b'-') {
                    return None;
                }
                at += 1;
            }
            if b.len() != at + 1 {
                return None;
            }
            number(&b[at..])?
        };
        let day = match day {
            1 => chrono::Weekday::Mon,
            2 => chrono::Weekday::Tue,
            3 => chrono::Weekday::Wed,
            4 => chrono::Weekday::Thu,
            5 => chrono::Weekday::Fri,
            6 => chrono::Weekday::Sat,
            7 => chrono::Weekday::Sun,
            _ => return None,
        };
        NaiveDate::from_isoywd_opt(year, week, day)?
    } else {
        let month = number(b.get(at..at + 2)?)?;
        at += 2;
        if dashed {
            if b.get(at) != Some(&b'-') {
                return None;
            }
            at += 1;
        }
        if b.len() != at + 2 {
            return None;
        }
        NaiveDate::from_ymd_opt(year, month, number(&b[at..])?)?
    };
    (1..=9999).contains(&date.year()).then_some(date)
}
fn clock(raw: &str) -> Option<[u32; 4]> {
    let b = raw.as_bytes();
    let mut out = [0; 4];
    let mut at = 0;
    let mut colon = false;
    for component in 0..3 {
        out[component] = number(b.get(at..at + 2)?)?;
        at += 2;
        if component == 0 {
            colon = b.get(at) == Some(&b':');
        }
        if at == b.len() || b.get(at).is_some_and(|b| b".,".contains(b)) {
            break;
        }
        if component == 2 {
            return None;
        }
        if colon {
            if b.get(at) != Some(&b':') {
                return None;
            }
            at += 1;
        }
    }
    if at < b.len() {
        if !b".,".contains(&b[at]) {
            return None;
        }
        at += 1;
        if at == b.len() || !b[at..].iter().all(u8::is_ascii_digit) {
            return None;
        }
        let n = (b.len() - at).min(6);
        out[3] = number(&b[at..at + n])? * 10u32.pow((6 - n) as u32);
    }
    Some(out)
}
pub(super) fn normalize(raw: &str) -> Result<String, Error> {
    let chars: Vec<_> = raw.trim().chars().collect();
    let end = date_end(&chars).ok_or(Error::Invalid)?;
    let d = date(
        &chars
            .get(..end)
            .ok_or(Error::Invalid)?
            .iter()
            .collect::<String>(),
    )
    .ok_or(Error::Invalid)?;
    if chars.len() == end {
        return Err(Error::Naive);
    }
    let time = chars
        .get(end + 1..)
        .ok_or(Error::Invalid)?
        .iter()
        .collect::<String>();
    if time.is_empty() {
        return Err(Error::Invalid);
    }
    let zone = time.find(['+', '-', 'Z']);
    let components = clock(&time[..zone.unwrap_or(time.len())]).ok_or(Error::Invalid)?;
    if components[0] > 23 || components[1] > 59 || components[2] > 59 {
        return Err(Error::Invalid);
    }
    let local = d
        .and_hms_micro_opt(components[0], components[1], components[2], components[3])
        .ok_or(Error::Invalid)?;
    let Some(zone) = zone else {
        return Err(Error::Naive);
    };
    let offset = if &time[zone..] == "Z" {
        0i64
    } else {
        let sign = match time.as_bytes()[zone] {
            b'+' => 1i64,
            b'-' => -1,
            _ => return Err(Error::Invalid),
        };
        let c = clock(&time[zone + 1..]).ok_or(Error::Invalid)?;
        let micros =
            ((c[0] as i64 * 60 + c[1] as i64) * 60 + c[2] as i64) * 1_000_000 + c[3] as i64;
        if micros >= 86_400_000_000 {
            return Err(Error::Invalid);
        }
        sign * micros
    };
    let utc: NaiveDateTime = local
        .checked_sub_signed(chrono::Duration::microseconds(offset))
        .ok_or(Error::Invalid)?;
    if !(1..=9999).contains(&utc.year()) {
        return Err(Error::Invalid);
    }
    let mut out = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        utc.year(),
        utc.month(),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second()
    );
    if utc.nanosecond() != 0 {
        out.push_str(&format!(".{:06}", utc.nanosecond() / 1000));
    }
    out.push('Z');
    Ok(out)
}
