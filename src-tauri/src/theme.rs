//! CSS color parsing + luminance — direct port of the Swift helpers
//! (BrowserWindowController.parseCSSColor / cssColorIsDark / hexString).

/// Parse `#RGB`, `#RRGGBB`, `rgb(...)`, `rgba(...)` into components in [0, 1].
pub fn parse_css_color(css: &str) -> Option<(f64, f64, f64)> {
    let s = css.trim();
    if let Some(hex) = s.strip_prefix('#') {
        let hex: String = hex.trim().to_string();
        let parse2 = |a: &str| u8::from_str_radix(a, 16).ok().map(|v| v as f64 / 255.0);
        return match hex.len() {
            3 => {
                let d: Vec<String> = hex.chars().map(|c| format!("{c}{c}")).collect();
                Some((parse2(&d[0])?, parse2(&d[1])?, parse2(&d[2])?))
            }
            6 => Some((
                parse2(&hex[0..2])?,
                parse2(&hex[2..4])?,
                parse2(&hex[4..6])?,
            )),
            _ => None,
        };
    }
    if s.starts_with("rgb") {
        let inside = s.split('(').nth(1)?.split(')').next()?;
        // Computed styles use comma separation; accept space separation too.
        let parts: Vec<&str> = inside
            .split([',', ' ', '/'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() < 3 {
            return None;
        }
        let r: f64 = parts[0].parse().ok()?;
        let g: f64 = parts[1].parse().ok()?;
        let b: f64 = parts[2].parse().ok()?;
        return Some((r / 255.0, g / 255.0, b / 255.0));
    }
    None
}

/// WCAG-ish relative luminance (linear approximation — same as the Swift app).
pub fn luminance(r: f64, g: f64, b: f64) -> f64 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

pub fn is_dark(r: f64, g: f64, b: f64) -> bool {
    luminance(r, g, b) < 0.5
}

pub fn hex_string(r: f64, g: f64, b: f64) -> String {
    let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", to8(r), to8(g), to8(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_and_rgb() {
        assert_eq!(
            parse_css_color("#1a1a1a"),
            Some((26.0 / 255.0, 26.0 / 255.0, 26.0 / 255.0))
        );
        assert_eq!(parse_css_color("#fff"), Some((1.0, 1.0, 1.0)));
        let (r, g, b) = parse_css_color("rgb(26, 26, 26)").unwrap();
        assert!((r - 26.0 / 255.0).abs() < 1e-9 && g == r && b == r);
        assert!(parse_css_color("var(--bg)").is_none());
        assert!(parse_css_color("#12345").is_none());
    }

    #[test]
    fn dark_bisects() {
        assert!(is_dark(0.1, 0.1, 0.1));
        assert!(!is_dark(0.95, 0.94, 0.9));
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(
            hex_string(26.0 / 255.0, 26.0 / 255.0, 26.0 / 255.0),
            "#1A1A1A"
        );
    }
}
