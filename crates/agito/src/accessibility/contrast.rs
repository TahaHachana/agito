//! WCAG colour contrast: parsing CSS colours, blending translucent layers, and the contrast
//! ratio with its thresholds.

/// WCAG AA's minimum ratio for normal text.
pub const NORMAL_TEXT: f64 = 4.5;
/// WCAG AA's minimum ratio for large text: at least 24px, or bold and at least 18.66px.
pub const LARGE_TEXT: f64 = 3.0;

/// A colour with straight (not premultiplied) alpha; channels 0-255, alpha 0-1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    /// Red, 0-255.
    pub r: f64,
    /// Green, 0-255.
    pub g: f64,
    /// Blue, 0-255.
    pub b: f64,
    /// Opacity, 0 (transparent) to 1 (opaque).
    pub a: f64,
}

impl Rgba {
    /// Opaque white: the canvas behind a page that paints no background.
    pub const WHITE: Rgba = Rgba {
        r: 255.0,
        g: 255.0,
        b: 255.0,
        a: 1.0,
    };

    /// Parses a computed CSS colour: `rgb()`/`rgba()` in either syntax, `#rgb`, `#rgba`,
    /// `#rrggbb`, `#rrggbbaa`, or `transparent`. Other forms, such as `color(display-p3 …)`,
    /// are `None`: a colour that cannot be read is not guessed at.
    pub fn parse(css: &str) -> Option<Rgba> {
        let css = css.trim();
        if css.eq_ignore_ascii_case("transparent") {
            return Some(Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            });
        }
        if let Some(hex) = css.strip_prefix('#') {
            return parse_hex(hex);
        }
        let inner = css
            .strip_prefix("rgba(")
            .or_else(|| css.strip_prefix("rgb("))?
            .strip_suffix(')')?;
        // `rgb(1, 2, 3)`, `rgba(1, 2, 3, 0.5)`, `rgb(1 2 3)` or `rgb(1 2 3 / 0.5)`.
        let parts: Vec<&str> = inner
            .split([',', ' ', '/'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() < 3 || parts.len() > 4 {
            return None;
        }
        let channel = |p: &str| -> Option<f64> {
            match p.strip_suffix('%') {
                Some(pct) => pct.parse::<f64>().ok().map(|v| v * 2.55),
                None => p.parse::<f64>().ok(),
            }
        };
        let alpha = match parts.get(3) {
            None => 1.0,
            Some(p) => match p.strip_suffix('%') {
                Some(pct) => pct.parse::<f64>().ok()? / 100.0,
                None => p.parse::<f64>().ok()?,
            },
        };
        Some(Rgba {
            r: channel(parts[0])?.clamp(0.0, 255.0),
            g: channel(parts[1])?.clamp(0.0, 255.0),
            b: channel(parts[2])?.clamp(0.0, 255.0),
            a: alpha.clamp(0.0, 1.0),
        })
    }

    /// Whether nothing behind it shows through.
    pub fn is_opaque(&self) -> bool {
        self.a >= 1.0
    }

    /// This colour painted over an opaque `backdrop`.
    pub fn over(&self, backdrop: Rgba) -> Rgba {
        let mix = |top: f64, bottom: f64| top * self.a + bottom * (1.0 - self.a);
        Rgba {
            r: mix(self.r, backdrop.r),
            g: mix(self.g, backdrop.g),
            b: mix(self.b, backdrop.b),
            a: 1.0,
        }
    }

    /// WCAG relative luminance, 0 (black) to 1 (white), of an opaque colour.
    pub fn luminance(&self) -> f64 {
        let linear = |c: f64| {
            let c = c / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// `rgb(r, g, b)`, rounded, for messages.
    pub fn css(&self) -> String {
        format!(
            "rgb({}, {}, {})",
            self.r.round(),
            self.g.round(),
            self.b.round()
        )
    }
}

fn parse_hex(hex: &str) -> Option<Rgba> {
    let digit = |s: &str| u8::from_str_radix(s, 16).ok().map(f64::from);
    let (r, g, b, a) = match hex.len() {
        3 | 4 => {
            let d = |i: usize| digit(&hex[i..=i].repeat(2));
            (
                d(0)?,
                d(1)?,
                d(2)?,
                if hex.len() == 4 { d(3)? } else { 255.0 },
            )
        }
        6 | 8 => {
            let d = |i: usize| digit(&hex[i..i + 2]);
            (
                d(0)?,
                d(2)?,
                d(4)?,
                if hex.len() == 8 { d(6)? } else { 255.0 },
            )
        }
        _ => return None,
    };
    Some(Rgba {
        r,
        g,
        b,
        a: a / 255.0,
    })
}

/// The colour behind an element: its own background and its ancestors', nearest first, laid
/// over one another and over the white canvas. `None` when a layer cannot be read.
pub fn effective_background(layers: &[String]) -> Option<Rgba> {
    let mut parsed = Vec::with_capacity(layers.len());
    for layer in layers {
        let colour = Rgba::parse(layer)?;
        let opaque = colour.is_opaque();
        parsed.push(colour);
        if opaque {
            break;
        }
    }
    // Paint from the farthest layer up.
    Some(
        parsed
            .iter()
            .rev()
            .fold(Rgba::WHITE, |below, layer| layer.over(below)),
    )
}

/// The WCAG contrast ratio of two opaque colours: 1 (none) to 21 (black on white).
pub fn ratio(a: Rgba, b: Rgba) -> f64 {
    let (la, lb) = (a.luminance(), b.luminance());
    let (lighter, darker) = if la > lb { (la, lb) } else { (lb, la) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Whether text counts as large for WCAG: at least 18pt (24px), or bold and at least 14pt
/// (18.66px).
pub fn is_large(font_size_px: f64, font_weight: f64) -> bool {
    font_size_px >= 24.0 || (font_weight >= 700.0 && font_size_px >= 18.66)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse_in_every_computed_form() {
        let rgb = |r, g, b, a| Rgba { r, g, b, a };
        assert_eq!(Rgba::parse("rgb(1, 2, 3)"), Some(rgb(1.0, 2.0, 3.0, 1.0)));
        assert_eq!(
            Rgba::parse("rgba(1, 2, 3, 0.5)"),
            Some(rgb(1.0, 2.0, 3.0, 0.5))
        );
        assert_eq!(
            Rgba::parse("rgb(1 2 3 / 25%)"),
            Some(rgb(1.0, 2.0, 3.0, 0.25))
        );
        assert_eq!(Rgba::parse("rgba(0, 0, 0, 0)").map(|c| c.a), Some(0.0));
        assert_eq!(Rgba::parse("transparent").map(|c| c.a), Some(0.0));
        assert_eq!(Rgba::parse("#fff"), Some(Rgba::WHITE));
        assert_eq!(
            Rgba::parse("#00000080").map(|c| (c.a * 100.0).round()),
            Some(50.0)
        );
        assert_eq!(Rgba::parse("color(display-p3 1 0 0)"), None);
        assert_eq!(Rgba::parse("rgb(1, 2)"), None);
    }

    #[test]
    fn a_transparent_background_shows_what_is_behind_it() {
        // The bug this guards against: transparent read as black.
        let layers = [
            "rgba(0, 0, 0, 0)".to_string(),
            "rgba(0, 0, 0, 0)".to_string(),
        ];
        assert_eq!(effective_background(&layers), Some(Rgba::WHITE));

        // An opaque ancestor ends the walk; a translucent layer above it blends.
        let layers = [
            "rgba(0, 0, 0, 0.5)".to_string(),
            "rgb(255, 255, 255)".to_string(),
            "rgb(255, 0, 0)".to_string(),
        ];
        let bg = effective_background(&layers).unwrap();
        assert_eq!(bg.css(), "rgb(128, 128, 128)");

        assert_eq!(
            effective_background(&["oklch(0.5 0.1 120)".to_string()]),
            None
        );
    }

    #[test]
    fn ratios_match_wcag() {
        let black = Rgba::parse("#000").unwrap();
        assert!((ratio(black, Rgba::WHITE) - 21.0).abs() < 1e-9);
        assert!((ratio(Rgba::WHITE, Rgba::WHITE) - 1.0).abs() < 1e-9);
        // #767676 on white is the classic just-passing grey: 4.54:1.
        let grey = Rgba::parse("#767676").unwrap();
        let r = ratio(grey, Rgba::WHITE);
        assert!(r > NORMAL_TEXT && r < 4.6, "{r}");
        // #777 just fails.
        assert!(ratio(Rgba::parse("#777").unwrap(), Rgba::WHITE) < NORMAL_TEXT);
        // Default link blue on white passes.
        assert!(ratio(Rgba::parse("rgb(0, 0, 238)").unwrap(), Rgba::WHITE) > NORMAL_TEXT);
    }

    #[test]
    fn large_text_is_by_size_or_bold_size() {
        assert!(is_large(24.0, 400.0));
        assert!(!is_large(23.9, 400.0));
        assert!(is_large(18.67, 700.0));
        assert!(!is_large(18.67, 600.0));
        assert!(!is_large(16.0, 900.0));
    }
}
