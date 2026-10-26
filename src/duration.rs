//! "1h30m" <-> seconds.

use anyhow::{Result, bail};

/// Parses "30m", "1h", "1h30m", "90s", "-15m". A bare number is minutes.
pub fn parse(s: &str) -> Result<i64> {
    let (sign, body) = match s.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    if body.is_empty() {
        bail!("empty duration");
    }
    if let Ok(min) = body.parse::<i64>() {
        return Ok(sign * min * 60);
    }
    let mut total = 0i64;
    let mut num = String::new();
    for c in body.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let unit = match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => bail!("bad duration {s:?} (use e.g. 30m, 1h, 1h30m)"),
        };
        let n: i64 = num
            .parse()
            .map_err(|_| anyhow::anyhow!("bad duration {s:?}"))?;
        total += n * unit;
        num.clear();
    }
    if !num.is_empty() {
        bail!("bad duration {s:?}: trailing number without unit");
    }
    Ok(sign * total)
}

/// 5400 -> "1h 30m", 300 -> "5m", 42 -> "42s".
pub fn format(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    match (h, m) {
        (0, 0) => format!("{secs}s"),
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse("30").unwrap(), 1800);
        assert_eq!(parse("30m").unwrap(), 1800);
        assert_eq!(parse("+1h").unwrap(), 3600);
        assert_eq!(parse("1h30m").unwrap(), 5400);
        assert_eq!(parse("-15m").unwrap(), -900);
        assert_eq!(parse("90s").unwrap(), 90);
        assert!(parse("1x").is_err());
        assert!(parse("1h30").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn formats() {
        assert_eq!(format(5400), "1h 30m");
        assert_eq!(format(3600), "1h");
        assert_eq!(format(300), "5m");
        assert_eq!(format(42), "42s");
    }
}
