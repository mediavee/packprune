//! `Double.toString` and `Float.toString`: the text Jackson's `asString` gives a number and the canonical descriptors
//! write.

use std::fmt::Write as _;

/// `Double.toString`: shortest digits, plain between 10^-3 and 10^7, else `d.dddE±n`.
pub fn java_double(value: f64) -> String {
    java_number(
        value.is_nan(),
        value.is_infinite(),
        value.is_sign_negative(),
        &format!("{:e}", value.abs()),
    )
}

/// `Float.toString`, the same rules over a float's shortest digits.
pub fn java_float(value: f32) -> String {
    java_number(
        value.is_nan(),
        value.is_infinite(),
        value.is_sign_negative(),
        &format!("{:e}", value.abs()),
    )
}

fn java_number(nan: bool, infinite: bool, negative: bool, scientific: &str) -> String {
    if nan {
        return "NaN".into();
    }
    let sign = if negative { "-" } else { "" };
    if infinite {
        return format!("{sign}Infinity");
    }
    let (mantissa, exponent) = scientific.split_once('e').expect("`{:e}` has an exponent");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().expect("an integer exponent");
    if digits.bytes().all(|digit| digit == b'0') {
        return format!("{sign}0.0");
    }
    let mut out = String::from(sign);
    if (-3..7).contains(&exponent) {
        if exponent < 0 {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', (-exponent - 1) as usize));
            out.push_str(&digits);
        } else {
            let point = exponent as usize + 1;
            let padded = format!("{digits:0<point$}");
            out.push_str(&padded[..point]);
            out.push('.');
            let fraction = &padded[point..];
            out.push_str(if fraction.is_empty() { "0" } else { fraction });
        }
    } else {
        out.push_str(&digits[..1]);
        out.push('.');
        out.push_str(if digits.len() > 1 { &digits[1..] } else { "0" });
        let _ = write!(out, "E{exponent}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Double.toString` of each value, as Java 25 prints it.
    #[test]
    fn numbers_print_as_java_prints_them() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (0.5, "0.5"),
            (100.0, "100.0"),
            (45.0, "45.0"),
            (0.1, "0.1"),
            (0.001, "0.001"),
            (0.0001, "1.0E-4"),
            (1.5e-4, "1.5E-4"),
            (1234567.0, "1234567.0"),
            (1e7, "1.0E7"),
            (12345678.0, "1.2345678E7"),
            (f64::from(0.1f32), "0.10000000149011612"),
            (-2.5, "-2.5"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for &(value, expected) in cases {
            assert_eq!(java_double(value), expected, "{value}");
        }
        assert_eq!(java_float(4.0), "4.0");
        assert_eq!(java_float(0.1), "0.1");
    }
}
