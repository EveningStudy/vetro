use std::path::PathBuf;

pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(TABLE[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                let b = (h * 16 + l) as u8;
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `file:///d%3A/x/y` → `D:\x\y` on Windows, `file:///home/x` → `/home/x` elsewhere.
/// Plain paths are passed through.
pub fn file_uri_to_path(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let path = match raw.strip_prefix("file://") {
        Some(rest) => percent_decode(rest),
        None if raw.contains("://") => return None,
        None => raw.to_owned(),
    };
    let drive =
        |s: &str| s.len() > 1 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':';
    let path = match path.strip_prefix('/') {
        Some(rest) if drive(rest) => rest,
        _ => &path[..],
    };
    if drive(path) || path.starts_with("\\\\") {
        let mut s = path.replace('/', "\\");
        s[..1].make_ascii_uppercase();
        if s.len() > 3 && s.ends_with('\\') {
            s.pop();
        }
        return Some(PathBuf::from(s));
    }
    let trimmed = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    Some(PathBuf::from(trimmed))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-11-01 01:02:44.8475682+00:00` (Go `time.Time` as stored by GORM) → ms since the epoch.
/// The zero time `0001-01-01 ...` and anything unparsable yield `None`.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if y < 1971 {
        return None;
    }
    let mut rest = s.get(19..)?;
    let mut ms = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.chars().take_while(char::is_ascii_digit).count();
        let padded = format!("{:0<3}", &frac[..digits.min(3)]);
        ms = padded.parse::<i64>().ok()?;
        rest = &frac[digits..];
    }
    let offset = match rest.trim() {
        "" | "Z" | "+00:00" => 0,
        o => {
            let sign = match o.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hh = o.get(1..3)?.parse::<i64>().ok()?;
            let mm = o.get(4..6).and_then(|m| m.parse::<i64>().ok()).unwrap_or(0);
            sign * (hh * 3600 + mm * 60)
        }
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset;
    Some(secs * 1000 + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn parses_store_times() {
        assert_eq!(parse_time("1970-01-01 00:00:00+00:00"), None);
        assert_eq!(
            parse_time("2026-11-01 01:02:44.8475682+00:00"),
            Some(1_793_494_964_847)
        );
        assert_eq!(
            parse_time("2026-11-01 03:02:44+02:00"),
            Some(1_793_494_964_000)
        );
        assert_eq!(parse_time("0001-01-01 00:00:00+00:00"), None);
        assert_eq!(parse_time("garbage"), None);
    }

    #[test]
    fn converts_file_uris() {
        if cfg!(windows) {
            assert_eq!(
                file_uri_to_path("file:///d%3A/Dev/x%20y"),
                Some(PathBuf::from(r"D:\Dev\x y"))
            );
            assert_eq!(file_uri_to_path("c:/a/"), Some(PathBuf::from(r"C:\a")));
        }
        assert_eq!(
            file_uri_to_path("file:///home/u/p/"),
            Some(PathBuf::from("/home/u/p"))
        );
        assert_eq!(file_uri_to_path("https://x"), None);
        assert_eq!(file_uri_to_path(""), None);
    }
}
