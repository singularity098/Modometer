// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! Colours and sizes, borrowed from Xythos: a near-black window, hairline outlines, Jost Light,
//! and Xythos's "Iris" gradient (#c688d6, #a85eea, #7278fe) for the highlights.

use femtovg::{Color, Paint};

pub const WIDTH: u32 = 800;
pub const HEIGHT: u32 = 450;

pub const FONT: &[u8] = include_bytes!("../../resources/fonts/Jost.ttf");

pub fn background() -> Color {
    Color::rgb(9, 8, 12)
}
pub fn text() -> Color {
    Color::rgba(236, 232, 244, 225)
}
pub fn dim() -> Color {
    Color::rgba(200, 194, 214, 120)
}
pub fn faint() -> Color {
    Color::rgba(200, 194, 214, 60)
}
pub fn hairline() -> Color {
    Color::rgba(255, 255, 255, 22)
}
pub fn hover_fill() -> Color {
    Color::rgba(255, 255, 255, 12)
}
pub fn panel_fill() -> Color {
    Color::rgba(255, 255, 255, 6)
}
pub fn popup_fill() -> Color {
    Color::rgba(18, 16, 24, 248)
}

pub const PINK: (u8, u8, u8) = (0xc6, 0x88, 0xd6);
pub const VIOLET: (u8, u8, u8) = (0xa8, 0x5e, 0xea);
pub const INDIGO: (u8, u8, u8) = (0x72, 0x78, 0xfe);

fn c((r, g, b): (u8, u8, u8), a: u8) -> Color {
    Color::rgba(r, g, b, a)
}

/// The Iris gradient from (x0, y0) to (x1, y1).
pub fn gradient(x0: f32, y0: f32, x1: f32, y1: f32, alpha: u8) -> Paint {
    Paint::linear_gradient_stops(
        x0,
        y0,
        x1,
        y1,
        [(0.0, c(PINK, alpha)), (0.55, c(VIOLET, alpha)), (1.0, c(INDIGO, alpha))],
    )
}

/// The gradient spread across the whole window, for outlines and bars (as in Xythos).
pub fn window_gradient(alpha: u8) -> Paint {
    gradient(0.0, 0.0, WIDTH as f32, HEIGHT as f32 * 0.4, alpha)
}

/// 1234567 -> "1,234,567"
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Minutes as "45 min", "3 h 05 min", or "1,234 h" from 100 hours.
pub fn duration(minutes: f64) -> String {
    let m = minutes.round() as u64;
    match m {
        0 if minutes > 0.0 => "< 1 min".to_string(),
        0..=59 => format!("{m} min"),
        60..=5_999 => format!("{} h {:02} min", m / 60, m % 60),
        _ => format!("{} h", commas((minutes / 60.0).round() as u64)),
    }
}

/// Minutes as "45m", "3h 05", "120h" for axis labels.
pub fn short_duration(minutes: f64) -> String {
    let m = minutes.round() as u64;
    match m {
        0..=59 => format!("{m}m"),
        60..=599 => format!("{}h {:02}", m / 60, m % 60),
        _ => format!("{}h", short((minutes / 60.0).round() as u64)),
    }
}

/// 1234 -> "1.2k" for axis labels.
pub fn short(n: u64) -> String {
    match n {
        0..=9_999 => commas(n),
        10_000..=999_999 => format!("{:.0}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

pub trait WithAlpha {
    fn with_alpha(self, alpha: u8) -> Color;
}

impl WithAlpha for Color {
    fn with_alpha(mut self, alpha: u8) -> Color {
        self.set_alpha(alpha);
        self
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn commas() {
        assert_eq!(super::commas(0), "0");
        assert_eq!(super::commas(999), "999");
        assert_eq!(super::commas(1000), "1,000");
        assert_eq!(super::commas(1234567), "1,234,567");
    }

    #[test]
    fn durations() {
        assert_eq!(super::duration(0.0), "0 min");
        assert_eq!(super::duration(0.2), "< 1 min");
        assert_eq!(super::duration(45.4), "45 min");
        assert_eq!(super::duration(185.0), "3 h 05 min");
        assert_eq!(super::duration(123_456.0), "2,058 h");
        assert_eq!(super::short_duration(185.0), "3h 05");
        assert_eq!(super::short_duration(1200.0), "20h");
    }
}
