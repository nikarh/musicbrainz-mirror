//! PostgreSQL's `COPY ... TO` text format, which the MusicBrainz dump files are in: one row per line, columns separated
//! by tabs, `\N` for NULL, and backslash escapes for the characters that would break that.

use std::borrow::Cow;

/// Splits a line (without its newline) into its fields, unescaped. `None` is NULL.
pub fn split_row<'a>(line: &'a [u8], out: &mut Vec<Option<Cow<'a, str>>>) {
    out.clear();
    for field in line.split(|&b| b == b'\t') {
        out.push(decode_field(field));
    }
}

fn decode_field(field: &[u8]) -> Option<Cow<'_, str>> {
    if field == b"\\N" {
        return None;
    }
    if !field.contains(&b'\\') {
        return Some(String::from_utf8_lossy(field));
    }
    let mut bytes = Vec::with_capacity(field.len());
    let mut i = 0;
    while let Some(&b) = field.get(i) {
        i += 1;
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        let Some(&next) = field.get(i) else {
            bytes.push(b'\\');
            break;
        };
        i += 1;
        match next {
            b'b' => bytes.push(0x08),
            b'f' => bytes.push(0x0c),
            b'n' => bytes.push(b'\n'),
            b'r' => bytes.push(b'\r'),
            b't' => bytes.push(b'\t'),
            b'v' => bytes.push(0x0b),
            b'0'..=b'7' => {
                let mut value = u32::from(next - b'0');
                for _ in 0..2 {
                    match field.get(i) {
                        Some(&d @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                bytes.push((value & 0xff) as u8);
            }
            b'x' if field.get(i).is_some_and(u8::is_ascii_hexdigit) => {
                let mut value = 0u8;
                for _ in 0..2 {
                    match field.get(i).and_then(|&d| char::from(d).to_digit(16)) {
                        Some(d) => {
                            value = value
                                .wrapping_mul(16)
                                .wrapping_add(u8::try_from(d).unwrap_or(0));
                            i += 1;
                        }
                        None => break,
                    }
                }
                bytes.push(value);
            }
            other => bytes.push(other),
        }
    }
    Some(Cow::Owned(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }))
}

/// A PostgreSQL boolean: `t`/`f` (or `1`/`0`, as the sample dumps have them).
#[must_use]
pub fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "t" | "true" | "1" => Some(true),
        "f" | "false" | "0" => Some(false),
        _ => None,
    }
}

/// A PostgreSQL integer array, `{1,2,3}` or `{"1","2","3"}`.
#[must_use]
pub fn parse_int_array(value: &str) -> Vec<i64> {
    value
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(',')
        .filter_map(|v| v.trim().trim_matches('"').parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(line: &str) -> Vec<Option<String>> {
        let mut out = Vec::new();
        split_row(line.as_bytes(), &mut out);
        out.into_iter().map(|f| f.map(Cow::into_owned)).collect()
    }

    #[test]
    fn fields_nulls_and_escapes() {
        assert_eq!(
            row("1\tabc\t\\N\t\ta\\tb\\nc\\\\d"),
            vec![
                Some("1".into()),
                Some("abc".into()),
                None,
                Some(String::new()),
                Some("a\tb\nc\\d".into())
            ]
        );
        assert_eq!(row("\\101\\x42\\q\\r"), vec![Some("ABq\r".into())]);
        // UTF-8 written as octal escapes
        assert_eq!(row("\\303\\251"), vec![Some("é".into())]);
        // a literal "\N" inside a longer value is not NULL
        assert_eq!(row("\\\\N"), vec![Some("\\N".into())]);
    }

    #[test]
    fn booleans_and_arrays() {
        assert_eq!(parse_bool("t"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("x"), None);
        assert_eq!(parse_int_array("{150,20750}"), vec![150, 20750]);
        assert_eq!(parse_int_array(r#"{"150","20750"}"#), vec![150, 20750]);
        assert_eq!(parse_int_array("{}"), Vec::<i64>::new());
    }
}
