//! Born-digital document extraction: deterministic, label-anchored
//! field extraction from the PDF text layer (plus an AcroForm pass for
//! issuer PDFs that carry fillable fields).
//!
//! Design rules (settled in docs research; see docs/KNOWN_LIMITS.md):
//! - No OCR, no LLM, no guessing. A field is emitted only when its
//!   printed label anchor is found AND a value token is located by the
//!   geometric rules below — or when the labeled box is found and is
//!   verifiably empty, for fields whose payload type defaults to zero.
//! - Money is integer cents (decimal strings are parsed exactly, never
//!   through floats; floats appear only in geometry).
//! - The taxpayer's SSN is never extracted: no rule looks at the
//!   employee/recipient SSN boxes, and PAYER'S TIN values are emitted
//!   only in EIN shape (`NN-NNNNNNN`), never in SSN shape.
//!
//! Text geometry comes from parsing page content streams directly
//! (lopdf supplies decompressed content; its positioned extraction is
//! not public, so a small tokenizer + text-state machine lives here).

use crate::{classify_text, validate_w2, DocKind, Finding};
use lopdf::{Dictionary, Document, Object};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;
use tax_core::{Money, TaxParams};
use tax_forms::W2Input;

// ---------------------------------------------------------------------------
// Words with geometry
// ---------------------------------------------------------------------------

/// One whitespace-delimited word from the text layer, in PDF user-space
/// coordinates (origin bottom-left, y up). `baseline` is the text
/// baseline the word sits on; `y0`/`y1` are an approximation
/// (descent −0.28 em, ascent +0.82 em) used only for display/debug.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    /// 1-based page number.
    pub page: u32,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub baseline: f32,
    pub font_size: f32,
}

// ---------------------------------------------------------------------------
// 2x3 transformation matrices, PDF convention:
// x' = a*x + c*y + e ; y' = b*x + d*y + f
// ---------------------------------------------------------------------------

type Mat = [f32; 6];

const IDENTITY: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Composition: the result applies `first`, then `second`.
fn then(first: &Mat, second: &Mat) -> Mat {
    let [a1, b1, c1, d1, e1, f1] = *first;
    let [a2, b2, c2, d2, e2, f2] = *second;
    [
        a2 * a1 + c2 * b1,
        b2 * a1 + d2 * b1,
        a2 * c1 + c2 * d1,
        b2 * c1 + d2 * d1,
        a2 * e1 + c2 * f1 + e2,
        b2 * e1 + d2 * f1 + f2,
    ]
}

fn apply(m: &Mat, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn translate(tx: f32, ty: f32) -> Mat {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

// ---------------------------------------------------------------------------
// Content-stream tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f32),
    Name(String),
    Str(Vec<u8>),
    Arr(Vec<Tok>),
    /// Inline-image data section (BI..ID..EI): contents are skipped.
    InlineImage,
    Op(String),
}

struct Tokenizer<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Tokenizer { bytes, pos: 0 }
    }

    fn skip_ws_and_comments(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b == b'%' {
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else if b.is_ascii_whitespace() || b == 0 {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn next_tok(&mut self) -> Option<Tok> {
        self.skip_ws_and_comments();
        if self.pos >= self.bytes.len() {
            return None;
        }
        let b = self.bytes[self.pos];
        match b {
            b'(' => self.literal_string().map(Tok::Str),
            b'<' => {
                if self.bytes.get(self.pos + 1) == Some(&b'<') {
                    // Dictionary: skip to the matching >> (nesting-aware).
                    self.skip_dict();
                    self.next_tok()
                } else {
                    self.hex_string().map(Tok::Str)
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws_and_comments();
                    if self.pos >= self.bytes.len() {
                        break;
                    }
                    if self.bytes[self.pos] == b']' {
                        self.pos += 1;
                        break;
                    }
                    match self.next_tok() {
                        Some(t) => items.push(t),
                        None => break,
                    }
                }
                Some(Tok::Arr(items))
            }
            b'/' => {
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.bytes.len()
                    && !self.bytes[self.pos].is_ascii_whitespace()
                    && !matches!(
                        self.bytes[self.pos],
                        b'[' | b']' | b'<' | b'>' | b'(' | b')' | b'/' | b'%'
                    )
                {
                    self.pos += 1;
                }
                Some(Tok::Name(
                    String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned(),
                ))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let start = self.pos;
                while self.pos < self.bytes.len()
                    && matches!(self.bytes[self.pos], b'+' | b'-' | b'.' | b'0'..=b'9')
                {
                    self.pos += 1;
                }
                let s = String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned();
                s.parse::<f32>().ok().map(Tok::Num)
            }
            _ => {
                // An operator: letters plus the specials ' " and *.
                let start = self.pos;
                while self.pos < self.bytes.len()
                    && (self.bytes[self.pos].is_ascii_alphabetic()
                        || matches!(self.bytes[self.pos], b'\'' | b'"' | b'*'))
                {
                    self.pos += 1;
                }
                if self.pos == start {
                    // Unknown byte: skip it to guarantee progress.
                    self.pos += 1;
                    return self.next_tok();
                }
                let op = String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned();
                if op == "BI" {
                    self.skip_inline_image();
                    Some(Tok::InlineImage)
                } else {
                    Some(Tok::Op(op))
                }
            }
        }
    }

    fn literal_string(&mut self) -> Option<Vec<u8>> {
        // self.bytes[pos] == '('
        self.pos += 1;
        let mut out = Vec::new();
        let mut depth = 1usize;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            match b {
                b'\\' => {
                    if self.pos >= self.bytes.len() {
                        break;
                    }
                    let e = self.bytes[self.pos];
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'(' | b')' | b'\\' => out.push(e),
                        b'\n' => {}
                        b'\r' => {
                            if self.bytes.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'0'..=b'7' => {
                            let mut val = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.bytes.get(self.pos) {
                                    Some(d @ b'0'..=b'7') => {
                                        val = val * 8 + (*d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(val as u8);
                        }
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(out);
                    }
                    out.push(b);
                }
                _ => out.push(b),
            }
        }
        Some(out)
    }

    fn hex_string(&mut self) -> Option<Vec<u8>> {
        // self.bytes[pos] == '<', not '<<'
        self.pos += 1;
        let mut nibbles: Vec<u8> = Vec::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if let Some(n) = (b as char).to_digit(16) {
                nibbles.push(n as u8);
            }
        }
        if nibbles.len() % 2 == 1 {
            nibbles.push(0);
        }
        Some(
            nibbles
                .chunks(2)
                .map(|c| (c[0] << 4) | c[1])
                .collect::<Vec<u8>>(),
        )
    }

    fn skip_dict(&mut self) {
        let mut depth = 0usize;
        while self.pos + 1 < self.bytes.len() {
            if self.bytes[self.pos] == b'<' && self.bytes[self.pos + 1] == b'<' {
                depth += 1;
                self.pos += 2;
            } else if self.bytes[self.pos] == b'>' && self.bytes[self.pos + 1] == b'>' {
                depth -= 1;
                self.pos += 2;
                if depth == 0 {
                    return;
                }
            } else {
                self.pos += 1;
            }
        }
        self.pos = self.bytes.len();
    }

    /// After a BI operator: skip tokens until ID, then raw image bytes
    /// until a whitespace-delimited EI. Best-effort — inline images do
    /// not occur in generated tax forms; text state is unaffected.
    fn skip_inline_image(&mut self) {
        // Consume key/value tokens up to the ID operator.
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 10_000 {
                return;
            }
            match self.next_tok() {
                Some(Tok::Op(op)) if op == "ID" => break,
                Some(_) => continue,
                None => return,
            }
        }
        // ID is followed by a single whitespace byte, then raw data.
        if self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        while self.pos + 2 < self.bytes.len() {
            if self.bytes[self.pos].is_ascii_whitespace()
                && self.bytes[self.pos + 1] == b'E'
                && self.bytes[self.pos + 2] == b'I'
                && self
                    .bytes
                    .get(self.pos + 3)
                    .map(|b| b.is_ascii_whitespace())
                    .unwrap_or(true)
            {
                self.pos += 3;
                return;
            }
            self.pos += 1;
        }
        self.pos = self.bytes.len();
    }
}

// ---------------------------------------------------------------------------
// Font decoding
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct FontInfo {
    /// Bytes per character code (1 for simple fonts, usually 2 for Type0).
    code_len: usize,
    /// code → Unicode string, from /ToUnicode.
    tounicode: HashMap<u32, String>,
    /// code → Unicode string, from /Encoding /Differences glyph names.
    differences: HashMap<u32, String>,
    /// code → advance width in 1/1000 em, from /Widths or Type0 /W.
    widths: HashMap<u32, u16>,
    /// Fallback widths by decoded ASCII char (base-14 AFM tables).
    afm: Option<&'static [u16; 95]>,
    courier: bool,
    default_width: u16,
}

impl FontInfo {
    fn decode_code(&self, code: u32) -> Option<String> {
        if let Some(s) = self.tounicode.get(&code) {
            return Some(s.clone());
        }
        if let Some(s) = self.differences.get(&code) {
            return Some(s.clone());
        }
        if self.code_len == 1 && code <= 0xFF {
            return Some(winansi_char(code as u8).to_string());
        }
        None
    }

    fn width_units(&self, code: u32, decoded: Option<&str>) -> u16 {
        if let Some(w) = self.widths.get(&code) {
            return *w;
        }
        if let Some(s) = decoded {
            let mut chars = s.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                if let Some(table) = self.afm {
                    if let Some(w) = crate::afm::char_width(table, c) {
                        return w;
                    }
                }
            }
        }
        if self.courier {
            return crate::afm::COURIER_WIDTH;
        }
        self.default_width
    }
}

/// WinAnsiEncoding for one byte. The 0x80..=0x9F block is where WinAnsi
/// differs from Latin-1; bytes WinAnsi leaves undefined map to the
/// Latin-1 control char (they do not occur in form text in practice).
fn winansi_char(b: u8) -> char {
    match b {
        0x80 => '\u{20AC}',
        0x81 => '\u{0081}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8D => '\u{008D}',
        0x8E => '\u{017D}',
        0x8F => '\u{008F}',
        0x90 => '\u{0090}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9D => '\u{009D}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        _ => b as char,
    }
}

/// Map an Adobe glyph name to its Unicode string: the names that occur
/// in /Differences arrays of real form PDFs (letters, digits, common
/// punctuation, ligatures), plus the `uniXXXX` convention.
fn glyph_name_to_unicode(name: &str) -> Option<String> {
    let base = name.split('.').next().unwrap_or(name);
    if let Some(hex) = base.strip_prefix("uni") {
        if hex.len() == 4 {
            if let Ok(cp) = u32::from_str_radix(hex, 16) {
                return char::from_u32(cp).map(|c| c.to_string());
            }
        }
    }
    let named: &str = match base {
        "space" => " ",
        "exclam" => "!",
        "quotedbl" => "\"",
        "numbersign" => "#",
        "dollar" => "$",
        "percent" => "%",
        "ampersand" => "&",
        "quotesingle" | "quoteright" | "quote" => "'",
        "parenleft" => "(",
        "parenright" => ")",
        "asterisk" => "*",
        "plus" => "+",
        "comma" => ",",
        "hyphen" | "minus" | "sfthyphen" => "-",
        "period" => ".",
        "slash" => "/",
        "zero" => "0",
        "one" => "1",
        "two" => "2",
        "three" => "3",
        "four" => "4",
        "five" => "5",
        "six" => "6",
        "seven" => "7",
        "eight" => "8",
        "nine" => "9",
        "colon" => ":",
        "semicolon" => ";",
        "less" => "<",
        "equal" => "=",
        "greater" => ">",
        "question" => "?",
        "at" => "@",
        "bracketleft" => "[",
        "backslash" => "\\",
        "bracketright" => "]",
        "asciicircum" => "^",
        "underscore" => "_",
        "grave" | "quoteleft" => "`",
        "braceleft" => "{",
        "bar" => "|",
        "braceright" => "}",
        "asciitilde" => "~",
        "bullet" => "\u{2022}",
        "endash" => "\u{2013}",
        "emdash" => "\u{2014}",
        "quotedblleft" => "\u{201C}",
        "quotedblright" => "\u{201D}",
        "Euro" => "\u{20AC}",
        "fi" => "fi",
        "fl" => "fl",
        "nbspace" => "\u{A0}",
        _ => {
            // Single letters name themselves.
            let mut chars = base.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => return Some(c.to_string()),
                _ => return None,
            }
        }
    };
    Some(named.to_string())
}

fn hex_bytes_to_u32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |acc, b| (acc << 8) | (*b as u32))
}

/// Parse a /ToUnicode CMap stream: beginbfchar / beginbfrange sections.
/// Returns code → Unicode string. Hand-rolled: lopdf's CMap parser is
/// crate-private.
fn parse_tounicode(content: &[u8]) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    let text = String::from_utf8_lossy(content);
    // Tokenize into hex strings and bare words.
    #[derive(Debug, PartialEq)]
    enum CT {
        Hex(Vec<u8>),
        Word(String),
        LBracket,
        RBracket,
    }
    let bytes = text.as_bytes();
    let mut toks: Vec<CT> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'<' && bytes.get(i + 1) != Some(&b'<') {
            let mut j = i + 1;
            let mut nibbles: Vec<u8> = Vec::new();
            while j < bytes.len() && bytes[j] != b'>' {
                if let Some(n) = (bytes[j] as char).to_digit(16) {
                    nibbles.push(n as u8);
                }
                j += 1;
            }
            if nibbles.len() % 2 == 1 {
                nibbles.push(0);
            }
            toks.push(CT::Hex(
                nibbles.chunks(2).map(|c| (c[0] << 4) | c[1]).collect(),
            ));
            i = j + 1;
        } else if b == b'[' {
            toks.push(CT::LBracket);
            i += 1;
        } else if b == b']' {
            toks.push(CT::RBracket);
            i += 1;
        } else if b.is_ascii_alphanumeric() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                i += 1;
            }
            toks.push(CT::Word(text[start..i].to_string()));
        } else {
            i += 1;
        }
    }
    let utf16 = |raw: &[u8]| -> String {
        if raw.len() >= 2 && raw.len().is_multiple_of(2) {
            let units: Vec<u16> = raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect();
            String::from_utf16_lossy(&units)
        } else {
            raw.iter().map(|&b| b as char).collect()
        }
    };
    let mut k = 0;
    while k < toks.len() {
        match &toks[k] {
            CT::Word(w) if w == "beginbfchar" => {
                k += 1;
                while k + 1 < toks.len() && toks[k] != CT::Word("endbfchar".to_string()) {
                    if let (CT::Hex(src), CT::Hex(dst)) = (&toks[k], &toks[k + 1]) {
                        map.insert(hex_bytes_to_u32(src), utf16(dst));
                        k += 2;
                    } else {
                        break;
                    }
                }
            }
            CT::Word(w) if w == "beginbfrange" => {
                k += 1;
                while k + 2 < toks.len() && toks[k] != CT::Word("endbfrange".to_string()) {
                    let (CT::Hex(lo), CT::Hex(hi)) = (&toks[k], &toks[k + 1]) else {
                        break;
                    };
                    let (lo_v, hi_v) = (hex_bytes_to_u32(lo), hex_bytes_to_u32(hi));
                    match &toks[k + 2] {
                        CT::Hex(dst0) => {
                            // Sequential destinations of the same byte length.
                            let base = hex_bytes_to_u32(dst0);
                            let nbytes = dst0.len();
                            for (off, code) in (lo_v..=hi_v).enumerate() {
                                let v = base + off as u32;
                                let raw: Vec<u8> = (0..nbytes)
                                    .map(|bi| (v >> (8 * (nbytes - 1 - bi))) as u8)
                                    .collect();
                                map.insert(code, utf16(&raw));
                            }
                            k += 3;
                        }
                        CT::LBracket => {
                            let mut code = lo_v;
                            let mut j = k + 3;
                            while j < toks.len() && toks[j] != CT::RBracket {
                                if let CT::Hex(dst) = &toks[j] {
                                    map.insert(code, utf16(dst));
                                    code += 1;
                                }
                                j += 1;
                            }
                            k = j + 1;
                        }
                        _ => break,
                    }
                }
            }
            _ => k += 1,
        }
    }
    map
}

fn dict_get<'a>(dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    dict.get(key).ok()
}

fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Object> {
    match obj {
        Object::Reference(id) => doc.get_object(*id).ok(),
        other => Some(other),
    }
}

fn as_dict<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    resolve(doc, obj).and_then(|o| o.as_dict().ok())
}

fn name_of(obj: &Object) -> Option<String> {
    obj.as_name()
        .ok()
        .map(|n| String::from_utf8_lossy(n).into_owned())
}

fn num_of(obj: &Object) -> Option<f32> {
    match obj {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

/// Build the FontInfo for one font dictionary (simple or Type0).
fn build_font(doc: &Document, font_dict: &Dictionary) -> FontInfo {
    let subtype = dict_get(font_dict, b"Subtype")
        .and_then(name_of)
        .unwrap_or_default();
    let base_font = dict_get(font_dict, b"BaseFont")
        .and_then(name_of)
        .unwrap_or_default();
    let (afm, courier) = crate::afm::table_for_base_font(&base_font);
    let mut info = FontInfo {
        code_len: if subtype == "Type0" { 2 } else { 1 },
        tounicode: HashMap::new(),
        differences: HashMap::new(),
        widths: HashMap::new(),
        afm,
        courier,
        default_width: 1000,
    };

    // /ToUnicode
    if let Some(tu) = dict_get(font_dict, b"ToUnicode") {
        if let Some(Object::Stream(stream)) = resolve(doc, tu) {
            if let Ok(content) = stream.decompressed_content() {
                info.tounicode = parse_tounicode(&content);
                // Code length: the byte length of the mapped source codes.
                if let Some((code, _)) = info.tounicode.iter().next() {
                    let mut v = *code;
                    let mut len = 0usize;
                    while v > 0 {
                        len += 1;
                        v >>= 8;
                    }
                    if subtype != "Type0" && len > 0 {
                        info.code_len = len;
                    }
                }
            }
        }
    }

    if subtype == "Type0" {
        // Descendant CIDFont: /W widths + /DW default.
        if let Some(Object::Array(descs)) = dict_get(font_dict, b"DescendantFonts") {
            for item in descs {
                let Some(dd) = as_dict(doc, item) else {
                    continue;
                };
                if let Some(dw) = dict_get(dd, b"DW").and_then(num_of) {
                    info.default_width = dw as u16;
                }
                if let Some(Object::Array(warr)) = dict_get(dd, b"W") {
                    parse_cid_widths(warr, &mut info.widths);
                }
                // The descendant may carry the ToUnicode-less encoding;
                // widths keyed by CID are what we need.
            }
        }
        return info;
    }

    // Simple font: /FirstChar + /Widths.
    let first_char = dict_get(font_dict, b"FirstChar")
        .and_then(num_of)
        .unwrap_or(0.0) as u32;
    if let Some(Object::Array(widths)) = dict_get(font_dict, b"Widths") {
        for (i, w) in widths.iter().enumerate() {
            if let Some(v) = num_of(w) {
                info.widths.insert(first_char + i as u32, v as u16);
            }
        }
    }
    if let Some(fd) = dict_get(font_dict, b"FontDescriptor") {
        if let Some(fd) = as_dict(doc, fd) {
            if let Some(mw) = dict_get(fd, b"MissingWidth").and_then(num_of) {
                info.default_width = mw as u16;
            }
        }
    }
    // /Encoding /Differences.
    if let Some(enc) = dict_get(font_dict, b"Encoding") {
        if let Some(enc_dict) = as_dict(doc, enc) {
            if let Some(Object::Array(diffs)) = dict_get(enc_dict, b"Differences") {
                let mut code: Option<u32> = None;
                for item in diffs {
                    match item {
                        Object::Integer(n) => code = Some(*n as u32),
                        Object::Name(nm) => {
                            if let Some(c) = code {
                                let name = String::from_utf8_lossy(nm).into_owned();
                                if let Some(u) = glyph_name_to_unicode(&name) {
                                    info.differences.insert(c, u);
                                }
                                code = Some(c + 1);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    info
}

/// Parse a CIDFont /W array: `c [w ...]` and `cfirst clast w` groups.
fn parse_cid_widths(warr: &[Object], out: &mut HashMap<u32, u16>) {
    let mut i = 0;
    while i < warr.len() {
        let Some(c) = num_of(&warr[i]) else {
            i += 1;
            continue;
        };
        match warr.get(i + 1) {
            Some(Object::Array(ws)) => {
                for (j, w) in ws.iter().enumerate() {
                    if let Some(v) = num_of(w) {
                        out.insert(c as u32 + j as u32, v as u16);
                    }
                }
                i += 2;
            }
            Some(second) => {
                if let (Some(last), Some(w)) = (num_of(second), warr.get(i + 2).and_then(num_of)) {
                    for code in (c as u32)..=(last as u32) {
                        out.insert(code, w as u16);
                    }
                    i += 3;
                } else {
                    i += 1;
                }
            }
            None => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Text-state machine: content stream -> positioned words
// ---------------------------------------------------------------------------

struct Atom {
    text: String,
    x0: f32,
    x1: f32,
    baseline: f32,
    font_size: f32,
    page: u32,
}

struct TextMachine<'a> {
    fonts: &'a HashMap<String, FontInfo>,
    ctm: Mat,
    ctm_stack: Vec<Mat>,
    tm: Mat,
    tlm: Mat,
    font_name: Option<String>,
    font_size: f32,
    tc: f32,
    tw: f32,
    th: f32,
    tl: f32,
    page: u32,
    atoms: Vec<Atom>,
    in_text: bool,
}

impl<'a> TextMachine<'a> {
    fn new(fonts: &'a HashMap<String, FontInfo>, page: u32) -> Self {
        TextMachine {
            fonts,
            ctm: IDENTITY,
            ctm_stack: Vec::new(),
            tm: IDENTITY,
            tlm: IDENTITY,
            font_name: None,
            font_size: 0.0,
            tc: 0.0,
            tw: 0.0,
            th: 100.0,
            tl: 0.0,
            page,
            atoms: Vec::new(),
            in_text: false,
        }
    }

    fn font(&self) -> Option<&FontInfo> {
        self.font_name.as_ref().and_then(|n| self.fonts.get(n))
    }

    /// Show one decoded glyph atom and advance the text matrix.
    fn show_glyph(&mut self, text: String, width_units: u16, tj_adj: f32) {
        let size = self.font_size;
        // Advance in text space (PDF 32000-1 §9.4.3):
        // tx = ((w0 - Tj/1000) * fontsize + Tc (+ Tw for a space)) * Th/100
        let mut step = (width_units as f32 / 1000.0 - tj_adj / 1000.0) * size + self.tc;
        if text == " " {
            step += self.tw;
        }
        step *= self.th / 100.0;
        let render = then(&self.tm, &self.ctm);
        let (x0, baseline) = apply(&render, 0.0, 0.0);
        let (x1, _) = apply(&render, step, 0.0);
        self.atoms.push(Atom {
            text,
            x0,
            x1,
            baseline,
            font_size: size,
            page: self.page,
        });
        self.tm = then(&translate(step, 0.0), &self.tm);
    }

    /// Decode a string operand and show its glyphs.
    fn show_bytes(&mut self, bytes: &[u8], tj_adj: f32) {
        let Some(font) = self.font().cloned() else {
            return;
        };
        let len = font.code_len.max(1);
        let mut i = 0;
        while i < bytes.len() {
            let take = len.min(bytes.len() - i);
            let chunk = &bytes[i..i + take];
            let code = hex_bytes_to_u32(chunk);
            let decoded = font
                .decode_code(code)
                .unwrap_or_else(|| '\u{FFFD}'.to_string());
            let width = font.width_units(code, Some(&decoded));
            self.show_glyph(decoded, width, if i == 0 { tj_adj } else { 0.0 });
            i += take;
        }
    }

    fn newline(&mut self) {
        let m = translate(0.0, -self.tl);
        self.tlm = then(&m, &self.tlm);
        self.tm = self.tlm;
    }

    fn op(&mut self, operator: &str, operands: &[Tok]) {
        let num = |idx: usize| -> f32 {
            operands
                .get(idx)
                .and_then(|t| match t {
                    Tok::Num(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0.0)
        };
        match operator {
            "q" => self.ctm_stack.push(self.ctm),
            "Q" => {
                if let Some(m) = self.ctm_stack.pop() {
                    self.ctm = m;
                }
            }
            "cm" => {
                let m: Mat = [num(0), num(1), num(2), num(3), num(4), num(5)];
                self.ctm = then(&m, &self.ctm);
            }
            "BT" => {
                self.tm = IDENTITY;
                self.tlm = IDENTITY;
                self.in_text = true;
            }
            "ET" => self.in_text = false,
            "Tf" => {
                if let Some(Tok::Name(n)) = operands.first() {
                    self.font_name = Some(n.clone());
                }
                self.font_size = num(1);
            }
            "Td" => {
                let m = translate(num(0), num(1));
                self.tlm = then(&m, &self.tlm);
                self.tm = self.tlm;
            }
            "TD" => {
                self.tl = -num(1);
                let m = translate(num(0), num(1));
                self.tlm = then(&m, &self.tlm);
                self.tm = self.tlm;
            }
            "Tm" => {
                let m: Mat = [num(0), num(1), num(2), num(3), num(4), num(5)];
                self.tm = m;
                self.tlm = m;
            }
            "T*" => self.newline(),
            "TL" => self.tl = num(0),
            "Tc" => self.tc = num(0),
            "Tw" => self.tw = num(0),
            "Tz" => self.th = num(0),
            "Ts" => {}
            "Tj" => {
                if let Some(Tok::Str(s)) = operands.first() {
                    self.show_bytes(&s.clone(), 0.0);
                }
            }
            "TJ" => {
                if let Some(Tok::Arr(items)) = operands.first() {
                    for item in items.clone() {
                        match item {
                            Tok::Str(s) => self.show_bytes(&s, 0.0),
                            // TJ adjustments: positive numbers move the
                            // next glyph back (subtracted from advance);
                            // a large displacement becomes a visible gap
                            // via the atom positions themselves.
                            Tok::Num(n) if n != 0.0 => {
                                // Fold the adjustment into the next
                                // glyph by shifting the text matrix.
                                let shift = (-n / 1000.0) * self.font_size * (self.th / 100.0);
                                self.tm = then(&translate(shift, 0.0), &self.tm);
                            }
                            _ => {}
                        }
                    }
                }
            }
            "'" => {
                self.newline();
                if let Some(Tok::Str(s)) = operands.first() {
                    self.show_bytes(&s.clone(), 0.0);
                }
            }
            "\"" => {
                self.tw = num(0);
                self.tc = num(1);
                self.newline();
                if let Some(Tok::Str(s)) = operands.get(2) {
                    self.show_bytes(&s.clone(), 0.0);
                }
            }
            _ => {}
        }
    }
}

/// Split atoms into words: break on whitespace atoms and on horizontal
/// gaps larger than 0.16 em (catches TJ-displacement spacing).
fn atoms_to_words(atoms: &[Atom]) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut cur: Option<Word> = None;
    let mut cur_baseline = 0.0f32;
    let flush = |cur: &mut Option<Word>, words: &mut Vec<Word>| {
        if let Some(w) = cur.take() {
            if !w.text.trim().is_empty() {
                words.push(w);
            }
        }
    };
    for atom in atoms {
        let is_space = atom.text.trim().is_empty() && !atom.text.is_empty();
        if is_space {
            flush(&mut cur, &mut words);
            continue;
        }
        let break_here = match &cur {
            None => true,
            Some(w) => {
                atom.page != w.page
                    || (atom.baseline - cur_baseline).abs() > 1.0
                    || atom.x0 - w.x1 > 0.16 * atom.font_size.max(1.0)
                    || atom.x0 < w.x0 - 1.0 // position jumped backwards
            }
        };
        if break_here {
            flush(&mut cur, &mut words);
            cur = Some(Word {
                text: atom.text.clone(),
                page: atom.page,
                x0: atom.x0,
                y0: atom.baseline - 0.28 * atom.font_size,
                x1: atom.x1,
                y1: atom.baseline + 0.82 * atom.font_size,
                baseline: atom.baseline,
                font_size: atom.font_size,
            });
            cur_baseline = atom.baseline;
        } else if let Some(w) = &mut cur {
            w.text.push_str(&atom.text);
            w.x1 = atom.x1;
        }
    }
    flush(&mut cur, &mut words);
    words
}

/// Extract positioned words from every page of the PDF at `path`.
pub fn extract_words(path: &Path) -> Result<Vec<Word>, String> {
    let doc = Document::load(path).map_err(|e| format!("cannot load PDF: {e}"))?;
    let mut all_atoms: Vec<Atom> = Vec::new();
    for (page_num, page_id) in doc.get_pages() {
        // Fonts for this page.
        let mut fonts: HashMap<String, FontInfo> = HashMap::new();
        if let Ok(page_fonts) = doc.get_page_fonts(page_id) {
            for (name, dict) in page_fonts {
                let key = String::from_utf8_lossy(&name).into_owned();
                fonts.insert(key, build_font(&doc, dict));
            }
        }
        let content = doc.get_page_content(page_id);
        let mut machine = TextMachine::new(&fonts, page_num);
        let mut tokenizer = Tokenizer::new(&content);
        let mut operands: Vec<Tok> = Vec::new();
        while let Some(tok) = tokenizer.next_tok() {
            match tok {
                Tok::Op(op) => {
                    machine.op(&op, &operands);
                    operands.clear();
                }
                Tok::InlineImage => operands.clear(),
                other => operands.push(other),
            }
        }
        all_atoms.extend(machine.atoms);
    }
    Ok(atoms_to_words(&all_atoms))
}

// ---------------------------------------------------------------------------
// Lines
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Line {
    page: u32,
    baseline: f32,
    /// Word indices into the words slice, sorted by x0.
    words: Vec<usize>,
}

fn group_lines(words: &[Word]) -> Vec<Line> {
    let mut order: Vec<usize> = (0..words.len()).collect();
    order.sort_by(|&a, &b| {
        words[a]
            .page
            .cmp(&words[b].page)
            .then(
                words[b]
                    .baseline
                    .partial_cmp(&words[a].baseline)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                words[a]
                    .x0
                    .partial_cmp(&words[b].x0)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    let mut lines: Vec<Line> = Vec::new();
    for idx in order {
        let w = &words[idx];
        let tol = (0.35 * w.font_size).clamp(1.5, 4.0);
        match lines.last_mut() {
            Some(line) if line.page == w.page && (line.baseline - w.baseline).abs() <= tol => {
                line.words.push(idx);
            }
            _ => lines.push(Line {
                page: w.page,
                baseline: w.baseline,
                words: vec![idx],
            }),
        }
    }
    for line in &mut lines {
        line.words.sort_by(|&a, &b| {
            words[a]
                .x0
                .partial_cmp(&words[b].x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    lines
}

/// Normalized comparison form of a word: lowercase alphanumerics only.
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn line_norm_words(words: &[Word], line: &Line) -> Vec<String> {
    line.words.iter().map(|&i| norm(&words[i].text)).collect()
}

/// A located label phrase.
#[derive(Debug, Clone)]
struct LabelHit {
    page: u32,
    /// Index of the first line of the span in the page's line list.
    first_line: usize,
    /// Index of the last line of the span (== first_line for 1-line).
    last_line: usize,
    x0: f32,
    x1: f32,
    /// Baseline of the last line of the span.
    baseline: f32,
    /// The matched words as printed, joined with single spaces.
    label_text: String,
}

/// All hits for `phrase` (normalized words) in the page's line list,
/// allowing the phrase to wrap across two consecutive lines. `lines`
/// must contain only lines of one page, in reading (top-first) order.
fn find_label_hits(words: &[Word], lines: &[Line], phrase: &[&str]) -> Vec<LabelHit> {
    let want: Vec<String> = phrase.iter().map(|p| norm(p)).collect();
    let mut hits = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let lw = line_norm_words(words, line);
        // Single-line match.
        if lw.len() >= want.len() {
            for start in 0..=(lw.len() - want.len()) {
                if lw[start..start + want.len()] == want[..] {
                    let first = &words[line.words[start]];
                    let last = &words[line.words[start + want.len() - 1]];
                    hits.push(LabelHit {
                        page: line.page,
                        first_line: li,
                        last_line: li,
                        x0: first.x0,
                        x1: last.x1,
                        baseline: line.baseline,
                        label_text: line.words[start..start + want.len()]
                            .iter()
                            .map(|&wi| words[wi].text.as_str())
                            .collect::<Vec<_>>()
                            .join(" "),
                    });
                }
            }
        }
        // Two-line wrap: phrase starts on this line, ends on the next.
        if li + 1 < lines.len() && lines[li + 1].page == line.page {
            let next = &lines[li + 1];
            if line.baseline - next.baseline <= 16.0 {
                let nw = line_norm_words(words, next);
                let combined: Vec<String> = lw.iter().chain(nw.iter()).cloned().collect();
                if combined.len() > lw.len() && combined.len() >= want.len() {
                    for start in 0..=(combined.len() - want.len()) {
                        if start < lw.len()
                            && start + want.len() > lw.len()
                            && combined[start..start + want.len()] == want[..]
                        {
                            let first = &words[line.words[start]];
                            let last_idx = start + want.len() - 1 - lw.len();
                            let last = &words[next.words[last_idx]];
                            let mut span: Vec<&str> = line.words[start..]
                                .iter()
                                .map(|&wi| words[wi].text.as_str())
                                .collect();
                            span.extend(
                                next.words[..=last_idx]
                                    .iter()
                                    .map(|&wi| words[wi].text.as_str()),
                            );
                            hits.push(LabelHit {
                                page: line.page,
                                first_line: li,
                                last_line: li + 1,
                                x0: first.x0,
                                x1: last.x1,
                                baseline: next.baseline,
                                label_text: span.join(" "),
                            });
                        }
                    }
                }
            }
        }
    }
    hits
}

// ---------------------------------------------------------------------------
// Money tokens
// ---------------------------------------------------------------------------

/// Parse a printed money token to integer cents, exactly (string math).
/// Accepts: optional `$`, thousands commas, an optional decimal part of
/// 1–2 digits, a leading `-`, and parenthesized negatives. Returns None
/// for anything else. `strict` requires the token to LOOK like money
/// (decimal point, comma group, or ≥5 digits) so box numbers, years and
/// counts never qualify; non-strict accepts any plain integer (used for
/// AcroForm values, where the field itself is unambiguous).
pub fn parse_money_token(text: &str, strict: bool) -> Option<i64> {
    let mut t = text.trim();
    let mut negative = false;
    if t.len() >= 2 && t.starts_with('(') && t.ends_with(')') {
        negative = true;
        t = &t[1..t.len() - 1];
    }
    if let Some(rest) = t.strip_prefix('-') {
        negative = true;
        t = rest;
    }
    if let Some(rest) = t.strip_prefix('$') {
        t = rest;
    }
    let has_comma = t.contains(',');
    let cleaned: String = t.chars().filter(|c| *c != ',').collect();
    let (dollars, cents_part) = match cleaned.split_once('.') {
        Some((d, c)) => {
            if c.is_empty() || c.len() > 2 || !c.chars().all(|ch| ch.is_ascii_digit()) {
                return None;
            }
            (d, c)
        }
        None => (cleaned.as_str(), ""),
    };
    if dollars.is_empty() || !dollars.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if strict && !cleaned.contains('.') && !has_comma && dollars.len() < 5 {
        return None;
    }
    let dollars_v: i64 = dollars.parse().ok()?;
    let cents_v: i64 = match cents_part.len() {
        0 => 0,
        1 => cents_part.parse::<i64>().ok()? * 10,
        _ => cents_part.parse().ok()?,
    };
    let total = dollars_v.checked_mul(100)?.checked_add(cents_v)?;
    Some(if negative { -total } else { total })
}

/// Is this token in EIN shape `NN-NNNNNNN`?
fn is_ein_token(text: &str) -> bool {
    let t = text.trim();
    let b = t.as_bytes();
    b.len() == 10
        && b[2] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 2 || c.is_ascii_digit())
}

/// Is this token in SSN shape `NNN-NN-NNNN`? (Used only to REFUSE to
/// emit it — never as a value source.)
fn is_ssn_token(text: &str) -> bool {
    let t = text.trim();
    let b = t.as_bytes();
    b.len() == 11
        && b[3] == b'-'
        && b[6] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 3 || i == 6 || c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Region search: given a label hit, find the value token near it
// ---------------------------------------------------------------------------

/// Words around a label hit, in search-priority order, each tagged
/// (region, confidence). Order: same line near right → directly below
/// (≤30 pt) → further below (≤45 pt — capped so the NEXT box's value,
/// typically ≥46 pt down, is never stolen) → same line far right.
fn region_words<'w>(
    words: &'w [Word],
    lines: &[Line],
    hit: &LabelHit,
) -> Vec<(&'w Word, &'static str, &'static str)> {
    let mut a: Vec<&Word> = Vec::new();
    let mut a2: Vec<&Word> = Vec::new();
    let mut b: Vec<(&Word, f32)> = Vec::new();
    let mut c: Vec<&Word> = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        if line.page != hit.page {
            continue;
        }
        if li == hit.last_line {
            for &wi in &line.words {
                let w = &words[wi];
                if w.x0 > hit.x1 + 1.0 {
                    if w.x0 - hit.x1 <= 200.0 {
                        a.push(w);
                    } else {
                        a2.push(w);
                    }
                }
            }
        } else if li > hit.last_line {
            let drop = hit.baseline - line.baseline;
            if drop <= 2.5 {
                continue;
            }
            for &wi in &line.words {
                let w = &words[wi];
                // The left edge allows 20 pt: values typically start
                // at the box's left edge, left of the label text that
                // follows the printed box number.
                if w.x0 < hit.x0 - 20.0 || w.x0 > hit.x0 + 240.0 {
                    continue;
                }
                if drop <= 30.0 {
                    b.push((w, drop));
                } else if drop <= 45.0 {
                    c.push(w);
                }
            }
        }
    }
    let mut out: Vec<(&Word, &'static str, &'static str)> = Vec::new();
    for w in a {
        out.push((w, "same_line", "high"));
    }
    for (w, drop) in b {
        out.push((w, "below", if drop <= 20.0 { "high" } else { "medium" }));
    }
    for w in c {
        out.push((w, "below_far", "medium"));
    }
    for w in a2 {
        out.push((w, "same_line_far", "medium"));
    }
    out
}

#[derive(Debug, Clone)]
struct FoundValue {
    cents: i64,
    raw: String,
    region: &'static str,
    confidence: &'static str,
}

/// Evaluate every hit for a money field: the winner is the first hit
/// (reading order) whose regions contain a money token.
fn find_money(
    words: &[Word],
    lines: &[Line],
    hits: &[LabelHit],
) -> (Option<(LabelHit, FoundValue)>, bool) {
    let mut any_label = false;
    for hit in hits {
        any_label = true;
        for (w, region, conf) in region_words(words, lines, hit) {
            if let Some(cents) = parse_money_token(&w.text, true) {
                return (
                    Some((
                        hit.clone(),
                        FoundValue {
                            cents,
                            raw: w.text.clone(),
                            region,
                            confidence: conf,
                        },
                    )),
                    any_label,
                );
            }
        }
    }
    (None, any_label)
}

// ---------------------------------------------------------------------------
// Field collection
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Collector {
    fields: Map<String, Value>,
    evidence: Map<String, Value>,
    warnings: Vec<String>,
    /// Payload keys the struct requires (no serde default) that were
    /// not extracted.
    missing_required: Vec<String>,
}

impl Collector {
    fn put_money(&mut self, key: &str, cents: i64, ev: Value) {
        self.fields
            .insert(key.to_string(), Value::String(cents.to_string()));
        self.evidence.insert(key.to_string(), ev);
    }

    fn put_string(&mut self, key: &str, value: &str, ev: Value) {
        self.fields
            .insert(key.to_string(), Value::String(value.to_string()));
        self.evidence.insert(key.to_string(), ev);
    }

    fn money_evidence(hit: &LabelHit, found: &FoundValue, box_label: &str) -> Value {
        json!({
            "page": hit.page,
            "label": hit.label_text,
            "raw": found.raw,
            "box": box_label,
            "region": found.region,
            "confidence": found.confidence,
            "source": "text",
        })
    }

    fn empty_evidence(hit: &LabelHit, box_label: &str) -> Value {
        json!({
            "page": hit.page,
            "label": hit.label_text,
            "box": box_label,
            "empty": true,
            "confidence": "high",
            "source": "text",
            "note": "labeled box located; no value printed in it",
        })
    }
}

struct MoneySpec {
    key: &'static str,
    phrases: &'static [&'static str],
    box_label: &'static str,
    required: bool,
    empty_means_zero: bool,
}

/// Does the word immediately before this hit's span look like the
/// box number ("1", "1a", "4")? Form titles also contain label phrases
/// ("Form 1099-NEC — Nonemployee Compensation"); the real box carries
/// its number, and hits with one are evaluated first.
fn hit_has_box_number(
    words: &[Word],
    pages: &[Vec<Line>],
    hit: &LabelHit,
    box_label: &str,
) -> bool {
    let Some(lines) = pages
        .iter()
        .find(|ls| ls.first().map(|l| l.page) == Some(hit.page))
    else {
        return false;
    };
    let line = &lines[hit.first_line];
    // The last word ending at or before the span start.
    let prev = line
        .words
        .iter()
        .rev()
        .map(|&wi| &words[wi])
        .find(|w| w.x1 <= hit.x0 + 0.5);
    match prev {
        Some(w) => {
            let n = norm(&w.text);
            n == norm(box_label) || (n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit()))
        }
        None => false,
    }
}

/// Extract one money field, evaluating label hits across all pages in
/// reading order.
fn extract_money_field(col: &mut Collector, words: &[Word], pages: &[Vec<Line>], spec: &MoneySpec) {
    // Gather hits across pages in reading order, then stably sort so
    // hits carrying their box number are evaluated first.
    let mut hits: Vec<LabelHit> = Vec::new();
    for lines in pages {
        hits.extend(find_label_hits(words, lines, spec.phrases));
    }
    let boxy: Vec<bool> = hits
        .iter()
        .map(|h| hit_has_box_number(words, pages, h, spec.box_label))
        .collect();
    let mut order: Vec<usize> = (0..hits.len()).collect();
    order.sort_by_key(|&i| !boxy[i]);
    let hits: Vec<LabelHit> = order.iter().map(|&i| hits[i].clone()).collect();
    let boxy: Vec<bool> = order.iter().map(|&i| boxy[i]).collect();
    if hits.is_empty() {
        if spec.required {
            col.missing_required.push(spec.key.to_string());
            col.warnings.push(format!(
                "could not locate the label for {} (box {}); field omitted",
                spec.key, spec.box_label
            ));
        }
        return;
    }
    // Evaluate all hits: first with a value wins; note disagreeing
    // copies (only box-numbered hits count as copies — a form title
    // containing the phrase is not a copy).
    let mut winner: Option<(LabelHit, FoundValue)> = None;
    let mut other_values: Vec<i64> = Vec::new();
    let mut other_boxy_copy = false;
    for (hi, hit) in hits.iter().enumerate() {
        // find_money expects the page's lines; hits carry their page.
        let page_lines: &[Line] = pages
            .iter()
            .find(|ls| ls.first().map(|l| l.page) == Some(hit.page))
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let single = std::slice::from_ref(hit);
        let (found, _) = find_money(words, page_lines, single);
        if let Some((h, f)) = found {
            if winner.is_none() {
                winner = Some((h, f));
            } else if boxy[hi] {
                other_boxy_copy = true;
                if let Some((_, wf)) = &winner {
                    if f.cents != wf.cents {
                        other_values.push(f.cents);
                    }
                }
            }
        }
    }
    match winner {
        Some((hit, found)) => {
            if other_boxy_copy {
                if other_values.is_empty() {
                    col.warnings.push(format!(
                        "multiple copies of the {} label found; extracted the first in reading order",
                        spec.key
                    ));
                } else {
                    col.warnings.push(format!(
                        "multiple copies of the {} label found with DIFFERING values; extracted the first in reading order — verify against the document",
                        spec.key
                    ));
                }
            }
            let ev = Collector::money_evidence(&hit, &found, spec.box_label);
            col.put_money(spec.key, found.cents, ev);
        }
        None => {
            // Label(s) found, no value anywhere: the box is empty.
            let hit = &hits[0];
            if spec.empty_means_zero {
                let ev = Collector::empty_evidence(hit, spec.box_label);
                col.put_money(spec.key, 0, ev);
            } else {
                if spec.required {
                    col.missing_required.push(spec.key.to_string());
                }
                col.warnings.push(format!(
                    "box {} ({}) located but empty; field {} omitted",
                    spec.box_label, hit.label_text, spec.key
                ));
            }
        }
    }
}

/// Extract a "name block" field (payer / employer name): the first
/// text line below the label block that is not a label continuation.
fn extract_name_block(
    col: &mut Collector,
    words: &[Word],
    pages: &[Vec<Line>],
    key: &str,
    phrases: &[&str],
    continuation_markers: &[&str],
) {
    for lines in pages {
        let hits = find_label_hits(words, lines, phrases);
        let Some(hit) = hits.into_iter().next() else {
            continue;
        };
        for line in lines.iter().skip(hit.last_line + 1) {
            if line.page != hit.page {
                break;
            }
            let drop = hit.baseline - line.baseline;
            if drop > 60.0 {
                break;
            }
            let first = &words[line.words[0]];
            if first.x0 < hit.x0 - 6.0 || first.x0 > hit.x0 + 320.0 {
                continue;
            }
            let joined_norm = line_norm_words(words, line).join("");
            if continuation_markers.iter().any(|m| joined_norm.contains(m)) {
                continue;
            }
            // Take only this block's column: words starting near the
            // label's x, extending while gaps stay small. (A form row
            // often carries a second block — e.g. the employee's name
            // beside the employer's — on the same visual line.)
            let mut text: Vec<&str> = Vec::new();
            let mut prev_x1: Option<f32> = None;
            for &wi in &line.words {
                let w = &words[wi];
                if w.x0 < hit.x0 - 6.0 {
                    continue;
                }
                if let Some(px) = prev_x1 {
                    if w.x0 - px > 30.0 {
                        break;
                    }
                }
                text.push(w.text.as_str());
                prev_x1 = Some(w.x1);
            }
            if text.is_empty() {
                continue;
            }
            let name = text.join(" ");
            // A name line is text, never a number or an ID.
            if parse_money_token(&name, true).is_some()
                || is_ein_token(&name)
                || is_ssn_token(&name)
            {
                continue;
            }
            let ev = json!({
                "page": hit.page,
                "label": hit.label_text,
                "raw": name,
                "region": "below",
                "confidence": "high",
                "source": "text",
            });
            col.put_string(key, &name, ev);
            return;
        }
    }
}

/// Extract an EIN-shaped token near a label (employer EIN, payer TIN).
/// SSN-shaped tokens in the region are refused, never emitted.
fn extract_ein_field(
    col: &mut Collector,
    words: &[Word],
    pages: &[Vec<Line>],
    key: &str,
    phrases: &[&str],
    box_label: &str,
    required: bool,
) {
    let mut ssn_refused = false;
    for lines in pages {
        let hits = find_label_hits(words, lines, phrases);
        for hit in &hits {
            for (w, region, conf) in region_words(words, lines, hit) {
                if is_ein_token(&w.text) {
                    let ev = json!({
                        "page": hit.page,
                        "label": hit.label_text,
                        "raw": w.text,
                        "box": box_label,
                        "region": region,
                        "confidence": conf,
                        "source": "text",
                    });
                    col.put_string(key, w.text.trim(), ev);
                    return;
                }
                if is_ssn_token(&w.text) {
                    ssn_refused = true;
                }
            }
        }
    }
    if ssn_refused {
        col.warnings.push(format!(
            "{}: an SSN-shaped number was found near the label and deliberately NOT emitted; only EIN-shaped payer/employer IDs are extracted",
            key
        ));
    } else if required {
        col.missing_required.push(key.to_string());
        col.warnings.push(format!(
            "could not locate an EIN-shaped value for {}; field omitted",
            key
        ));
    }
}

// ---------------------------------------------------------------------------
// AcroForm pass
// ---------------------------------------------------------------------------

struct AcroSpec {
    key: &'static str,
    hints: &'static [&'static str],
    is_money: bool,
}

/// Try AcroForm fields for the given specs. Returns the set of keys
/// filled (and whether any field existed at all).
fn acroform_pass(path: &Path, specs: &[AcroSpec], col: &mut Collector) -> bool {
    let Ok(fields) = tax_pdf::inspect_fields(path) else {
        return false;
    };
    let mut any = false;
    let mut consumed: Vec<bool> = vec![false; fields.len()];
    for spec in specs {
        for (i, f) in fields.iter().enumerate() {
            if consumed[i] {
                continue;
            }
            let Some(value) = &f.value else { continue };
            if value.trim().is_empty() {
                continue;
            }
            let nname = norm(&f.name);
            if !spec.hints.iter().any(|h| nname.contains(&norm(h))) {
                continue;
            }
            if spec.is_money {
                let Some(cents) = parse_money_token(value, false) else {
                    continue;
                };
                consumed[i] = true;
                any = true;
                let ev = json!({
                    "source": "acroform",
                    "field": f.name,
                    "raw": value,
                    "confidence": "high",
                });
                col.put_money(spec.key, cents, ev);
            } else {
                consumed[i] = true;
                any = true;
                let ev = json!({
                    "source": "acroform",
                    "field": f.name,
                    "raw": value,
                    "confidence": "high",
                });
                col.put_string(spec.key, value.trim(), ev);
            }
            break;
        }
    }
    any
}

/// Classify from AcroForm field names when the text layer gave no
/// title (some issuers produce nearly text-free fillable copies).
fn classify_acroform_names(path: &Path) -> Option<DocKind> {
    let fields = tax_pdf::inspect_fields(path).ok()?;
    let joined: String = fields
        .iter()
        .map(|f| norm(&f.name))
        .collect::<Vec<_>>()
        .join(" ");
    if joined.contains("1099int") || joined.contains("int1099") {
        Some(DocKind::I1099Int)
    } else if joined.contains("1099div") || joined.contains("div1099") {
        Some(DocKind::I1099Div)
    } else if joined.contains("1099nec") || joined.contains("nec1099") {
        Some(DocKind::I1099Nec)
    } else if joined.contains("w2") {
        Some(DocKind::W2)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractStatus {
    Extracted,
    Partial,
    NeedsOcr,
    Unrecognized,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractReport {
    /// "w2" | "form_1099_int" | "form_1099_div" | "form_1099_nec" for
    /// extractable kinds; the classify kind string otherwise; null when
    /// the document could not be classified at all.
    pub form: Option<String>,
    pub status: ExtractStatus,
    /// "text" | "acroform" | "acroform+text" | null.
    pub method: Option<String>,
    /// The `tax form add --node-type` value for `node`, when applicable.
    pub node_type: Option<String>,
    /// A payload shaped exactly like the `form add` entry (plus a
    /// "type" key repeating `node_type`); null when nothing usable was
    /// extracted.
    pub node: Option<Value>,
    /// Extracted values by payload key (money = string of cents).
    pub fields: Map<String, Value>,
    /// Per-field evidence: page, matched label text, raw token, box.
    pub evidence: Map<String, Value>,
    /// Deterministic cross-check findings (same shape as validate).
    pub checks: Vec<Finding>,
    pub warnings: Vec<String>,
}

fn form_key(kind: DocKind) -> String {
    match kind {
        DocKind::W2 => "w2".to_string(),
        DocKind::I1099Int => "form_1099_int".to_string(),
        DocKind::I1099Div => "form_1099_div".to_string(),
        DocKind::I1099Nec => "form_1099_nec".to_string(),
        other => other.as_str().to_string(),
    }
}

fn finding(code: &str, severity: &str, message: String, field: Option<&str>) -> Finding {
    Finding {
        code: code.to_string(),
        severity: severity.to_string(),
        message,
        field: field.map(str::to_string),
    }
}

fn make_node(node_type: &str, fields: &Map<String, Value>) -> Option<Value> {
    if fields.is_empty() {
        return None;
    }
    let mut node = Map::new();
    node.insert("type".to_string(), Value::String(node_type.to_string()));
    for (k, v) in fields {
        node.insert(k.clone(), v.clone());
    }
    Some(Value::Object(node))
}

// ---------------------------------------------------------------------------
// Per-form drivers
// ---------------------------------------------------------------------------

const NAME_CONTINUATION_MARKERS: &[&str] = &[
    "streetaddress",
    "cityortown",
    "stateorprovince",
    "postalcode",
    "telephoneno",
    "countryzip",
    "orforeign",
];

fn extract_w2(
    path: &Path,
    words: &[Word],
    pages: &[Vec<Line>],
    params: &TaxParams,
) -> ExtractReport {
    let mut col = Collector::default();
    let acro_specs = [
        AcroSpec {
            key: "social_security_wages",
            hints: &["box3", "socialsecuritywages", "sswages"],
            is_money: true,
        },
        AcroSpec {
            key: "social_security_tax",
            hints: &["box4", "socialsecuritytax", "sstax"],
            is_money: true,
        },
        AcroSpec {
            key: "medicare_wages",
            hints: &["box5", "medicarewages"],
            is_money: true,
        },
        AcroSpec {
            key: "medicare_tax",
            hints: &["box6", "medicaretax"],
            is_money: true,
        },
        AcroSpec {
            key: "wages",
            hints: &["box1", "wages"],
            is_money: true,
        },
        AcroSpec {
            key: "federal_withholding",
            hints: &["box2", "federal"],
            is_money: true,
        },
        AcroSpec {
            key: "employer_ein",
            hints: &["ein", "employeri"],
            is_money: false,
        },
        AcroSpec {
            key: "employer_name",
            hints: &["employername", "boxc"],
            is_money: false,
        },
    ];
    let acro_any = acroform_pass(path, &acro_specs, &mut col);
    let from_acro: Vec<String> = col.fields.keys().cloned().collect();

    // Text pass for whatever AcroForm did not provide.
    let money_specs = [
        MoneySpec {
            key: "wages",
            phrases: &["wages", "tips", "other", "compensation"],
            box_label: "1",
            required: true,
            empty_means_zero: false,
        },
        MoneySpec {
            key: "federal_withholding",
            phrases: &["federal", "income", "tax", "withheld"],
            box_label: "2",
            required: true,
            empty_means_zero: false,
        },
        MoneySpec {
            key: "social_security_wages",
            phrases: &["social", "security", "wages"],
            box_label: "3",
            required: false,
            empty_means_zero: false,
        },
        MoneySpec {
            key: "social_security_tax",
            phrases: &["social", "security", "tax", "withheld"],
            box_label: "4",
            required: false,
            empty_means_zero: false,
        },
        MoneySpec {
            key: "medicare_wages",
            phrases: &["medicare", "wages", "and", "tips"],
            box_label: "5",
            required: false,
            empty_means_zero: false,
        },
        MoneySpec {
            key: "medicare_tax",
            phrases: &["medicare", "tax", "withheld"],
            box_label: "6",
            required: false,
            empty_means_zero: false,
        },
    ];
    for spec in &money_specs {
        if !col.fields.contains_key(spec.key) {
            extract_money_field(&mut col, words, pages, spec);
        }
    }
    if !col.fields.contains_key("employer_ein") {
        extract_ein_field(
            &mut col,
            words,
            pages,
            "employer_ein",
            &["employer", "identification", "number"],
            "b",
            false,
        );
    }
    if !col.fields.contains_key("employer_ein") {
        // Issuer variants that print a bare "EIN" caption.
        extract_ein_field(&mut col, words, pages, "employer_ein", &["ein"], "b", true);
    }
    if !col.fields.contains_key("employer_name") {
        extract_name_block(
            &mut col,
            words,
            pages,
            "employer_name",
            &["employers", "name", "address", "and", "zip", "code"],
            NAME_CONTINUATION_MARKERS,
        );
    }

    // Cross-checks once the core boxes are present.
    let mut checks = Vec::new();
    if let (Some(wages), Some(wh)) = (
        col.fields.get("wages").and_then(|v| v.as_str()),
        col.fields
            .get("federal_withholding")
            .and_then(|v| v.as_str()),
    ) {
        let money = |k: &str| {
            col.fields
                .get(k)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .map(Money::from_cents)
        };
        let input = W2Input {
            employer_ein: col
                .fields
                .get("employer_ein")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            wages: Money::from_cents(wages.parse().unwrap_or(0)),
            federal_withholding: Money::from_cents(wh.parse().unwrap_or(0)),
            social_security_wages: money("social_security_wages"),
            social_security_tax: money("social_security_tax"),
            medicare_wages: money("medicare_wages"),
            medicare_tax: money("medicare_tax"),
            state_wages: None,
            state_withholding: None,
        };
        checks = validate_w2(&input, params);
    } else {
        checks.push(finding(
            "w2_checks_skipped",
            "info",
            "W-2 rate cross-checks skipped — Box 1 and/or Box 2 were not extracted".to_string(),
            None,
        ));
    }

    finish_report("w2", "w2", col, checks, acro_any, &from_acro)
}

fn extract_1099_int(path: &Path, words: &[Word], pages: &[Vec<Line>]) -> ExtractReport {
    let mut col = Collector::default();
    let acro_specs = [
        AcroSpec {
            key: "interest_income",
            hints: &["box1", "interestincome"],
            is_money: true,
        },
        AcroSpec {
            key: "tax_exempt_interest",
            hints: &["box8", "taxexempt"],
            is_money: true,
        },
        AcroSpec {
            key: "federal_withholding",
            hints: &["box4", "federal"],
            is_money: true,
        },
        AcroSpec {
            key: "payer_name",
            hints: &["payername", "payer"],
            is_money: false,
        },
    ];
    let acro_any = acroform_pass(path, &acro_specs, &mut col);
    let from_acro: Vec<String> = col.fields.keys().cloned().collect();
    let specs = [
        MoneySpec {
            key: "interest_income",
            phrases: &["interest", "income"],
            box_label: "1",
            required: true,
            empty_means_zero: false,
        },
        MoneySpec {
            key: "tax_exempt_interest",
            phrases: &["tax-exempt", "interest"],
            box_label: "8",
            required: false,
            empty_means_zero: true,
        },
        MoneySpec {
            key: "federal_withholding",
            phrases: &["federal", "income", "tax", "withheld"],
            box_label: "4",
            required: false,
            empty_means_zero: true,
        },
    ];
    for spec in &specs {
        if !col.fields.contains_key(spec.key) {
            extract_money_field(&mut col, words, pages, spec);
        }
    }
    if !col.fields.contains_key("payer_name") {
        extract_name_block(
            &mut col,
            words,
            pages,
            "payer_name",
            &["payers", "name"],
            NAME_CONTINUATION_MARKERS,
        );
    }
    if !col.fields.contains_key("payer_name") {
        col.missing_required.push("payer_name".to_string());
        col.warnings
            .push("could not locate the PAYER'S name block; field omitted".to_string());
    }
    finish_report(
        "form_1099_int",
        "1099-int",
        col,
        Vec::new(),
        acro_any,
        &from_acro,
    )
}

fn extract_1099_div(path: &Path, words: &[Word], pages: &[Vec<Line>]) -> ExtractReport {
    let mut col = Collector::default();
    let acro_specs = [
        AcroSpec {
            key: "total_ordinary_dividends",
            hints: &["box1a", "ordinary"],
            is_money: true,
        },
        AcroSpec {
            key: "qualified_dividends",
            hints: &["box1b", "qualified"],
            is_money: true,
        },
        AcroSpec {
            key: "federal_withholding",
            hints: &["box4", "federal"],
            is_money: true,
        },
        AcroSpec {
            key: "payer_name",
            hints: &["payername", "payer"],
            is_money: false,
        },
    ];
    let acro_any = acroform_pass(path, &acro_specs, &mut col);
    let from_acro: Vec<String> = col.fields.keys().cloned().collect();
    let specs = [
        MoneySpec {
            key: "total_ordinary_dividends",
            phrases: &["total", "ordinary", "dividends"],
            box_label: "1a",
            required: true,
            empty_means_zero: false,
        },
        MoneySpec {
            key: "qualified_dividends",
            phrases: &["qualified", "dividends"],
            box_label: "1b",
            required: false,
            empty_means_zero: true,
        },
        MoneySpec {
            key: "federal_withholding",
            phrases: &["federal", "income", "tax", "withheld"],
            box_label: "4",
            required: false,
            empty_means_zero: true,
        },
    ];
    for spec in &specs {
        if !col.fields.contains_key(spec.key) {
            extract_money_field(&mut col, words, pages, spec);
        }
    }
    if !col.fields.contains_key("payer_name") {
        extract_name_block(
            &mut col,
            words,
            pages,
            "payer_name",
            &["payers", "name"],
            NAME_CONTINUATION_MARKERS,
        );
    }
    if !col.fields.contains_key("payer_name") {
        col.missing_required.push("payer_name".to_string());
        col.warnings
            .push("could not locate the PAYER'S name block; field omitted".to_string());
    }
    // Consistency check: qualified can never exceed ordinary.
    let mut checks = Vec::new();
    let cents = |k: &str| {
        col.fields
            .get(k)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<i64>().ok())
    };
    if let (Some(ord), Some(qual)) = (
        cents("total_ordinary_dividends"),
        cents("qualified_dividends"),
    ) {
        if qual > ord {
            checks.push(finding(
                "div_qualified_exceeds_ordinary",
                "error",
                format!(
                    "1099-DIV Box 1b qualified dividends ({}) exceed Box 1a total ordinary dividends ({})",
                    Money::from_cents(qual),
                    Money::from_cents(ord)
                ),
                Some("qualified_dividends"),
            ));
        }
    }
    finish_report(
        "form_1099_div",
        "1099-div",
        col,
        checks,
        acro_any,
        &from_acro,
    )
}

fn extract_1099_nec(path: &Path, words: &[Word], pages: &[Vec<Line>]) -> ExtractReport {
    let mut col = Collector::default();
    let acro_specs = [
        AcroSpec {
            key: "nonemployee_compensation",
            hints: &["box1", "nonemployee"],
            is_money: true,
        },
        AcroSpec {
            key: "federal_withholding",
            hints: &["box4", "federal"],
            is_money: true,
        },
        AcroSpec {
            key: "payer_ein",
            hints: &["payerstin", "payerein", "tin"],
            is_money: false,
        },
        AcroSpec {
            key: "payer_name",
            hints: &["payername", "payer"],
            is_money: false,
        },
    ];
    let acro_any = acroform_pass(path, &acro_specs, &mut col);
    let from_acro: Vec<String> = col.fields.keys().cloned().collect();
    let specs = [
        MoneySpec {
            key: "nonemployee_compensation",
            phrases: &["nonemployee", "compensation"],
            box_label: "1",
            required: true,
            empty_means_zero: false,
        },
        MoneySpec {
            key: "federal_withholding",
            phrases: &["federal", "income", "tax", "withheld"],
            box_label: "4",
            required: false,
            empty_means_zero: true,
        },
    ];
    for spec in &specs {
        if !col.fields.contains_key(spec.key) {
            extract_money_field(&mut col, words, pages, spec);
        }
    }
    if !col.fields.contains_key("payer_ein") {
        extract_ein_field(
            &mut col,
            words,
            pages,
            "payer_ein",
            &["payers", "tin"],
            "PAYER'S TIN",
            false,
        );
    } else if let Some(v) = col
        .fields
        .get("payer_ein")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    {
        // An AcroForm-supplied TIN in SSN shape is a person's SSN in an
        // identification slot: refuse to carry it, like the text path.
        if is_ssn_token(&v) {
            col.fields.remove("payer_ein");
            col.evidence.remove("payer_ein");
            col.warnings.push(
                "payer_ein: the PAYER'S TIN is in SSN shape and was deliberately NOT emitted; only EIN-shaped payer IDs are extracted"
                    .to_string(),
            );
        }
    }
    if !col.fields.contains_key("payer_name") {
        extract_name_block(
            &mut col,
            words,
            pages,
            "payer_name",
            &["payers", "name"],
            NAME_CONTINUATION_MARKERS,
        );
    }
    if !col.fields.contains_key("payer_name") {
        col.missing_required.push("payer_name".to_string());
        col.warnings
            .push("could not locate the PAYER'S name block; field omitted".to_string());
    }
    col.warnings.push(
        "the 1099-NEC entry's `expenses` field is not printed on the form and is never extracted — the filer supplies it from their records (tax-ingest skill)"
            .to_string(),
    );
    finish_report(
        "form_1099_nec",
        "1099-nec",
        col,
        Vec::new(),
        acro_any,
        &from_acro,
    )
}

fn finish_report(
    form: &str,
    node_type: &str,
    col: Collector,
    checks: Vec<Finding>,
    acro_any: bool,
    from_acro: &[String],
) -> ExtractReport {
    let status = if col.missing_required.is_empty() {
        ExtractStatus::Extracted
    } else {
        ExtractStatus::Partial
    };
    let method = if !acro_any {
        Some("text".to_string())
    } else if from_acro.len() == col.fields.len() {
        Some("acroform".to_string())
    } else {
        Some("acroform+text".to_string())
    };
    let node = make_node(node_type, &col.fields);
    ExtractReport {
        form: Some(form.to_string()),
        status,
        method,
        node_type: Some(node_type.to_string()),
        node,
        fields: col.fields,
        evidence: col.evidence,
        checks,
        warnings: col.warnings,
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Extract a document at `path`. Born-digital PDFs only: a document
/// with no usable text layer reports `needs_ocr` (v1 ships no OCR
/// backend); an unrecognized document reports `unrecognized`. Values
/// are never guessed.
pub fn extract_document(path: &Path, params: &TaxParams) -> Result<ExtractReport, String> {
    let words = extract_words(path)?;
    let total_chars: usize = words.iter().map(|w| w.text.chars().count()).sum();

    // Classification text: words in reading order, space-joined.
    let pages_map: Vec<Vec<Line>> = {
        let lines = group_lines(&words);
        let mut by_page: std::collections::BTreeMap<u32, Vec<Line>> =
            std::collections::BTreeMap::new();
        for line in lines {
            by_page.entry(line.page).or_default().push(line);
        }
        by_page.into_values().collect()
    };
    let class_text = {
        let mut parts: Vec<String> = Vec::new();
        for lines in &pages_map {
            for line in lines {
                for &wi in &line.words {
                    parts.push(words[wi].text.clone());
                }
            }
        }
        parts.join(" ")
    };

    if total_chars < 25 {
        let kind = classify_text(&class_text);
        return Ok(ExtractReport {
            form: kind.map(form_key),
            status: ExtractStatus::NeedsOcr,
            method: None,
            node_type: None,
            node: None,
            fields: Map::new(),
            evidence: Map::new(),
            checks: Vec::new(),
            warnings: vec![format!(
                "document has no usable text layer ({total_chars} characters extracted); it is scanned or image-only. v1 ships no OCR backend (NoOcr) — route it to the agent-vision path (tax-ingest skill); no values were guessed"
            )],
        });
    }

    let kind = classify_text(&class_text).or_else(|| classify_acroform_names(path));
    let kind = match kind {
        Some(k) => k,
        None => {
            return Ok(ExtractReport {
                form: None,
                status: ExtractStatus::Unrecognized,
                method: None,
                node_type: None,
                node: None,
                fields: Map::new(),
                evidence: Map::new(),
                checks: Vec::new(),
                warnings: vec![
                    "no known form title found in the text layer and no recognizable AcroForm field names; the document type was not guessed — route to the agent-vision path (tax-ingest skill)"
                        .to_string(),
                ],
            });
        }
    };

    match kind {
        DocKind::W2 => Ok(extract_w2(path, &words, &pages_map, params)),
        DocKind::I1099Int => Ok(extract_1099_int(path, &words, &pages_map)),
        DocKind::I1099Div => Ok(extract_1099_div(path, &words, &pages_map)),
        DocKind::I1099Nec => Ok(extract_1099_nec(path, &words, &pages_map)),
        other => Ok(ExtractReport {
            form: Some(form_key(other)),
            status: ExtractStatus::Unrecognized,
            method: None,
            node_type: None,
            node: None,
            fields: Map::new(),
            evidence: Map::new(),
            checks: Vec::new(),
            warnings: vec![match other {
                DocKind::I1099B => "1099-B broker statements are deliberately NOT extracted in v1: layouts vary too much for label-anchored rules; use the agent-vision path (tax-ingest skill)".to_string(),
                DocKind::I1099R => "1099-R extraction is not implemented in v1; use the agent-vision path (tax-ingest skill)".to_string(),
                _ => format!("{} documents are recognized but not extractable in v1; use the agent-vision path (tax-ingest skill)", other.as_str()),
            }],
        }),
    }
}
