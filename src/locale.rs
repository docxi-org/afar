//! Dates, times and numbers as Windows' regional settings write them
//! (Far's `locale.cpp`, `datetime.cpp`): the order of the date, its
//! separator, the time's separator, the thousands' separator.

use std::sync::OnceLock;

use chrono::{Datelike, Timelike};

/// The order of a date (Far's `date_type`, from `LOCALE_IDATE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Mdy,
    Dmy,
    Ymd,
}

#[derive(Clone, Copy, Debug)]
pub struct Locale {
    pub order: Order,
    pub date_sep: char,
    pub time_sep: char,
    pub thousands: char,
}

impl Default for Locale {
    fn default() -> Self {
        Self {
            order: Order::Dmy,
            date_sep: '.',
            time_sep: ':',
            thousands: ' ',
        }
    }
}

/// The user's settings, read once.
pub fn get() -> &'static Locale {
    static LOCALE: OnceLock<Locale> = OnceLock::new();
    LOCALE.get_or_init(read)
}

#[cfg(windows)]
fn read() -> Locale {
    use windows_sys::Win32::Globalization::{
        GetLocaleInfoEx, LOCALE_IDATE, LOCALE_SDATE, LOCALE_SSHORTDATE, LOCALE_STHOUSAND,
        LOCALE_STIME,
    };
    let value = |kind: u32| -> String {
        let mut buf = [0u16; 128];
        // SAFETY: the user's locale (null name), a buffer and its size.
        let n =
            unsafe { GetLocaleInfoEx(std::ptr::null(), kind, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..(n.max(1) as usize - 1)])
    };
    let order = match value(LOCALE_IDATE).trim() {
        "0" => Order::Mdy,
        "1" => Order::Dmy,
        _ => Order::Ymd,
    };
    let first = |s: String, or: char| s.chars().next().unwrap_or(or);
    Locale {
        order,
        date_sep: date_separator(&value(LOCALE_SSHORTDATE))
            .unwrap_or_else(|| first(value(LOCALE_SDATE), '/')),
        time_sep: first(value(LOCALE_STIME), ':'),
        thousands: first(value(LOCALE_STHOUSAND), ','),
    }
}

#[cfg(not(windows))]
fn read() -> Locale {
    Locale::default()
}

/// The separator in a short date pattern (`dd.MM.yyyy`): one of `/-.`
/// first, else any; a leading day of the week is skipped (Far's
/// `get_date_separator`).
fn date_separator(pattern: &str) -> Option<char> {
    let mut s = pattern;
    if let Some(rest) = s.strip_prefix("ddd") {
        let rest = rest.trim_start_matches('d');
        s = rest.trim_start_matches(|c: char| !"dMyg".contains(c));
    }
    s.chars()
        .find(|c| "/-.".contains(*c))
        .or_else(|| s.chars().find(|c| !"dMyg".contains(*c)))
}

impl Locale {
    /// A date: the year with two digits or `full` (Far's panels: two).
    pub fn date(&self, t: &impl Datelike, full: bool) -> String {
        let y = if full { t.year() } else { t.year() % 100 };
        let (m, d, s) = (t.month(), t.day(), self.date_sep);
        let year = if full {
            format!("{y:04}")
        } else {
            format!("{y:02}")
        };
        match self.order {
            Order::Dmy => format!("{d:02}{s}{m:02}{s}{year}"),
            Order::Mdy => format!("{m:02}{s}{d:02}{s}{year}"),
            Order::Ymd => format!("{year}{s}{m:02}{s}{d:02}"),
        }
    }

    /// A day and a month without the year (Far's brief dates).
    pub fn day_month(&self, t: &impl Datelike) -> String {
        let (m, d, s) = (t.month(), t.day(), self.date_sep);
        match self.order {
            Order::Dmy => format!("{d:02}{s}{m:02}"),
            Order::Mdy | Order::Ymd => format!("{m:02}{s}{d:02}"),
        }
    }

    /// A time: hours and minutes, with `seconds` too.
    pub fn time(&self, t: &impl Timelike, seconds: bool) -> String {
        let s = self.time_sep;
        if seconds {
            format!("{:02}{s}{:02}{s}{:02}", t.hour(), t.minute(), t.second())
        } else {
            format!("{:02}{s}{:02}", t.hour(), t.minute())
        }
    }

    /// A number with its thousands apart.
    pub fn thousands(&self, n: u64) -> String {
        let s = n.to_string();
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i).is_multiple_of(3) {
                out.push(self.thousands);
            }
            out.push(c);
        }
        out
    }

    /// A date typed as `date` writes it (full year; a two-digit one is
    /// taken as 20xx).
    pub fn parse_date(&self, text: &str) -> Option<chrono::NaiveDate> {
        let parts: Vec<u32> = text
            .trim()
            .split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .map(|p| p.parse().ok())
            .collect::<Option<_>>()?;
        let [a, b, c] = parts[..] else {
            return None;
        };
        let (y, m, d) = match self.order {
            Order::Dmy => (c, b, a),
            Order::Mdy => (c, a, b),
            Order::Ymd => (a, b, c),
        };
        let y = if y < 100 { 2000 + y } else { y };
        chrono::NaiveDate::from_ymd_opt(y as i32, m, d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_times_and_numbers_by_the_settings() {
        let t = chrono::NaiveDate::from_ymd_opt(2026, 3, 9)
            .unwrap()
            .and_hms_opt(7, 5, 2)
            .unwrap();
        let ru = Locale::default();
        assert_eq!(ru.date(&t, false), "09.03.26");
        assert_eq!(ru.date(&t, true), "09.03.2026");
        assert_eq!(ru.time(&t, true), "07:05:02");
        assert_eq!(ru.thousands(1234567), "1 234 567");
        let us = Locale {
            order: Order::Mdy,
            date_sep: '/',
            thousands: ',',
            ..ru
        };
        assert_eq!(us.date(&t, false), "03/09/26");
        assert_eq!(us.thousands(4812), "4,812");
        let iso = Locale {
            order: Order::Ymd,
            date_sep: '-',
            ..ru
        };
        assert_eq!(iso.date(&t, true), "2026-03-09");
        assert_eq!(iso.parse_date("2026-03-09"), Some(t.date()));
        assert_eq!(us.parse_date("03/09/2026"), Some(t.date()));
        assert_eq!(date_separator("dd.MM.yyyy"), Some('.'));
        assert_eq!(date_separator("ddd, dd/MM/yyyy"), Some('/'));
        assert_eq!(date_separator("yyyy/M/d"), Some('/'));
    }
}
