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
