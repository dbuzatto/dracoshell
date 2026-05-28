//! Maps alacritty cell colors to the renderer's RGBA color type. Uses the
//! One Dark palette so colors match what most devs see in VS Code / Atom.

use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor, Rgb};

/// 8-bit RGBA color. Replaces glyphon::Color so the renderer can stay
/// independent from the text-shaping library.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 0xff }
    }

    pub fn to_linear_f32(self) -> [f32; 4] {
        [
            srgb_byte_to_linear(self.r),
            srgb_byte_to_linear(self.g),
            srgb_byte_to_linear(self.b),
            self.a as f32 / 255.0,
        ]
    }
}

pub fn srgb_byte_to_linear(b: u8) -> f32 {
    let s = b as f32 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Cursor color stays fixed (Draco red) regardless of theme.
pub const CURSOR_RED: Color = Color::rgb(0xff, 0x2a, 0x2a);

fn ansi(idx: usize) -> Color {
    crate::themes::active().ansi[idx]
}

pub fn fg_default() -> Color {
    crate::themes::active().foreground
}

pub fn bg_default() -> Color {
    crate::themes::active().background
}

pub fn term_color(c: TermColor) -> Color {
    match c {
        TermColor::Spec(Rgb { r, g, b }) => Color { r, g, b, a: 0xff },
        TermColor::Indexed(idx) => indexed(idx),
        TermColor::Named(named) => named_color(named),
    }
}

fn indexed(idx: u8) -> Color {
    if (idx as usize) < 16 {
        ansi(idx as usize)
    } else if idx < 232 {
        let i = idx - 16;
        let r = (i / 36) % 6;
        let g = (i / 6) % 6;
        let b = i % 6;
        Color::rgb(cube(r), cube(g), cube(b))
    } else {
        let lvl = 8 + (idx - 232) as u16 * 10;
        let l = lvl.min(255) as u8;
        Color::rgb(l, l, l)
    }
}

fn cube(v: u8) -> u8 {
    if v == 0 {
        0
    } else {
        55 + v * 40
    }
}

fn named_color(n: NamedColor) -> Color {
    use NamedColor::*;
    match n {
        Foreground | BrightForeground => fg_default(),
        DimForeground => Color::rgb(0xa0, 0xa0, 0xa0),
        Background => bg_default(),
        Cursor => CURSOR_RED,
        Black => ansi(0),
        Red => ansi(1),
        Green => ansi(2),
        Yellow => ansi(3),
        Blue => ansi(4),
        Magenta => ansi(5),
        Cyan => ansi(6),
        White => ansi(7),
        BrightBlack => ansi(8),
        BrightRed => ansi(9),
        BrightGreen => ansi(10),
        BrightYellow => ansi(11),
        BrightBlue => ansi(12),
        BrightMagenta => ansi(13),
        BrightCyan => ansi(14),
        BrightWhite => ansi(15),
        DimBlack => ansi(0),
        DimRed => ansi(1),
        DimGreen => ansi(2),
        DimYellow => ansi(3),
        DimBlue => ansi(4),
        DimMagenta => ansi(5),
        DimCyan => ansi(6),
        DimWhite => ansi(7),
    }
}
