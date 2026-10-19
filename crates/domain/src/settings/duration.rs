//! Durations written as text: `24h`, `1h30m`, `90s`, `7d`.

use thiserror::Error;
use time::Duration;

/// Why a text is not a duration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DurationError {
    /// The text is not a sequence of whole numbers each followed by `d`, `h`, `m` or `s`.
    #[error("a duration is written like '24h', '1h30m', '90s' or '7d'")]
    Malformed,
    /// The duration does not fit.
    #[error("a duration is too long")]
    TooLong,
}

/// Parses a duration: whole numbers, each followed by a unit — `d` days, `h` hours, `m` minutes,
/// `s` seconds — with no spaces (`24h`, `1h30m`).
///
/// # Errors
/// [`DurationError::Malformed`] for any other text, [`DurationError::TooLong`] past the range of
/// whole seconds in an `i64`.
pub fn parse_duration(text: &str) -> Result<Duration, DurationError> {
    if text.is_empty() {
        return Err(DurationError::Malformed);
    }
    let mut seconds: i64 = 0;
    let mut number: Option<i64> = None;
    for c in text.chars() {
        if let Some(digit) = c.to_digit(10) {
            let so_far = number.unwrap_or(0);
            let next = so_far
                .checked_mul(10)
                .and_then(|n| n.checked_add(i64::from(digit)))
                .ok_or(DurationError::TooLong)?;
            number = Some(next);
            continue;
        }
        let unit = match c {
            'd' => 86_400,
            'h' => 3_600,
            'm' => 60,
            's' => 1,
            _ => return Err(DurationError::Malformed),
        };
        let count = number.take().ok_or(DurationError::Malformed)?;
        seconds = count
            .checked_mul(unit)
            .and_then(|part| seconds.checked_add(part))
            .ok_or(DurationError::TooLong)?;
    }
    if number.is_some() {
        return Err(DurationError::Malformed);
    }
    Ok(Duration::seconds(seconds))
}

/// Writes a duration the way [`parse_duration`] reads it, in whole seconds, largest units first
/// (`1h30m`, `0s`). A negative duration is written as `0s`.
pub fn format_duration(duration: Duration) -> String {
    let mut rest = duration.whole_seconds().max(0);
    if rest == 0 {
        return "0s".to_owned();
    }
    let mut text = String::new();
    for (unit, label) in [(86_400, 'd'), (3_600, 'h'), (60, 'm'), (1, 's')] {
        let count = rest / unit;
        if count > 0 {
            text.push_str(&count.to_string());
            text.push(label);
            rest -= count * unit;
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_table() {
        let cases: &[(&str, Result<i64, DurationError>)] = &[
            ("24h", Ok(86_400)),
            ("1m", Ok(60)),
            ("90s", Ok(90)),
            ("7d", Ok(604_800)),
            ("1h30m", Ok(5_400)),
            ("1d2h3m4s", Ok(93_784)),
            ("0s", Ok(0)),
            ("", Err(DurationError::Malformed)),
            ("24", Err(DurationError::Malformed)),
            ("h", Err(DurationError::Malformed)),
            ("1 h", Err(DurationError::Malformed)),
            ("1H", Err(DurationError::Malformed)),
            ("1w", Err(DurationError::Malformed)),
            ("-1h", Err(DurationError::Malformed)),
            ("1.5h", Err(DurationError::Malformed)),
            ("99999999999999999999s", Err(DurationError::TooLong)),
            ("9999999999999999d", Err(DurationError::TooLong)),
        ];
        for (text, expected) in cases {
            let got = parse_duration(text).map(Duration::whole_seconds);
            assert_eq!(&got, expected, "{text:?}");
        }
    }

    #[test]
    fn format_round_trips() {
        for text in ["1d", "23h", "1m", "1h30m", "1d2h3m4s", "30d", "0s", "15m"] {
            let duration = parse_duration(text).unwrap();
            assert_eq!(format_duration(duration), text);
        }
        assert_eq!(format_duration(Duration::minutes(90)), "1h30m");
        assert_eq!(format_duration(Duration::hours(24)), "1d");
        assert_eq!(format_duration(Duration::seconds(-5)), "0s");
    }
}
