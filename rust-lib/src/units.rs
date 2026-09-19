//! Raw integer -> token units, exactly.
//!
//! No rounding, no floats, no scientific notation: a signing screen that rounds is a
//! signing screen that can be made to show the wrong number. The input is a decimal
//! integer string of arbitrary width, so it is scaled by moving a decimal point through
//! the digits rather than by any arithmetic that could overflow.

/// `("10000000000", 18)` -> `"0.00000001"`. `None` for anything that is not a plain
/// decimal integer — the caller then shows the raw value alone, which is what it would
/// have shown anyway.
pub fn scale(raw: &str, decimals: u8) -> Option<String> {
    let digits = raw.trim();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let d = decimals as usize;
    if d == 0 {
        return Some(trim_leading(digits).to_string());
    }
    let padded;
    let digits = if digits.len() <= d {
        padded = format!("{}{}", "0".repeat(d + 1 - digits.len()), digits);
        padded.as_str()
    } else {
        digits
    };
    let (whole, frac) = digits.split_at(digits.len() - d);
    let frac = frac.trim_end_matches('0');
    let whole = trim_leading(whole);
    Some(if frac.is_empty() { whole.to_string() } else { format!("{whole}.{frac}") })
}

/// Unix seconds as `2026-09-19 18:12:24 UTC`.
pub fn utc(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", rem / 3_600, rem % 3_600 / 60, rem % 60)
}

fn trim_leading(s: &str) -> &str {
    let t = s.trim_start_matches('0');
    if t.is_empty() {
        "0"
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_smaller_than_one_unit_keeps_every_digit() {
        assert_eq!(scale("10000000000", 18).unwrap(), "0.00000001");
        assert_eq!(scale("1", 18).unwrap(), "0.000000000000000001");
    }

    #[test]
    fn unix_seconds_read_as_a_utc_date() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc(1_789_850_344), "2026-09-19 20:39:04 UTC");
    }

    #[test]
    fn a_whole_number_of_units_shows_no_point() {
        assert_eq!(scale("1000000000000000000", 18).unwrap(), "1");
        assert_eq!(scale("2500000", 6).unwrap(), "2.5");
        assert_eq!(scale("0", 18).unwrap(), "0");
    }

    #[test]
    fn nothing_is_rounded_however_long_the_number_is() {
        // 79 digits: wider than u256's decimal width, and wider than any float.
        let big = "1".repeat(79);
        let got = scale(&big, 18).unwrap();
        assert_eq!(got, format!("{}.{}", "1".repeat(61), "1".repeat(18)));
        assert_eq!(got.chars().filter(char::is_ascii_digit).count(), 79);
    }

    #[test]
    fn zero_decimals_is_the_integer_itself() {
        assert_eq!(scale("42", 0).unwrap(), "42");
    }

    #[test]
    fn anything_that_is_not_a_decimal_integer_scales_to_nothing() {
        // Hex, signs and blanks all reach here from a decoded arg. Refusing beats
        // guessing: the caller falls back to the raw string it already had.
        for bad in ["", "0x2a", "-1", "1.5", "1e18", " 42 x"] {
            assert!(scale(bad, 18).is_none(), "{bad:?}");
        }
    }
}
