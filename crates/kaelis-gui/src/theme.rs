//! Kaelis blue, and the two tones derived from it.
//!
//! The accent is fixed at build time. That keeps the shell predictable, makes
//! every highlight a constant instead of an atomic load in the paint path, and
//! removes a settings field, a picker, and an HSV round trip from the launcher.

use eframe::egui::Color32;

/// Kaelis blue. The one interactive colour in the interface.
pub const BLUE: Color32 = Color32::from_rgb(86, 140, 248);

/// 46 % white on `BLUE`: hairline strokes and small labels, which need to stay
/// legible on a near-black surface.
pub const BLUE_SOFT: Color32 = Color32::from_rgb(164, 193, 251);

/// Near-black painted on top of the blue fill. White would sit at about 3:1 on
/// this hue; near-black clears 7:1.
pub const BLUE_INK: Color32 = Color32::from_rgb(7, 9, 12);

/// The accent, named the way call sites read: a colour from the palette.
pub fn accent() -> Color32 {
    BLUE
}

/// The lighter companion used for hairline strokes and small accent labels.
pub fn accent_soft() -> Color32 {
    BLUE_SOFT
}

/// Text and glyphs painted *on* an accent fill.
pub fn accent_on() -> Color32 {
    BLUE_INK
}

/// The accent at a given alpha, for selection washes and hover tints.
pub fn accent_alpha(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(BLUE.r(), BLUE.g(), BLUE.b(), alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_blue_is_the_documented_one() {
        assert_eq!(accent(), Color32::from_rgb(86, 140, 248));
        assert_eq!(accent_alpha(64).to_array()[3], 64);
    }

    #[test]
    fn derived_tones_stay_readable() {
        // The soft tone is lighter than the base on every channel.
        let base = BLUE.to_array();
        let soft = BLUE_SOFT.to_array();
        for channel in 0..3 {
            assert!(soft[channel] > base[channel]);
        }
        // Blue is bright enough that dark ink is the legible choice.
        assert_eq!(accent_on(), BLUE_INK);
    }
}
