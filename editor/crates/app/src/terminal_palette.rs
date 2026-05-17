//! ANSI / xterm colour conversion for the embedded terminal pane.
//!
//! `alacritty_terminal` reports per-cell foreground / background as a
//! [`vte::ansi::Color`] which is one of three shapes:
//!
//! - `Named(NamedColor)` — the 16 standard names + Foreground / Background
//!   / Cursor / Dim* / Bright* sentinels;
//! - `Indexed(u8)` — an entry in the xterm 256-colour palette;
//! - `Spec(Rgb)` — a true-colour triple emitted by the program itself.
//!
//! We need to turn each into an `(r, g, b)` triple the renderer can hand
//! to cosmic-text. The palette itself is hardcoded to a Tango-ish set
//! (close to the macOS Terminal defaults) so this PR doesn't have to
//! touch the theme schema — the theme integration is a separate
//! follow-up.

use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};

/// RGB triple in the renderer's natural form. Identical to `vte::Rgb`
/// minus the trait baggage, so we can mix it freely with default
/// values and the inversion path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl PaletteColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn from_rgb(rgb: Rgb) -> Self {
        Self {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        }
    }
}

/// The 16 named ANSI colours, in `NamedColor`'s numeric order so a
/// `u8` cast indexes directly. Tango-ish palette — same family as the
/// macOS Terminal "Basic" profile, tuned for legibility on a dark
/// background.
const ANSI_16: [PaletteColor; 16] = [
    PaletteColor::new(0x00, 0x00, 0x00), // 0  Black
    PaletteColor::new(0xCC, 0x00, 0x00), // 1  Red
    PaletteColor::new(0x4E, 0x9A, 0x06), // 2  Green
    PaletteColor::new(0xC4, 0xA0, 0x00), // 3  Yellow
    PaletteColor::new(0x34, 0x65, 0xA4), // 4  Blue
    PaletteColor::new(0x75, 0x50, 0x7B), // 5  Magenta
    PaletteColor::new(0x06, 0x98, 0x9A), // 6  Cyan
    PaletteColor::new(0xD3, 0xD7, 0xCF), // 7  White
    PaletteColor::new(0x55, 0x57, 0x53), // 8  BrightBlack
    PaletteColor::new(0xEF, 0x29, 0x29), // 9  BrightRed
    PaletteColor::new(0x8A, 0xE2, 0x34), // 10 BrightGreen
    PaletteColor::new(0xFC, 0xE9, 0x4F), // 11 BrightYellow
    PaletteColor::new(0x72, 0x9F, 0xCF), // 12 BrightBlue
    PaletteColor::new(0xAD, 0x7F, 0xA8), // 13 BrightMagenta
    PaletteColor::new(0x34, 0xE2, 0xE2), // 14 BrightCyan
    PaletteColor::new(0xEE, 0xEE, 0xEC), // 15 BrightWhite
];

/// Build an xterm 256-palette entry from its index.
///
/// - `0..=15`  → the 16 named ANSI colours.
/// - `16..=231` → 6×6×6 RGB cube. Each channel is one of six levels
///   `[0, 95, 135, 175, 215, 255]`.
/// - `232..=255` → 24-step greyscale ramp from 8 to 238 in steps of 10.
pub fn xterm_256(idx: u8) -> PaletteColor {
    match idx {
        0..=15 => ANSI_16[idx as usize],
        16..=231 => {
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let n = idx - 16;
            let r = LEVELS[(n / 36) as usize];
            let g = LEVELS[((n / 6) % 6) as usize];
            let b = LEVELS[(n % 6) as usize];
            PaletteColor::new(r, g, b)
        }
        232..=255 => {
            let v = 8u16 + (idx as u16 - 232) * 10;
            let v = v.min(255) as u8;
            PaletteColor::new(v, v, v)
        }
    }
}

/// Resolve a `NamedColor` to RGB. `Foreground` / `Background` / `Cursor`
/// fall back to the chrome's defaults supplied by the caller — the
/// theme owns those colours, not us.
pub fn named_color(
    name: NamedColor,
    default_fg: PaletteColor,
    default_bg: PaletteColor,
    default_cursor: PaletteColor,
) -> PaletteColor {
    match name {
        // The 16 standard colours map straight through.
        NamedColor::Black => ANSI_16[0],
        NamedColor::Red => ANSI_16[1],
        NamedColor::Green => ANSI_16[2],
        NamedColor::Yellow => ANSI_16[3],
        NamedColor::Blue => ANSI_16[4],
        NamedColor::Magenta => ANSI_16[5],
        NamedColor::Cyan => ANSI_16[6],
        NamedColor::White => ANSI_16[7],
        NamedColor::BrightBlack => ANSI_16[8],
        NamedColor::BrightRed => ANSI_16[9],
        NamedColor::BrightGreen => ANSI_16[10],
        NamedColor::BrightYellow => ANSI_16[11],
        NamedColor::BrightBlue => ANSI_16[12],
        NamedColor::BrightMagenta => ANSI_16[13],
        NamedColor::BrightCyan => ANSI_16[14],
        NamedColor::BrightWhite => ANSI_16[15],

        // Dim* variants are the matching dim colour. We don't have a
        // dimmed palette, so reuse the bright→named lookup that vte
        // ships and then resolve recursively. The two-step keeps the
        // mapping in one place if we ever want to tune dim colours
        // separately.
        NamedColor::DimBlack => ANSI_16[0],
        NamedColor::DimRed => ANSI_16[1],
        NamedColor::DimGreen => ANSI_16[2],
        NamedColor::DimYellow => ANSI_16[3],
        NamedColor::DimBlue => ANSI_16[4],
        NamedColor::DimMagenta => ANSI_16[5],
        NamedColor::DimCyan => ANSI_16[6],
        NamedColor::DimWhite => ANSI_16[7],

        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            default_fg
        }
        NamedColor::Background => default_bg,
        NamedColor::Cursor => default_cursor,
    }
}

/// Resolve any [`AnsiColor`] (the cell-level enum) to an RGB triple,
/// given the chrome defaults to use for `Named(Foreground / Background /
/// Cursor)`. This is the only entry-point the renderer calls per cell.
pub fn resolve(
    color: AnsiColor,
    default_fg: PaletteColor,
    default_bg: PaletteColor,
    default_cursor: PaletteColor,
) -> PaletteColor {
    match color {
        AnsiColor::Named(name) => named_color(name, default_fg, default_bg, default_cursor),
        AnsiColor::Indexed(i) => xterm_256(i),
        AnsiColor::Spec(rgb) => PaletteColor::from_rgb(rgb),
    }
}

/// Apply the BOLD flag's classic xterm behaviour: bold text uses the
/// *bright* variant of a named colour. Indexed and Spec colours are
/// left alone, matching iTerm / xterm defaults — programs that emit a
/// true-colour escape are presumed to mean exactly that colour.
pub fn brighten_named(color: AnsiColor) -> AnsiColor {
    match color {
        AnsiColor::Named(n) => AnsiColor::Named(n.to_bright()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg() -> PaletteColor {
        PaletteColor::new(0xAA, 0xAA, 0xAA)
    }
    fn bg() -> PaletteColor {
        PaletteColor::new(0x11, 0x11, 0x11)
    }
    fn cur() -> PaletteColor {
        PaletteColor::new(0xFF, 0x00, 0xFF)
    }

    #[test]
    fn xterm_256_first_block_matches_ansi_16() {
        for i in 0..16u8 {
            assert_eq!(xterm_256(i), ANSI_16[i as usize], "idx {i}");
        }
    }

    #[test]
    fn xterm_256_cube_corners() {
        // 16 = (0,0,0) — pure black corner of the cube.
        assert_eq!(xterm_256(16), PaletteColor::new(0, 0, 0));
        // 231 = (255,255,255) — pure white corner.
        assert_eq!(xterm_256(231), PaletteColor::new(255, 255, 255));
        // 21 = (0,0,255) — a single channel maxed.
        assert_eq!(xterm_256(21), PaletteColor::new(0, 0, 255));
        // 196 = (255,0,0).
        assert_eq!(xterm_256(196), PaletteColor::new(255, 0, 0));
        // 46 = (0,255,0).
        assert_eq!(xterm_256(46), PaletteColor::new(0, 255, 0));
    }

    #[test]
    fn xterm_256_greyscale_ramp_endpoints() {
        // First grey: 232 → 8.
        assert_eq!(xterm_256(232), PaletteColor::new(8, 8, 8));
        // Last grey: 255 → 238.
        assert_eq!(xterm_256(255), PaletteColor::new(238, 238, 238));
    }

    #[test]
    fn named_foreground_uses_defaults() {
        assert_eq!(named_color(NamedColor::Foreground, fg(), bg(), cur()), fg());
        assert_eq!(named_color(NamedColor::Background, fg(), bg(), cur()), bg());
        assert_eq!(named_color(NamedColor::Cursor, fg(), bg(), cur()), cur());
    }

    #[test]
    fn named_red_is_palette_red() {
        assert_eq!(named_color(NamedColor::Red, fg(), bg(), cur()), ANSI_16[1]);
        assert_eq!(
            named_color(NamedColor::BrightRed, fg(), bg(), cur()),
            ANSI_16[9]
        );
    }

    #[test]
    fn resolve_handles_spec() {
        let rgb = Rgb { r: 1, g: 2, b: 3 };
        assert_eq!(
            resolve(AnsiColor::Spec(rgb), fg(), bg(), cur()),
            PaletteColor::new(1, 2, 3)
        );
    }

    #[test]
    fn brighten_named_lifts_red_to_bright_red() {
        let bright = brighten_named(AnsiColor::Named(NamedColor::Red));
        assert_eq!(bright, AnsiColor::Named(NamedColor::BrightRed));
    }

    #[test]
    fn brighten_named_passes_indexed_through() {
        let same = brighten_named(AnsiColor::Indexed(42));
        assert_eq!(same, AnsiColor::Indexed(42));
    }
}
