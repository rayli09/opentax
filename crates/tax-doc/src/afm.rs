//! Advance widths (units of 1/1000 em) for the printable ASCII range
//! (codes 32..=126) of the standard PDF base-14 font families.
//!
//! Provenance: measured from the Liberation fonts (Liberation Sans /
//! Liberation Serif, installed system fonts) with fontTools. Liberation
//! Sans is metric-compatible with Arial, whose advance widths are the
//! Helvetica widths by design, and Liberation Serif is metric-compatible
//! with Times New Roman / Times. The extracted values were spot-checked
//! against the canonical Adobe AFM data (Helvetica: space 278, digit
//! 556, W 944; Times: space 250, digit 500, W 944) and agree.
//!
//! Used ONLY as a fallback when a font carries no `/Widths` array (the
//! normal case for base-14 fonts): widths affect word bounding boxes,
//! never extracted values directly.

/// Widths for codes 32..=126.
pub static HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Widths for codes 32..=126.
pub static HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

/// Widths for codes 32..=126.
pub static TIMES: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];

/// Widths for codes 32..=126.
pub static TIMES_BOLD: [u16; 95] = [
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 930, 722, 667, 722, 722, 667,
    611, 778, 778, 389, 500, 778, 667, 944, 722, 778, 611, 778, 722, 556, 667, 722, 722, 1000, 722,
    722, 667, 333, 278, 333, 581, 500, 333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556,
    278, 833, 556, 500, 556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520,
];

/// Courier is monospaced: every glyph advances 600 units.
pub const COURIER_WIDTH: u16 = 600;

/// Pick the fallback table for a `/BaseFont` name. Subset prefixes
/// (`ABCDEE+Helvetica-Bold`) are stripped. Returns `(table, is_courier)`.
pub fn table_for_base_font(base_font: &str) -> (Option<&'static [u16; 95]>, bool) {
    let name = base_font
        .rsplit('+')
        .next()
        .unwrap_or(base_font)
        .to_ascii_lowercase();
    let bold = name.contains("bold");
    if name.starts_with("courier") {
        (None, true)
    } else if name.contains("times") || name.contains("serif") && !name.contains("sans") {
        (Some(if bold { &TIMES_BOLD } else { &TIMES }), false)
    } else if name.contains("helvetica") || name.contains("arial") || name.contains("sans") {
        (Some(if bold { &HELVETICA_BOLD } else { &HELVETICA }), false)
    } else {
        (None, false)
    }
}

/// Width of one printable-ASCII char in the table, if it has one.
pub fn char_width(table: &[u16; 95], c: char) -> Option<u16> {
    let code = c as u32;
    if (32..=126).contains(&code) {
        Some(table[(code - 32) as usize])
    } else {
        None
    }
}
