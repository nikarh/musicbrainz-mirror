//! MusicBrainz's partial dates: any of year, month and day may be missing.

use std::fmt::Write as _;

/// A date as MusicBrainz formats it: `2001-02-03`, `2001-02`, `2001`, `????-02-03`, or empty.
#[must_use]
pub fn format(year: Option<i64>, month: Option<i64>, day: Option<i64>) -> String {
    let month = month.filter(|m| *m != 0);
    let day = day.filter(|d| *d != 0);
    let mut out = String::new();
    match year {
        Some(y) => {
            let _ = write!(out, "{y:04}");
        }
        None if month.is_some() || day.is_some() => out.push_str("????"),
        None => {}
    }
    match month {
        Some(m) => {
            let _ = write!(out, "-{m:02}");
        }
        None if day.is_some() => out.push_str("-??"),
        None => {}
    }
    if let Some(d) = day {
        let _ = write!(out, "-{d:02}");
    }
    out
}

/// A date stored packed by derive.sql: `year * 10000 + month * 100 + day`, missing parts as 9999 / 99 / 99.
#[must_use]
pub fn unpack(packed: i64) -> (Option<i64>, Option<i64>, Option<i64>) {
    let year = packed / 10000;
    let month = packed / 100 % 100;
    let day = packed % 100;
    (
        (year != 9999).then_some(year),
        (month != 99).then_some(month),
        (day != 99).then_some(day),
    )
}

/// Formats a packed date.
#[must_use]
pub fn format_packed(packed: i64) -> String {
    let (y, m, d) = unpack(packed);
    format(y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_partial_dates() {
        assert_eq!(format(Some(2001), Some(2), Some(3)), "2001-02-03");
        assert_eq!(format(Some(2001), Some(2), None), "2001-02");
        assert_eq!(format(Some(987), None, None), "0987");
        assert_eq!(format(None, Some(2), Some(3)), "????-02-03");
        assert_eq!(format(Some(2001), None, Some(3)), "2001-??-03");
        assert_eq!(format(None, None, None), "");
        assert_eq!(format_packed(2001 * 10000 + 2 * 100 + 99), "2001-02");
        assert_eq!(format_packed(9999 * 10000 + 99 * 100 + 99), "");
        assert_eq!(
            unpack(1973 * 10000 + 3 * 100 + 24),
            (Some(1973), Some(3), Some(24))
        );
    }
}
