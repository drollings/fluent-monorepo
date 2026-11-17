pub fn hash_iri(iri: &str) -> i64 {
    let hash = common_core::hash::blake3_hash(iri.as_bytes());
    i64::from_le_bytes(hash[0..8].try_into().unwrap())
}

pub fn hash_blank_node(scope: &str, id: &str) -> i64 {
    let mut input = Vec::with_capacity(8 + scope.len() + 8 + id.len());
    input.extend_from_slice(&scope.len().to_le_bytes());
    input.extend_from_slice(scope.as_bytes());
    input.extend_from_slice(&id.len().to_le_bytes());
    input.extend_from_slice(id.as_bytes());
    let hash = common_core::hash::blake3_hash(&input);
    i64::from_le_bytes(hash[0..8].try_into().unwrap())
}

use crate::NS_XSD;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum XsdType {
    String,
    LangString,
    Integer,
    Decimal,
    Double,
    Boolean,
    DateTime,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TypedValue {
    String,
    LangString,
    Integer(i64),
    Double(f64),
    Boolean(bool),
    DateTime(i64),
    Other,
}

pub fn detect_xsd_type(datatype: Option<&str>) -> XsdType {
    let Some(dt) = datatype else {
        return XsdType::String;
    };
    // Single `strip_prefix` + `match`: the old chain built one `String`
    // per comparison (`XSD_NS.to_owned() + suffix`); classification is
    // unchanged, only the allocation is gone.
    let Some(suffix) = dt.strip_prefix(NS_XSD) else {
        return XsdType::Other;
    };
    match suffix {
        "string" => XsdType::String,
        "integer" | "int" | "long" | "short" => XsdType::Integer,
        "decimal" => XsdType::Decimal,
        "float" | "double" => XsdType::Double,
        "boolean" => XsdType::Boolean,
        "dateTime" | "date" => XsdType::DateTime,
        _ => XsdType::Other,
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> Option<i64> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap_year(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

/// Days since 1970-01-01 for a civil date (Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = yoe * 365 + yoe / 4 - yoe / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

fn parse_two_digits(text: &str) -> Option<i64> {
    if text.len() == 2 && text.bytes().all(|b| b.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}

/// Parse the ISO-8601 subset `YYYY-MM-DD[Thh:mm:ss[.frac][Z|±hh:mm]]` to
/// epoch seconds. A bare date means midnight UTC; a missing zone means UTC.
/// Anything else returns `None` (fail closed — never a silent epoch).
fn parse_xsd_datetime(value: &str) -> Option<i64> {
    let Some((date, time)) = value.split_once('T') else {
        return parse_xsd_date_midnight(value);
    };
    let mut date_parts = date.split('-');
    let (year, month, day) = match (date_parts.next(), date_parts.next(), date_parts.next()) {
        (Some(y), Some(m), Some(d)) if date_parts.next().is_none() => (
            y.parse::<i64>().ok()?,
            parse_two_digits(m)?,
            parse_two_digits(d)?,
        ),
        _ => return None,
    };
    if !(0..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > days_in_month(year, month)? {
        return None;
    }
    // Time: `hh:mm:ss`, optional `.frac`, optional zone.
    let (clock, zone) = match time.find(['Z', '+', '-']) {
        Some(idx) => time.split_at(idx),
        None => (time, ""),
    };
    let mut clock_parts = clock.split(':');
    let (hour, minute, second) = match (clock_parts.next(), clock_parts.next(), clock_parts.next())
    {
        (Some(h), Some(m), Some(s)) if clock_parts.next().is_none() => {
            let (sec, _frac) = match s.split_once('.') {
                Some((sec, frac))
                    if !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    (sec, Some(frac))
                }
                None => (s, None),
                _ => return None,
            };
            (
                parse_two_digits(h)?,
                parse_two_digits(m)?,
                parse_two_digits(sec)?,
            )
        }
        _ => return None,
    };
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let offset_secs = match zone {
        "" | "Z" => 0,
        _ => {
            let rest = zone.strip_prefix(['+', '-'])?;
            let sign = if zone.starts_with('+') { 1 } else { -1 };
            let (hours, minutes) = match rest.split_once(':') {
                Some((h, m)) => (parse_two_digits(h)?, parse_two_digits(m)?),
                None => return None,
            };
            if hours > 14 || minutes > 59 {
                return None;
            }
            sign * (hours * 3600 + minutes * 60)
        }
    };
    Some(
        days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second
            - offset_secs,
    )
}

/// A bare `YYYY-MM-DD` date means midnight UTC.
fn parse_xsd_date_midnight(value: &str) -> Option<i64> {
    let mut parts = value.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(y), Some(m), Some(d)) if parts.next().is_none() && y.len() == 4 => {
            let (year, month, day) = (
                y.parse::<i64>().ok()?,
                parse_two_digits(m)?,
                parse_two_digits(d)?,
            );
            if !(1..=12).contains(&month) || !(1..=days_in_month(year, month)?).contains(&day) {
                return None;
            }
            Some(days_from_civil(year, month, day) * 86400)
        }
        _ => None,
    }
}

pub fn normalize_literal(value: &str, lang: Option<&str>, datatype: Option<&str>) -> TypedValue {
    if lang.is_some() {
        return TypedValue::LangString;
    }
    match detect_xsd_type(datatype) {
        XsdType::Integer => {
            if let Ok(v) = value.trim().parse::<i64>() {
                return TypedValue::Integer(v);
            }
            TypedValue::Other
        }
        XsdType::Decimal | XsdType::Double => {
            if let Ok(v) = value.trim().parse::<f64>() {
                return TypedValue::Double(v);
            }
            TypedValue::Other
        }
        XsdType::Boolean => {
            if value == "true" || value == "1" {
                return TypedValue::Boolean(true);
            }
            if value == "false" || value == "0" {
                return TypedValue::Boolean(false);
            }
            TypedValue::Other
        }
        XsdType::DateTime => match parse_xsd_datetime(value.trim()) {
            Some(epoch) => TypedValue::DateTime(epoch),
            // Fail closed: an unparseable dateTime used to normalize to the
            // silent epoch `DateTime(0)` (1970-01-01, wrong for the task).
            None => TypedValue::Other,
        },
        XsdType::String => TypedValue::String,
        XsdType::LangString => TypedValue::LangString,
        XsdType::Other => TypedValue::Other,
    }
}

#[cfg(test)]
#[path = "../tests/normalize.rs"]
mod tests;
