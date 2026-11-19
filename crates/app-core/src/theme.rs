//! The colours of the app's themes, and their contrast (WCAG 2.2 §1.4.3, §1.4.11): text
//! needs 4.5:1 against what it is drawn on, the parts of controls 3:1.

/// Which set of colours the window uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Light,
    Dark,
    HighContrastDark,
    HighContrastLight,
}

/// An sRGB colour with alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

const fn hex(v: u32) -> Rgba {
    Rgba((v >> 16) as u8, (v >> 8) as u8, v as u8, 255)
}

const fn hexa(v: u32) -> Rgba {
    Rgba((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// The colour tokens the UI draws with (`Theme` in theme.slint).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub high_contrast: bool,
    pub text: Rgba,
    pub muted: Rgba,
    /// Hints, placeholders and shortcuts: still readable text.
    pub faint: Rgba,
    /// Dividers; decoration only.
    pub border: Rgba,
    /// The outline of inputs, which must be seen to be found.
    pub control_border: Rgba,
    pub chrome: Rgba,
    pub canvas: Rgba,
    pub track: Rgba,
    pub raised: Rgba,
    pub hover: Rgba,
    pub pressed: Rgba,
    /// Fills behind `on_accent` text.
    pub accent: Rgba,
    pub accent_hover: Rgba,
    pub accent_soft: Rgba,
    /// Accent coloured text and icons on the chrome.
    pub accent_fg: Rgba,
    pub on_accent: Rgba,
    pub focus: Rgba,
    pub danger: Rgba,
    pub success: Rgba,
    pub warning: Rgba,
    pub danger_soft: Rgba,
    pub success_soft: Rgba,
    pub warning_soft: Rgba,
    pub shadow_near: Rgba,
    pub shadow_far: Rgba,
    pub scrim: Rgba,
}

impl Look {
    pub fn palette(self) -> Palette {
        match self {
            Look::Light => Palette {
                dark: false,
                high_contrast: false,
                text: hex(0x18181B),
                muted: hex(0x52525B),
                faint: hex(0x6B6B73),
                border: hex(0xE4E4E7),
                control_border: hex(0x8A8A93),
                chrome: hex(0xFFFFFF),
                canvas: hex(0xF1F1F3),
                track: hex(0xF1F1F3),
                raised: hex(0xFFFFFF),
                hover: hex(0xF4F4F5),
                pressed: hex(0xE4E4E7),
                accent: hex(0x2563EB),
                accent_hover: hex(0x1D4ED8),
                accent_soft: hex(0xEAF1FF),
                accent_fg: hex(0x2563EB),
                on_accent: hex(0xFFFFFF),
                focus: hex(0x2563EB),
                danger: hex(0xB91C1C),
                success: hex(0x15803D),
                warning: hex(0xB45309),
                danger_soft: hex(0xFEF2F2),
                success_soft: hex(0xECFDF3),
                warning_soft: hex(0xFFF7E6),
                shadow_near: hexa(0x18181B1C),
                shadow_far: hexa(0x18181B0C),
                scrim: hexa(0x18181B66),
            },
            Look::Dark => Palette {
                dark: true,
                high_contrast: false,
                text: hex(0xF4F4F5),
                muted: hex(0xA1A1AA),
                faint: hex(0x8E8E96),
                border: hex(0x2E2E33),
                control_border: hex(0x76767F),
                chrome: hex(0x1E1E21),
                canvas: hex(0x141416),
                track: hex(0x2A2A2E),
                raised: hex(0x3A3A40),
                hover: hex(0x2A2A2E),
                pressed: hex(0x34343A),
                accent: hex(0x2563EB),
                accent_hover: hex(0x1D4ED8),
                accent_soft: hex(0x1E3157),
                accent_fg: hex(0x60A5FA),
                on_accent: hex(0xFFFFFF),
                focus: hex(0x60A5FA),
                danger: hex(0xF87171),
                success: hex(0x4ADE80),
                warning: hex(0xFBBF24),
                danger_soft: hex(0x3F1D1D),
                success_soft: hex(0x16301F),
                warning_soft: hex(0x3A2A0D),
                shadow_near: hexa(0x00000090),
                shadow_far: hexa(0x00000050),
                scrim: hexa(0x000000A0),
            },
            Look::HighContrastDark => Palette {
                dark: true,
                high_contrast: true,
                text: hex(0xFFFFFF),
                muted: hex(0xEDEDED),
                faint: hex(0xD4D4D4),
                border: hex(0xFFFFFF),
                control_border: hex(0xFFFFFF),
                chrome: hex(0x000000),
                canvas: hex(0x000000),
                track: hex(0x1A1A1A),
                raised: hex(0x333333),
                hover: hex(0x262626),
                pressed: hex(0x3D3D3D),
                accent: hex(0xFFD60A),
                accent_hover: hex(0xFFE45C),
                accent_soft: hex(0x3D3300),
                accent_fg: hex(0xFFD60A),
                on_accent: hex(0x000000),
                focus: hex(0xFFD60A),
                danger: hex(0xFF8A8A),
                success: hex(0x7CFC9A),
                warning: hex(0xFFD60A),
                danger_soft: hex(0x330000),
                success_soft: hex(0x002B0F),
                warning_soft: hex(0x332B00),
                shadow_near: hexa(0x00000000),
                shadow_far: hexa(0x00000000),
                scrim: hexa(0x000000C8),
            },
            Look::HighContrastLight => Palette {
                dark: false,
                high_contrast: true,
                text: hex(0x000000),
                muted: hex(0x1A1A1A),
                faint: hex(0x333333),
                border: hex(0x000000),
                control_border: hex(0x000000),
                chrome: hex(0xFFFFFF),
                canvas: hex(0xD4D4D4),
                track: hex(0xEDEDED),
                raised: hex(0xFFFFFF),
                hover: hex(0xDDDDDD),
                pressed: hex(0xC8C8C8),
                accent: hex(0x0030B0),
                accent_hover: hex(0x00208A),
                accent_soft: hex(0xD9E2FF),
                accent_fg: hex(0x0030B0),
                on_accent: hex(0xFFFFFF),
                focus: hex(0x0030B0),
                danger: hex(0xA00000),
                success: hex(0x006400),
                warning: hex(0x7A4A00),
                danger_soft: hex(0xFFE5E5),
                success_soft: hex(0xE0F5E0),
                warning_soft: hex(0xFFF0D6),
                shadow_near: hexa(0x00000000),
                shadow_far: hexa(0x00000000),
                scrim: hexa(0x000000A0),
            },
        }
    }
}

/// Relative luminance of an opaque colour.
pub fn luminance(c: Rgba) -> f64 {
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
}

/// The contrast ratio of two opaque colours, 1 to 21.
pub fn contrast(a: Rgba, b: Rgba) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `top` drawn over the opaque `under`.
pub fn over(top: Rgba, under: Rgba) -> Rgba {
    let a = f64::from(top.3) / 255.0;
    let mix = |t: u8, u: u8| (f64::from(t) * a + f64::from(u) * (1.0 - a)).round() as u8;
    Rgba(
        mix(top.0, under.0),
        mix(top.1, under.1),
        mix(top.2, under.2),
        255,
    )
}

/// What must stand out from what, and by how much: (what, foreground, background, minimum).
pub fn checks(p: &Palette) -> Vec<(&'static str, Rgba, Rgba, f64)> {
    const TEXT: f64 = 4.5;
    const PART: f64 = 3.0;
    vec![
        ("text on chrome", p.text, p.chrome, TEXT),
        ("text on canvas", p.text, p.canvas, TEXT),
        ("text on hover", p.text, p.hover, TEXT),
        ("text on pressed", p.text, p.pressed, TEXT),
        ("text on raised", p.text, p.raised, TEXT),
        ("text on track", p.text, p.track, TEXT),
        ("text on accent-soft", p.text, p.accent_soft, TEXT),
        ("muted on chrome", p.muted, p.chrome, TEXT),
        ("muted on canvas", p.muted, p.canvas, TEXT),
        ("muted on track", p.muted, p.track, TEXT),
        ("muted on hover", p.muted, p.hover, TEXT),
        ("faint on chrome", p.faint, p.chrome, TEXT),
        ("faint on canvas", p.faint, p.canvas, TEXT),
        ("accent text on chrome", p.accent_fg, p.chrome, TEXT),
        (
            "accent text on accent-soft",
            p.accent_fg,
            p.accent_soft,
            TEXT,
        ),
        ("on-accent on accent", p.on_accent, p.accent, TEXT),
        (
            "on-accent on accent-hover",
            p.on_accent,
            p.accent_hover,
            TEXT,
        ),
        ("danger on chrome", p.danger, p.chrome, TEXT),
        ("danger on danger-soft", p.danger, p.danger_soft, TEXT),
        ("success on success-soft", p.success, p.success_soft, TEXT),
        ("warning on warning-soft", p.warning, p.warning_soft, TEXT),
        ("text on danger-soft", p.text, p.danger_soft, TEXT),
        ("text on success-soft", p.text, p.success_soft, TEXT),
        ("text on warning-soft", p.text, p.warning_soft, TEXT),
        ("input outline on chrome", p.control_border, p.chrome, PART),
        ("focus ring on chrome", p.focus, p.chrome, PART),
        ("focus ring on canvas", p.focus, p.canvas, PART),
        ("selected outline on chrome", p.accent_fg, p.chrome, PART),
        ("check box outline on chrome", p.muted, p.chrome, PART),
        ("primary button on chrome", p.accent, p.chrome, PART),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios() {
        assert!((contrast(hex(0xFFFFFF), hex(0x000000)) - 21.0).abs() < 1e-9);
        assert!((contrast(hex(0x777777), hex(0xFFFFFF)) - 4.48).abs() < 0.01);
        assert_eq!(over(hexa(0x00000080), hex(0xFFFFFF)), hex(0x7F7F7F));
    }

    #[test]
    fn every_theme_meets_wcag_aa() {
        let mut failures = Vec::new();
        for look in [
            Look::Light,
            Look::Dark,
            Look::HighContrastDark,
            Look::HighContrastLight,
        ] {
            let p = look.palette();
            for (what, fg, bg, min) in checks(&p) {
                let r = contrast(fg, bg);
                if r < min {
                    failures.push(format!("{look:?}: {what} is {r:.2}:1, needs {min}:1"));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn high_contrast_goes_further() {
        for look in [Look::HighContrastDark, Look::HighContrastLight] {
            let p = look.palette();
            assert!(contrast(p.text, p.chrome) >= 15.0);
            assert!(contrast(p.muted, p.chrome) >= 7.0, "{look:?} muted");
            assert!(contrast(p.faint, p.chrome) >= 7.0, "{look:?} faint");
            assert!(contrast(p.control_border, p.chrome) >= 7.0);
        }
    }
}
