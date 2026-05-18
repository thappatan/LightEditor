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
//! We need to turn each into an `(r, g, b)` triple the renderer can
//! hand to cosmic-text. The 16-colour palette and the three pane
//! sentinels (Foreground / Background / Cursor) come from the active
//! [`Theme`](editor_config::Theme)'s `[terminal]` section via
//! [`PaletteContext`]; [`DEFAULT_ANSI_16`] is kept as the Tango-ish
//! fallback for any slot the theme leaves blank.

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
/// background. Used as the fallback when a theme doesn't override the
/// `[terminal]` palette entries.
pub const DEFAULT_ANSI_16: [PaletteColor; 16] = [
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

/// Snapshot of every colour the cell-level [`resolve`] needs to turn
/// an alacritty [`AnsiColor`] into RGB. Built once per
/// `refresh_terminal_text` from the active theme so per-cell lookups
/// stay arithmetic; bundle includes the 16 ANSI slots plus the three
/// `Foreground` / `Background` / `Cursor` sentinels that the chrome
/// owns.
#[derive(Debug, Clone, Copy)]
pub struct PaletteContext {
    /// 16-colour palette, indexed in `NamedColor`'s numeric order.
    pub ansi_16: [PaletteColor; 16],
    /// Theme-owned editor text colour. Returned for `Named(Foreground)`.
    pub default_fg: PaletteColor,
    /// Theme-owned editor background. Returned for `Named(Background)`.
    pub default_bg: PaletteColor,
    /// Theme-owned caret colour. Returned for `Named(Cursor)`.
    pub default_cursor: PaletteColor,
}

impl PaletteContext {
    /// Convenience constructor for tests / quick callers — uses the
    /// built-in palette and the supplied chrome sentinels.
    #[cfg(test)]
    pub fn with_defaults(
        default_fg: PaletteColor,
        default_bg: PaletteColor,
        default_cursor: PaletteColor,
    ) -> Self {
        Self {
            ansi_16: DEFAULT_ANSI_16,
            default_fg,
            default_bg,
            default_cursor,
        }
    }
}

/// Build an xterm 256-palette entry from its index, using `palette`
/// for the 16 named colours and a computed value for the higher slots.
///
/// - `0..=15`  → entry of `palette` at that index.
/// - `16..=231` → 6×6×6 RGB cube. Each channel is one of six levels
///   `[0, 95, 135, 175, 215, 255]`.
/// - `232..=255` → 24-step greyscale ramp from 8 to 238 in steps of 10.
pub fn xterm_256(idx: u8, palette: &[PaletteColor; 16]) -> PaletteColor {
    match idx {
        0..=15 => palette[idx as usize],
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
/// fall back to the chrome defaults on `ctx`; the 16 ANSI names index
/// into the theme's palette.
pub fn named_color(name: NamedColor, ctx: &PaletteContext) -> PaletteColor {
    let p = &ctx.ansi_16;
    match name {
        NamedColor::Black => p[0],
        NamedColor::Red => p[1],
        NamedColor::Green => p[2],
        NamedColor::Yellow => p[3],
        NamedColor::Blue => p[4],
        NamedColor::Magenta => p[5],
        NamedColor::Cyan => p[6],
        NamedColor::White => p[7],
        NamedColor::BrightBlack => p[8],
        NamedColor::BrightRed => p[9],
        NamedColor::BrightGreen => p[10],
        NamedColor::BrightYellow => p[11],
        NamedColor::BrightBlue => p[12],
        NamedColor::BrightMagenta => p[13],
        NamedColor::BrightCyan => p[14],
        NamedColor::BrightWhite => p[15],

        // Dim* variants reuse the matching named entry. A dedicated
        // dim palette is a follow-up — for now `bold off; dim on`
        // just renders the standard colour.
        NamedColor::DimBlack => p[0],
        NamedColor::DimRed => p[1],
        NamedColor::DimGreen => p[2],
        NamedColor::DimYellow => p[3],
        NamedColor::DimBlue => p[4],
        NamedColor::DimMagenta => p[5],
        NamedColor::DimCyan => p[6],
        NamedColor::DimWhite => p[7],

        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
            ctx.default_fg
        }
        NamedColor::Background => ctx.default_bg,
        NamedColor::Cursor => ctx.default_cursor,
    }
}

/// Resolve any [`AnsiColor`] (the cell-level enum) to an RGB triple
/// using the theme-supplied [`PaletteContext`]. This is the only
/// entry-point the renderer calls per cell.
pub fn resolve(color: AnsiColor, ctx: &PaletteContext) -> PaletteColor {
    match color {
        AnsiColor::Named(name) => named_color(name, ctx),
        AnsiColor::Indexed(i) => xterm_256(i, &ctx.ansi_16),
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
    fn ctx() -> PaletteContext {
        PaletteContext::with_defaults(fg(), bg(), cur())
    }

    #[test]
    fn xterm_256_first_block_matches_ansi_16() {
        for i in 0..16u8 {
            assert_eq!(
                xterm_256(i, &DEFAULT_ANSI_16),
                DEFAULT_ANSI_16[i as usize],
                "idx {i}"
            );
        }
    }

    #[test]
    fn xterm_256_cube_corners() {
        // 16 = (0,0,0) — pure black corner of the cube.
        assert_eq!(xterm_256(16, &DEFAULT_ANSI_16), PaletteColor::new(0, 0, 0));
        // 231 = (255,255,255) — pure white corner.
        assert_eq!(
            xterm_256(231, &DEFAULT_ANSI_16),
            PaletteColor::new(255, 255, 255)
        );
        // 21 = (0,0,255) — a single channel maxed.
        assert_eq!(
            xterm_256(21, &DEFAULT_ANSI_16),
            PaletteColor::new(0, 0, 255)
        );
        // 196 = (255,0,0).
        assert_eq!(
            xterm_256(196, &DEFAULT_ANSI_16),
            PaletteColor::new(255, 0, 0)
        );
        // 46 = (0,255,0).
        assert_eq!(
            xterm_256(46, &DEFAULT_ANSI_16),
            PaletteColor::new(0, 255, 0)
        );
    }

    #[test]
    fn xterm_256_greyscale_ramp_endpoints() {
        // First grey: 232 → 8.
        assert_eq!(xterm_256(232, &DEFAULT_ANSI_16), PaletteColor::new(8, 8, 8));
        // Last grey: 255 → 238.
        assert_eq!(
            xterm_256(255, &DEFAULT_ANSI_16),
            PaletteColor::new(238, 238, 238)
        );
    }

    #[test]
    fn named_foreground_uses_defaults() {
        let c = ctx();
        assert_eq!(named_color(NamedColor::Foreground, &c), fg());
        assert_eq!(named_color(NamedColor::Background, &c), bg());
        assert_eq!(named_color(NamedColor::Cursor, &c), cur());
    }

    #[test]
    fn named_red_is_palette_red() {
        let c = ctx();
        assert_eq!(named_color(NamedColor::Red, &c), DEFAULT_ANSI_16[1]);
        assert_eq!(named_color(NamedColor::BrightRed, &c), DEFAULT_ANSI_16[9]);
    }

    #[test]
    fn theme_override_changes_ansi_red() {
        // Build a context with a custom palette so we can verify the
        // resolver actually reads from `ctx.ansi_16` rather than the
        // module-level default.
        let mut custom = DEFAULT_ANSI_16;
        custom[1] = PaletteColor::new(0x99, 0x00, 0x00); // override Red
        let c = PaletteContext {
            ansi_16: custom,
            default_fg: fg(),
            default_bg: bg(),
            default_cursor: cur(),
        };
        assert_eq!(
            named_color(NamedColor::Red, &c),
            PaletteColor::new(0x99, 0x00, 0x00),
        );
    }

    #[test]
    fn resolve_handles_spec() {
        let rgb = Rgb { r: 1, g: 2, b: 3 };
        assert_eq!(
            resolve(AnsiColor::Spec(rgb), &ctx()),
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
