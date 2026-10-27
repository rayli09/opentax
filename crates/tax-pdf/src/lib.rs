//! tax-pdf: AcroForm inspection and filling (pure Rust).
//!
//! Backends behind the [`PdfBackend`] trait:
//! - [`LopdfBackend`] (lopdf 0.46) — the inspect/text-extraction default. Its
//!   low-level [`fill_fields`] sets `/V` only and regenerates NO appearance
//!   streams; the PDF fill spike (docs/PDF_SPIKE.md) proved that path cannot
//!   fill real IRS forms (no `/AP`, wrong-typed checkbox values).
//! - [`PdferBackend`] (pdfer_forms + lopdf button pass) — the default FILL
//!   backend since the spike: pdfer_forms regenerates widget `/AP` for
//!   non-button fields, while button fields (`/Btn` checkboxes and radios)
//!   are filled by a direct lopdf pass that resolves the exact qualified
//!   name and sets `/V` + per-widget `/AS` as proper names (pdfer_forms
//!   matches buttons by leaf name and over-sets same-leaf siblings). Every
//!   fill is audited (see [`FillReport`]): fields that changed but were
//!   not requested fail the fill.
//!
//! Verify filled output independently — never trust the filling library to
//! grade its own work.

use lopdf::{Dictionary, Document, Object, ObjectId};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldInfo {
    /// Fully-qualified field name (dot-joined partial names).
    pub name: String,
    /// `/FT` value, e.g. "Tx" (text), "Btn" (button/checkbox), "Ch" (choice).
    pub field_type: Option<String>,
    /// Current `/V` if present, rendered as a string.
    pub value: Option<String>,
}

#[derive(Debug)]
pub struct PdfError(pub String);

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PdfError {}

type PdfResult<T> = Result<T, PdfError>;

fn err<E: std::fmt::Display>(e: E) -> PdfError {
    PdfError(e.to_string())
}

/// Warning attached to every fill report: AcroForm filling does not update
/// the XFA datasets of XFA+AcroForm hybrid forms (all current IRS forms).
/// We PRESERVE the XFA layer as-is (never strip it silently); viewers that
/// prefer the XFA layer may show stale data — an explicit, documented
/// behaviour, see docs/KNOWN_LIMITS.md.
pub const XFA_WARNING: &str = "XFA datasets not updated by AcroForm fill (preserved as-is); \
     viewers that prefer the XFA layer may show stale data";

/// Warning attached to lopdf-backend fills.
pub const LOPDF_FILL_WARNING: &str = "lopdf backend sets /V only and regenerates no /AP \
     appearance streams; filled values may not render in viewers that do not \
     regenerate appearances (spike verdict, docs/PDF_SPIKE.md)";

/// The audited result of a fill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FillReport {
    /// Backend that produced the fill ("lopdf" / "pdfer_forms" /
    /// "pdfer_forms+lopdf-buttons" when button fields went through the
    /// direct lopdf button pass).
    pub backend: String,
    /// Fields whose `/V` actually changed, per the post-fill audit.
    pub changed_fields: Vec<String>,
    /// Changed fields that were NOT requested. Non-empty here is only
    /// reachable for backends that do not audit; the pdfer backend returns
    /// an `Err` instead of a report in that case (see `PdferBackend`).
    pub unintended_changes: Vec<String>,
    /// How many of the changed fields have at least one widget with a
    /// non-empty `/AP` appearance stream in the output.
    pub appearance_verified: usize,
    /// Non-fatal caveats (XFA, comb rendering, duplicate leaf names…).
    pub warnings: Vec<String>,
}

/// A PDF backend. Inspection defaults to lopdf; filling defaults to
/// pdfer_forms ([`PdferBackend`]). Both work on file paths only.
pub trait PdfBackend {
    fn name(&self) -> &'static str;
    fn inspect_fields(&self, path: &Path) -> PdfResult<Vec<FieldInfo>>;
    fn fill_fields(
        &self,
        input: &Path,
        values: &HashMap<String, String>,
        output: &Path,
    ) -> PdfResult<FillReport>;
    fn extract_text(&self, path: &Path) -> PdfResult<String>;
}

/// Pure-Rust lopdf backend: inspect/extract default, honest fill fallback.
#[derive(Debug, Default, Clone, Copy)]
pub struct LopdfBackend;

impl PdfBackend for LopdfBackend {
    fn name(&self) -> &'static str {
        "lopdf"
    }
    fn inspect_fields(&self, path: &Path) -> PdfResult<Vec<FieldInfo>> {
        inspect_fields(path)
    }
    fn fill_fields(
        &self,
        input: &Path,
        values: &HashMap<String, String>,
        output: &Path,
    ) -> PdfResult<FillReport> {
        let changed = fill_fields(input, values, output)?;
        Ok(FillReport {
            backend: "lopdf".to_string(),
            changed_fields: changed,
            unintended_changes: Vec::new(),
            // lopdf regenerates no appearances — report 0 honestly rather
            // than counting pre-existing /AP streams as our work.
            appearance_verified: 0,
            warnings: vec![LOPDF_FILL_WARNING.to_string(), XFA_WARNING.to_string()],
        })
    }
    fn extract_text(&self, path: &Path) -> PdfResult<String> {
        extract_text(path)
    }
}

/// Default fill backend: pdfer_forms (regenerates `/AP`), with a pre-flight
/// duplicate-leaf guard and a post-fill audit. Works purely on file paths,
/// so pdfer_forms' own lopdf 0.40 never mixes with this crate's lopdf 0.46
/// `Document` objects.
#[derive(Debug, Default, Clone, Copy)]
pub struct PdferBackend;

impl PdfBackend for PdferBackend {
    fn name(&self) -> &'static str {
        "pdfer_forms"
    }
    fn inspect_fields(&self, path: &Path) -> PdfResult<Vec<FieldInfo>> {
        inspect_fields(path)
    }
    fn fill_fields(
        &self,
        input: &Path,
        values: &HashMap<String, String>,
        output: &Path,
    ) -> PdfResult<FillReport> {
        pdfer_fill(input, values, output)
    }
    fn extract_text(&self, path: &Path) -> PdfResult<String> {
        extract_text(path)
    }
}

// ---------------------------------------------------------------------------
// PDF string decoding
// ---------------------------------------------------------------------------

/// Decode a PDF *text string* (PDF 32000-1 §7.3.4.2): the bytes of a string
/// object used for human-facing text — field names (`/T`), string field
/// values (`/V`), etc.
///
/// - `FE FF` prefix → UTF-16BE (this is how LiveCycle Designer writes every
///   `/T` on real IRS forms; decoding it as UTF-8/lossy mangles 100% of
///   those names — the bug the fill spike found, docs/PDF_SPIKE.md).
/// - `FF FE` prefix → UTF-16LE.
/// - Otherwise PDFDocEncoding: ASCII for 0x00–0x7F (with the documented
///   diacritic block at 0x18–0x1F), the PDFDocEncoding table for 0x80–0x9F,
///   U+20AC (Euro) at 0xA0, and Latin-1 for 0xA1–0xFF. Codes PDFDocEncoding
///   leaves undefined decode to U+FFFD. This is a deliberate, documented
///   approximation of the full PDFDocEncoding table — exact for every byte
///   that occurs in IRS form field names and values in practice.
pub fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    bytes.iter().map(|&b| pdf_doc_encoding_char(b)).collect()
}

fn pdf_doc_encoding_char(b: u8) -> char {
    match b {
        0x00..=0x17 => b as char, // control range: unused in practice, kept 1:1
        0x18 => '\u{02D8}',       // breve
        0x19 => '\u{02C7}',       // caron
        0x1A => '\u{02C6}',       // circumflex modifier
        0x1B => '\u{02D9}',       // dot above
        0x1C => '\u{02DD}',       // double acute
        0x1D => '\u{02DB}',       // ogonek
        0x1E => '\u{02DA}',       // ring above
        0x1F => '\u{02DC}',       // small tilde
        0x20..=0x7F => b as char, // ASCII (+ DEL, kept 1:1)
        0x80 => '\u{2022}',       // bullet
        0x81 => '\u{2020}',       // dagger
        0x82 => '\u{2021}',       // double dagger
        0x83 => '\u{2026}',       // ellipsis
        0x84 => '\u{2014}',       // em dash
        0x85 => '\u{2013}',       // en dash
        0x86 => '\u{0192}',       // florin
        0x87 => '\u{2044}',       // fraction slash
        0x88 => '\u{2039}',       // single left guillemet
        0x89 => '\u{203A}',       // single right guillemet
        0x8A => '\u{2212}',       // minus
        0x8B => '\u{2030}',       // per mille
        0x8C => '\u{201E}',       // double low-9 quote
        0x8D => '\u{201C}',       // left double quote
        0x8E => '\u{201D}',       // right double quote
        0x8F => '\u{2018}',       // left single quote
        0x90 => '\u{2019}',       // right single quote
        0x91 => '\u{201A}',       // single low-9 quote
        0x92 => '\u{2122}',       // trademark
        0x93 => '\u{FB01}',       // fi ligature
        0x94 => '\u{FB02}',       // fl ligature
        0x95 => '\u{0141}',       // Ł
        0x96 => '\u{0152}',       // Œ
        0x97 => '\u{0160}',       // Š
        0x98 => '\u{0178}',       // Ÿ
        0x99 => '\u{017D}',       // Ž
        0x9A => '\u{0131}',       // dotless i
        0x9B => '\u{0142}',       // ł
        0x9C => '\u{0153}',       // œ
        0x9D => '\u{0161}',       // š
        0x9E => '\u{017E}',       // ž
        0x9F => '\u{FFFD}',       // undefined in PDFDocEncoding
        0xA0 => '\u{20AC}',       // Euro (PDFDocEncoding differs from Latin-1 here)
        0xA1..=0xFF => b as char, // Latin-1 supplement, identical in PDFDocEncoding
    }
}

/// Decode a PDF *name* object (e.g. an `/FT` value, an `/AS` appearance
/// state, a checkbox export state). Names are NOT text strings: they are
/// 7-bit ASCII by convention and never UTF-16, so they get their own helper
/// to keep the two decodings from being confused again.
fn decode_pdf_name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------------------
// Field tree walking
// ---------------------------------------------------------------------------

struct FieldAcc {
    name: String,
    field_type: Option<String>,
    value: Option<String>,
    /// Inherited-or-own `/Ff` field flags (bit 25, value 1<<24, = Comb).
    flags: u32,
    id: ObjectId,
}

/// Collect fields from a Fields/Kids array, joining partial names.
fn walk_fields(
    doc: &Document,
    refs: &[Object],
    parent_name: &str,
    inherited_ft: Option<String>,
    inherited_flags: u32,
    out: &mut Vec<FieldAcc>,
) {
    for item in refs {
        let Ok(id) = item.as_reference() else {
            continue;
        };
        let Ok(dict) = doc.get_dictionary(id) else {
            continue;
        };
        let partial = dict
            .get(b"T")
            .ok()
            .and_then(|o| o.as_str().ok())
            .map(decode_pdf_string);
        let name = match (&partial, parent_name.is_empty()) {
            (Some(p), false) => format!("{parent_name}.{p}"),
            (Some(p), true) => p.clone(),
            (None, _) => parent_name.to_string(),
        };
        let ft = dict
            .get(b"FT")
            .ok()
            .and_then(|o| o.as_name().ok())
            .map(decode_pdf_name)
            .or_else(|| inherited_ft.clone());
        let flags = dict
            .get(b"Ff")
            .ok()
            .and_then(|o| o.as_i64().ok())
            .map(|f| f as u32)
            .unwrap_or(inherited_flags);
        let value = dict.get(b"V").ok().and_then(object_to_string);
        let kids: Vec<Object> = dict
            .get(b"Kids")
            .ok()
            .and_then(|o| o.as_array().ok())
            .cloned()
            .unwrap_or_default();
        let kid_is_widget_only = !kids.is_empty()
            && kids.iter().all(|k| {
                let Ok(kid_id) = k.as_reference() else {
                    return false;
                };
                match doc.get_dictionary(kid_id) {
                    Ok(d) => d.get(b"T").is_err(),
                    Err(_) => false,
                }
            });
        // Record a node when it is a field: it has a fully-qualified name and
        // either a field type or it is a merged field+widget (kids are bare
        // widget annotations). Pure container nodes (no FT, named kids) are
        // traversed but not recorded.
        if !name.is_empty() && (ft.is_some() || kid_is_widget_only) {
            out.push(FieldAcc {
                name: name.clone(),
                field_type: ft.clone(),
                value,
                flags,
                id,
            });
        }
        if !kids.is_empty() && !kid_is_widget_only {
            walk_fields(doc, &kids, &name, ft, flags, out);
        }
    }
}

fn object_to_string(o: &Object) -> Option<String> {
    match o {
        // /V as a string IS a PDF text string (may be UTF-16BE on IRS forms).
        Object::String(bytes, _) => Some(decode_pdf_string(bytes)),
        // /V as a name (checkbox/button state) is a PDF name, not a string.
        Object::Name(n) => Some(decode_pdf_name(n)),
        Object::Integer(i) => Some(i.to_string()),
        Object::Real(r) => Some(r.to_string()),
        Object::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

fn acroform_field_refs(doc: &Document) -> PdfResult<Vec<Object>> {
    let catalog = doc.catalog().map_err(err)?;
    let Ok(acro) = catalog.get(b"AcroForm") else {
        return Ok(Vec::new());
    };
    let acro_owned: Dictionary = match acro {
        Object::Reference(id) => doc.get_dictionary(*id).map_err(err)?.clone(),
        Object::Dictionary(d) => d.clone(),
        _ => return Ok(Vec::new()),
    };
    Ok(acro_owned
        .get(b"Fields")
        .ok()
        .and_then(|o| o.as_array().ok())
        .cloned()
        .unwrap_or_default())
}

fn collect_fields(doc: &Document) -> PdfResult<Vec<FieldAcc>> {
    let refs = acroform_field_refs(doc)?;
    let mut out = Vec::new();
    walk_fields(doc, &refs, "", None, 0, &mut out);
    Ok(out)
}

/// Leaf segment of a qualified field name (after the last `.`).
fn leaf_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Does this widget dictionary carry a non-empty `/AP` `/N` appearance?
fn widget_has_appearance(doc: &Document, dict: &Dictionary) -> bool {
    let Ok(ap) = dict.get(b"AP") else {
        return false;
    };
    let ap_owned: Option<Dictionary> = match ap {
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        Object::Dictionary(d) => Some(d.clone()),
        _ => None,
    };
    let Some(ap_dict) = ap_owned else {
        return false;
    };
    match ap_dict.get(b"N") {
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(Object::Stream(s)) => !s.content.is_empty(),
            Ok(_) => true,
            Err(_) => false,
        },
        Ok(Object::Stream(s)) => !s.content.is_empty(),
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Widget annot object ids for a terminal field: its `/Kids` when the kids
/// are bare widget annotations, otherwise the field itself (merged
/// field+widget).
fn button_widgets(doc: &Document, field: &FieldAcc) -> Vec<ObjectId> {
    let Ok(dict) = doc.get_dictionary(field.id) else {
        return vec![field.id];
    };
    let kids: Vec<Object> = dict
        .get(b"Kids")
        .ok()
        .and_then(|o| o.as_array().ok())
        .cloned()
        .unwrap_or_default();
    let widget_kids: Vec<ObjectId> = kids
        .iter()
        .filter_map(|k| k.as_reference().ok())
        .filter(|kid_id| {
            doc.get_dictionary(*kid_id)
                .map(|d| d.get(b"T").is_err())
                .unwrap_or(false)
        })
        .collect();
    if widget_kids.is_empty() {
        vec![field.id]
    } else {
        widget_kids
    }
}

/// State names in a widget's `/AP` → `/N` dictionary (e.g. `["1", "Off"]`
/// for a checkbox widget). Empty when `/N` is a bare stream (single
/// appearance, no named states) or absent.
fn widget_ap_n_states(doc: &Document, dict: &Dictionary) -> Vec<String> {
    let Ok(ap) = dict.get(b"AP") else {
        return Vec::new();
    };
    let ap_owned: Option<Dictionary> = match ap {
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        Object::Dictionary(d) => Some(d.clone()),
        _ => None,
    };
    let Some(ap_dict) = ap_owned else {
        return Vec::new();
    };
    let n_owned: Option<Dictionary> = match ap_dict.get(b"N") {
        Ok(Object::Reference(id)) => doc.get_dictionary(*id).ok().cloned(),
        Ok(Object::Dictionary(d)) => Some(d.clone()),
        _ => None,
    };
    let Some(n_dict) = n_owned else {
        return Vec::new();
    };
    let mut states: Vec<String> = n_dict.iter().map(|(k, _)| decode_pdf_name(k)).collect();
    states.sort();
    states
}

/// Count, among `names`, the fields that have at least one widget with a
/// non-empty `/AP` in this document (the field itself if merged
/// field+widget, else any of its widget kids). Button fields are counted
/// only when the widget selected by `/AS` (matching the field's `/V`
/// state) carries that state in its `/AP` → `/N` dictionary — Designer
/// widgets ship their on-state appearance templates in the original form,
/// and the direct button pass selects among them rather than generating
/// new streams.
fn count_fields_with_appearance(
    doc: &Document,
    fields: &[FieldAcc],
    names: &HashSet<String>,
) -> usize {
    let mut count = 0;
    for field in fields.iter().filter(|f| names.contains(&f.name)) {
        let Ok(dict) = doc.get_dictionary(field.id) else {
            continue;
        };
        if field.field_type.as_deref() == Some("Btn") {
            let Some(state) = field.value.as_deref() else {
                continue;
            };
            let selected = button_widgets(doc, field).into_iter().any(|wid| {
                let Ok(wdict) = doc.get_dictionary(wid) else {
                    return false;
                };
                let as_state = wdict
                    .get(b"AS")
                    .ok()
                    .and_then(|o| o.as_name().ok())
                    .map(decode_pdf_name);
                as_state.as_deref() == Some(state)
                    && widget_ap_n_states(doc, wdict).iter().any(|s| s == state)
            });
            if selected {
                count += 1;
            }
            continue;
        }
        if widget_has_appearance(doc, dict) {
            count += 1;
            continue;
        }
        let kids: Vec<Object> = dict
            .get(b"Kids")
            .ok()
            .and_then(|o| o.as_array().ok())
            .cloned()
            .unwrap_or_default();
        let mut found = false;
        for kid in &kids {
            if let Ok(kid_id) = kid.as_reference() {
                if let Ok(kid_dict) = doc.get_dictionary(kid_id) {
                    if widget_has_appearance(doc, kid_dict) {
                        found = true;
                        break;
                    }
                }
            }
        }
        if found {
            count += 1;
        }
    }
    count
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Inspect the AcroForm fields of the PDF at `path`.
pub fn inspect_fields(path: &Path) -> PdfResult<Vec<FieldInfo>> {
    let doc = Document::load(path).map_err(err)?;
    Ok(collect_fields(&doc)?
        .into_iter()
        .map(|f| FieldInfo {
            name: f.name,
            field_type: f.field_type,
            value: f.value,
        })
        .collect())
}

/// Set `/V` on the named fields of the PDF at `input` and write `output`.
///
/// Text fields (`/Tx`) at minimum. Returns the names actually filled.
/// NOTE: appearance streams (`/AP`) are NOT regenerated — see crate docs.
pub fn fill_fields(
    input: &Path,
    values: &HashMap<String, String>,
    output: &Path,
) -> PdfResult<Vec<String>> {
    let mut doc = Document::load(input).map_err(err)?;
    let fields = collect_fields(&doc)?;
    let mut filled = Vec::new();
    for field in fields {
        if let Some(v) = values.get(&field.name) {
            let obj = doc.get_object_mut(field.id).map_err(err)?;
            let dict = obj.as_dict_mut().map_err(err)?;
            dict.set("V", Object::string_literal(v.clone()));
            filled.push(field.name);
        }
    }
    doc.save(output).map_err(err)?;
    Ok(filled)
}

/// Value equality for the audit: a requested button state may be given as
/// `"1"` or `"/1"` while inspect reports the bare name `"1"`.
fn value_matches(requested: &str, actual: Option<&String>) -> bool {
    match actual {
        Some(a) => a == requested || a.as_str() == requested.trim_start_matches('/'),
        None => false,
    }
}

/// Apply button-field (`/Btn`) values directly with lopdf, resolving each
/// field by its EXACT qualified name (never by leaf — the whole point of
/// this path: `pdfer_forms` leaf-matching over-sets same-leaf siblings).
///
/// For each requested field, with `on_state` = the value minus any leading
/// `/` (`"Off"` or empty = uncheck):
/// - the field's `/V` becomes `Object::Name(on_state)`;
/// - each widget's `/AS` becomes the on-state when that widget's
///   `/AP` → `/N` dictionary carries the state (the selected widget of a
///   checkbox or radio group), and `Name("Off")` on every other widget.
///
/// Appearance streams are NOT generated: Designer widgets already carry
/// their on-state `/AP` templates (spike evidence), and this pass selects
/// among them. A requested state that is not among the field's available
/// on-states (union of widget `/AP` → `/N` keys, excluding `Off`) is an
/// error naming the field and the available states — never a guess.
fn set_button_fields(doc: &mut Document, values: &HashMap<String, String>) -> PdfResult<()> {
    let fields = collect_fields(doc)?;
    for (name, raw_value) in values {
        let field = fields
            .iter()
            .find(|f| &f.name == name)
            .ok_or_else(|| PdfError(format!("button field not found: {name}")))?;
        let target = raw_value.trim_start_matches('/');
        let widgets = button_widgets(doc, field);
        // Available on-states across this field's widgets.
        let mut available: Vec<String> = Vec::new();
        let mut per_widget_states: Vec<(ObjectId, Vec<String>)> = Vec::new();
        for wid in &widgets {
            let states = doc
                .get_dictionary(*wid)
                .map(|d| widget_ap_n_states(doc, d))
                .unwrap_or_default();
            for s in &states {
                if s != "Off" && !available.contains(s) {
                    available.push(s.clone());
                }
            }
            per_widget_states.push((*wid, states));
        }
        available.sort();
        let uncheck = target.is_empty() || target == "Off";
        if !uncheck && !available.contains(&target.to_string()) {
            return Err(PdfError(format!(
                "button field {name}: requested on-state '{target}' is not available; \
                 available on-states: {}",
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            )));
        }
        let effective = if uncheck { "Off" } else { target };
        // /V on the field object.
        {
            let obj = doc.get_object_mut(field.id).map_err(err)?;
            let dict = obj.as_dict_mut().map_err(err)?;
            dict.set("V", Object::Name(effective.as_bytes().to_vec()));
        }
        // /AS per widget: selected widget gets the state, others Off.
        for (wid, states) in &per_widget_states {
            let as_state = if !uncheck && states.iter().any(|s| s == target) {
                target
            } else {
                "Off"
            };
            let obj = doc.get_object_mut(*wid).map_err(err)?;
            let dict = obj.as_dict_mut().map_err(err)?;
            dict.set("AS", Object::Name(as_state.as_bytes().to_vec()));
        }
    }
    Ok(())
}

/// Run the direct button pass from `input` to `output` (lopdf load/save).
fn button_fill_file(
    input: &Path,
    values: &HashMap<String, String>,
    output: &Path,
) -> PdfResult<()> {
    let mut doc = Document::load(input).map_err(err)?;
    set_button_fields(&mut doc, values)?;
    doc.save(output).map_err(err)?;
    Ok(())
}

/// Raw pdfer_forms fill (no audit): `input` → `output`.
fn pdfer_fill_raw(input: &Path, values: &HashMap<String, String>, output: &Path) -> PdfResult<()> {
    let reader = pdfer_forms::PdfReaderCompat::load(input).map_err(err)?;
    let mut writer = pdfer_forms::PdfWriterCompat::from_reader(&reader);
    writer.set_need_appearances_writer(false).map_err(err)?;
    let updates: BTreeMap<String, pdfer_forms::FieldInput> = values
        .iter()
        .map(|(k, v)| (k.clone(), pdfer_forms::FieldInput::from(v.clone())))
        .collect();
    writer
        .update_page_form_field_values(
            pdfer_forms::PageSelection::All,
            &updates,
            0,
            Some(false),
            false,
        )
        .map_err(err)?;
    writer.save(output).map_err(err)?;
    Ok(())
}

/// Fill with pre-flight guards and a post-fill audit, partitioning the
/// requested values by field type:
///
/// 1. Pre-flight (lopdf inspect, fixed decoder): every requested name must
///    resolve. Non-button leaf names shared by several fields produce
///    warnings (the audit in step 4 is what actually protects against
///    pdfer_forms' leaf-name matching fallback — spike: two 1040 fields
///    share leaf `c1_8[0]`, and filling one also set the other). Button
///    fields are exempt from that hazard by construction (step 3).
/// 2. Non-button fields fill via pdfer_forms to a temp file (appearances
///    regenerated, `NeedAppearances` explicitly false — appearances are
///    IN the file).
/// 3. Button fields (`/Btn`) fill via the direct lopdf pass
///    ([`set_button_fields`]) on top of step 2's output (or on the input
///    when there are no non-button fields): exact qualified-name
///    resolution, `/V` as a name, per-widget `/AS` selection among the
///    widgets' existing `/AP` state templates.
/// 4. Audit (lopdf inspect of the final temp file): diff every field's
///    `/V` before vs after. Any changed field that was not requested
///    fails the fill — the temp file is deleted and `output` is never
///    written. Requested fields whose value did not land also fail.
/// 5. Appearance check: changed fields should have a widget `/AP` (for
///    buttons: the selected widget's `/AP` → `/N` must carry the state);
///    misses become warnings (e.g. comb fields under pdfer are
///    data-correct but their appearances are not cell-spread — known
///    defect, KNOWN_LIMITS).
fn pdfer_fill(
    input: &Path,
    values: &HashMap<String, String>,
    output: &Path,
) -> PdfResult<FillReport> {
    // ---- pre-flight -------------------------------------------------------
    let before = inspect_fields(input)?;
    let before_map: BTreeMap<String, Option<String>> = before
        .iter()
        .map(|f| (f.name.clone(), f.value.clone()))
        .collect();

    let unknown: Vec<&String> = values
        .keys()
        .filter(|k| !before_map.contains_key(k.as_str()))
        .collect();
    if !unknown.is_empty() {
        return Err(PdfError(format!(
            "unknown field(s) not present in {}: {} — output not written",
            input.display(),
            unknown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    // Partition by field type: /Btn fields go to the direct lopdf button
    // pass; everything else goes through pdfer_forms.
    let type_map: BTreeMap<String, Option<String>> = before
        .iter()
        .map(|f| (f.name.clone(), f.field_type.clone()))
        .collect();
    let mut btn_values: HashMap<String, String> = HashMap::new();
    let mut other_values: HashMap<String, String> = HashMap::new();
    for (k, v) in values {
        if type_map.get(k).cloned().flatten().as_deref() == Some("Btn") {
            btn_values.insert(k.clone(), v.clone());
        } else {
            other_values.insert(k.clone(), v.clone());
        }
    }

    // Duplicate-leaf guard: leaf segment -> all qualified names using it.
    // Only the pdfer (non-button) set is exposed to leaf over-matching;
    // button fields resolve by exact qualified name in the direct pass.
    let mut leaf_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in &before {
        leaf_map
            .entry(leaf_name(&f.name).to_string())
            .or_default()
            .push(f.name.clone());
    }
    let mut warnings: Vec<String> = vec![XFA_WARNING.to_string()];
    for name in other_values.keys() {
        let leaf = leaf_name(name);
        if let Some(sharing) = leaf_map.get(leaf) {
            if sharing.len() > 1 {
                warnings.push(format!(
                    "duplicate leaf name '{leaf}': requested field {name} shares its leaf \
                     with {} — post-fill audit will fail this fill if any of them change",
                    sharing
                        .iter()
                        .filter(|n| *n != name)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    // Comb flags of requested fields (appearance defect warning, step 4).
    let input_doc = Document::load(input).map_err(err)?;
    let input_fields = collect_fields(&input_doc)?;
    let comb_requested: Vec<String> = input_fields
        .iter()
        .filter(|f| values.contains_key(&f.name) && (f.flags >> 24) & 1 == 1)
        .map(|f| f.name.clone())
        .collect();

    // ---- fill to temp -----------------------------------------------------
    // Stage 1 (pdfer, non-button fields only) → stage 2 (direct lopdf
    // button pass). The final staged file is `tmp`; stage 1's is
    // `tmp_pdfer`. The audit below runs on `tmp` exactly as before.
    let tmp: PathBuf = {
        let fname = output
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "filled.pdf".to_string());
        output.with_file_name(format!(".{fname}.tmp-{}", std::process::id()))
    };
    let tmp_pdfer = tmp.with_extension("pdfer.pdf");
    let fill_result = (|| -> PdfResult<()> {
        let mut current: PathBuf = input.to_path_buf();
        if !other_values.is_empty() {
            pdfer_fill_raw(input, &other_values, &tmp_pdfer)?;
            current = tmp_pdfer.clone();
        }
        if !btn_values.is_empty() {
            button_fill_file(&current, &btn_values, &tmp)?;
        } else if current != input {
            std::fs::copy(&current, &tmp).map_err(err)?;
        } else {
            std::fs::copy(input, &tmp).map_err(err)?;
        }
        Ok(())
    })();
    if let Err(e) = fill_result {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&tmp_pdfer);
        return Err(PdfError(format!(
            "{} — output not written ({})",
            e.0,
            output.display()
        )));
    }

    // ---- audit ------------------------------------------------------------
    let audit = (|| -> PdfResult<(Vec<String>, usize)> {
        let after = inspect_fields(&tmp)?;
        let after_map: BTreeMap<String, Option<String>> = after
            .iter()
            .map(|f| (f.name.clone(), f.value.clone()))
            .collect();
        let mut changed: Vec<String> = Vec::new();
        for (name, after_v) in &after_map {
            if before_map.get(name) != Some(after_v) {
                changed.push(name.clone());
            }
        }
        changed.sort();
        let unintended: Vec<String> = changed
            .iter()
            .filter(|n| !values.contains_key(n.as_str()))
            .cloned()
            .collect();
        if !unintended.is_empty() {
            return Err(PdfError(format!(
                "fill audit failed: {} field(s) changed that were not requested: {} — \
                 output not written; this is the duplicate-leaf over-match guard, \
                 request fields with unique leaf names or fill them one at a time \
                 after checking the audit",
                unintended.len(),
                unintended.join(", ")
            )));
        }
        let not_filled: Vec<&String> = values
            .iter()
            .filter(|(k, v)| !value_matches(v, after_map.get(k.as_str()).and_then(|o| o.as_ref())))
            .map(|(k, _)| k)
            .collect();
        if !not_filled.is_empty() {
            return Err(PdfError(format!(
                "fill audit failed: requested field(s) did not take their values: {} — \
                 output not written",
                not_filled
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let out_doc = Document::load(&tmp).map_err(err)?;
        let out_fields = collect_fields(&out_doc)?;
        let changed_set: HashSet<String> = changed.iter().cloned().collect();
        let verified = count_fields_with_appearance(&out_doc, &out_fields, &changed_set);
        Ok((changed, verified))
    })();
    let (changed, appearance_verified) = match audit {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(&tmp_pdfer);
            return Err(e);
        }
    };
    let _ = std::fs::remove_file(&tmp_pdfer);

    // ---- appearance warnings ----------------------------------------------
    if appearance_verified < changed.len() {
        warnings.push(format!(
            "appearance check: only {appearance_verified} of {} changed field(s) have a \
             non-empty /AP appearance stream in the output",
            changed.len()
        ));
    }
    let comb_changed: Vec<&String> = comb_requested
        .iter()
        .filter(|n| changed.contains(n))
        .collect();
    if !comb_changed.is_empty() {
        warnings.push(format!(
            "comb field(s) filled ({}): values are data-correct but pdfer_forms \
             appearances are not spread across the comb cells (known defect, \
             docs/KNOWN_LIMITS.md)",
            comb_changed
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // ---- promote temp to output -------------------------------------------
    std::fs::rename(&tmp, output)
        .or_else(|_| {
            std::fs::copy(&tmp, output)
                .map(|_| ())
                .and_then(|_| std::fs::remove_file(&tmp))
        })
        .map_err(err)?;

    Ok(FillReport {
        backend: if btn_values.is_empty() {
            "pdfer_forms".to_string()
        } else {
            "pdfer_forms+lopdf-buttons".to_string()
        },
        changed_fields: changed,
        unintended_changes: Vec::new(),
        appearance_verified,
        warnings,
    })
}

/// Extract text from all pages (lopdf's built-in extraction: crude, no
/// positions — documented in docs/KNOWN_LIMITS.md).
pub fn extract_text(path: &Path) -> PdfResult<String> {
    let doc = Document::load(path).map_err(err)?;
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    if pages.is_empty() {
        return Ok(String::new());
    }
    doc.extract_text(&pages).map_err(err)
}

/// Build a minimal one-page PDF with a single AcroForm text field.
/// Used for tests and fixtures.
pub fn make_test_pdf(path: &Path, field_name: &str) -> PdfResult<()> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let field_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let catalog_id = doc.new_object_id();

    let mut field = Dictionary::new();
    field.set("Type", Object::Name(b"Annot".to_vec()));
    field.set("Subtype", Object::Name(b"Widget".to_vec()));
    field.set("FT", Object::Name(b"Tx".to_vec()));
    field.set("T", Object::string_literal(field_name));
    field.set(
        "Rect",
        Object::Array(vec![
            Object::Integer(100),
            Object::Integer(700),
            Object::Integer(300),
            Object::Integer(720),
        ]),
    );
    field.set("P", Object::Reference(page_id));
    field.set("F", Object::Integer(4));
    doc.set_object(field_id, field);

    let mut page = Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set("Parent", Object::Reference(pages_id));
    page.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    );
    page.set("Annots", Object::Array(vec![Object::Reference(field_id)]));
    doc.set_object(page_id, page);

    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
    pages.set("Count", Object::Integer(1));
    doc.set_object(pages_id, pages);

    let mut acro = Dictionary::new();
    acro.set("Fields", Object::Array(vec![Object::Reference(field_id)]));
    acro.set("DA", Object::string_literal("/Helv 0 Tf 0 g"));

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    catalog.set("AcroForm", Object::Dictionary(acro));
    doc.set_object(catalog_id, catalog);
    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc.save(path).map_err(err)?;
    Ok(())
}

/// Encode a string as a PDF text string in UTF-16BE with BOM — the encoding
/// LiveCycle Designer uses for every `/T` on real IRS forms.
fn utf16be_pdf_string(s: &str) -> Object {
    let mut bytes = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        bytes.extend_from_slice(&u.to_be_bytes());
    }
    Object::String(bytes, lopdf::StringFormat::Literal)
}

/// Build a richer test form (fixture builder for the pdfer/audit tests):
///
/// ```text
/// form                       (container, no /FT)
/// ├── name_field             (Tx, /T stored as UTF-16BE bytes)
/// ├── agree                  (Btn checkbox, /AP states /Off and /1)
/// ├── a
/// │   ├── dup                (Tx)  ┐ same leaf name "dup"
/// │   └── c[0]               (Btn, separate field + widget kid) ┐ same
/// └── b                      (Tx)  ┘ under different parents    │ leaf
///     ├── dup                                                   │ "c[0]"
///     └── c[0]               (Btn, separate field + widget kid) ┘
/// ```
pub fn make_test_form(path: &Path) -> PdfResult<()> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let catalog_id = doc.new_object_id();
    let font_id = doc.new_object_id();
    let root_id = doc.new_object_id();
    let text_id = doc.new_object_id();
    let check_id = doc.new_object_id();
    let off_stream_id = doc.new_object_id();
    let on_stream_id = doc.new_object_id();
    let dupa_parent_id = doc.new_object_id();
    let dupa_id = doc.new_object_id();
    let dupb_parent_id = doc.new_object_id();
    let dupb_id = doc.new_object_id();
    let ca_field_id = doc.new_object_id();
    let ca_widget_id = doc.new_object_id();
    let cb_field_id = doc.new_object_id();
    let cb_widget_id = doc.new_object_id();

    // Base-14 font (for AcroForm /DR and page resources).
    let mut font = Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"Type1".to_vec()));
    font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
    font.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    doc.set_object(font_id, font);

    let widget = |doc: &mut Document,
                  id: ObjectId,
                  parent: ObjectId,
                  t: Object,
                  ft: &str,
                  y: i64|
     -> Dictionary {
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"Annot".to_vec()));
        d.set("Subtype", Object::Name(b"Widget".to_vec()));
        d.set("FT", Object::Name(ft.as_bytes().to_vec()));
        d.set("T", t);
        d.set(
            "Rect",
            Object::Array(vec![
                Object::Integer(100),
                Object::Integer(y),
                Object::Integer(300),
                Object::Integer(y + 20),
            ]),
        );
        d.set("P", Object::Reference(page_id));
        d.set("Parent", Object::Reference(parent));
        d.set("F", Object::Integer(4));
        doc.set_object(id, d.clone());
        d
    };

    // Text field with a UTF-16BE /T (like a real IRS form).
    widget(
        &mut doc,
        text_id,
        root_id,
        utf16be_pdf_string("name_field"),
        "Tx",
        700,
    );
    // Checkbox with /Off and /1 appearance states.
    let mut check = widget(
        &mut doc,
        check_id,
        root_id,
        Object::string_literal("agree"),
        "Btn",
        660,
    );
    for (sid, _label) in [(off_stream_id, "off"), (on_stream_id, "on")] {
        let mut sdict = Dictionary::new();
        sdict.set("Type", Object::Name(b"XObject".to_vec()));
        sdict.set("Subtype", Object::Name(b"Form".to_vec()));
        sdict.set(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(20),
                Object::Integer(20),
            ]),
        );
        doc.set_object(
            sid,
            Object::Stream(lopdf::Stream::new(sdict, b"q\nQ\n".to_vec())),
        );
    }
    let mut ap_n = Dictionary::new();
    ap_n.set("Off", Object::Reference(off_stream_id));
    ap_n.set("1", Object::Reference(on_stream_id));
    let mut ap = Dictionary::new();
    ap.set("N", Object::Dictionary(ap_n));
    check.set("AP", Object::Dictionary(ap));
    check.set("AS", Object::Name(b"Off".to_vec()));
    doc.set_object(check_id, check);
    // Duplicate-leaf text fields under parents "a" and "b", plus a
    // duplicate-leaf CHECKBOX field under each parent (leaf "c[0]",
    // separate field object with a bare widget kid carrying the /AP state
    // templates — the shape of the real 1040 filing-status collision).
    for (parent_id, widget_id, c_field_id, c_widget_id, tname, y) in [
        (
            dupa_parent_id,
            dupa_id,
            ca_field_id,
            ca_widget_id,
            "a",
            620_i64,
        ),
        (
            dupb_parent_id,
            dupb_id,
            cb_field_id,
            cb_widget_id,
            "b",
            540_i64,
        ),
    ] {
        let mut parent = Dictionary::new();
        parent.set("T", Object::string_literal(tname));
        parent.set(
            "Kids",
            Object::Array(vec![
                Object::Reference(widget_id),
                Object::Reference(c_field_id),
            ]),
        );
        parent.set("Parent", Object::Reference(root_id));
        doc.set_object(parent_id, parent);
        widget(
            &mut doc,
            widget_id,
            parent_id,
            Object::string_literal("dup"),
            "Tx",
            y,
        );
        // Checkbox field (FT lives here) with one widget kid.
        let mut cfield = Dictionary::new();
        cfield.set("FT", Object::Name(b"Btn".to_vec()));
        cfield.set("T", Object::string_literal("c[0]"));
        cfield.set("Parent", Object::Reference(parent_id));
        cfield.set("Kids", Object::Array(vec![Object::Reference(c_widget_id)]));
        doc.set_object(c_field_id, cfield);
        let mut cwidget = Dictionary::new();
        cwidget.set("Type", Object::Name(b"Annot".to_vec()));
        cwidget.set("Subtype", Object::Name(b"Widget".to_vec()));
        cwidget.set(
            "Rect",
            Object::Array(vec![
                Object::Integer(320),
                Object::Integer(y),
                Object::Integer(340),
                Object::Integer(y + 20),
            ]),
        );
        cwidget.set("P", Object::Reference(page_id));
        cwidget.set("Parent", Object::Reference(c_field_id));
        cwidget.set("F", Object::Integer(4));
        let mut cap_n = Dictionary::new();
        cap_n.set("Off", Object::Reference(off_stream_id));
        cap_n.set("1", Object::Reference(on_stream_id));
        let mut cap = Dictionary::new();
        cap.set("N", Object::Dictionary(cap_n));
        cwidget.set("AP", Object::Dictionary(cap));
        cwidget.set("AS", Object::Name(b"Off".to_vec()));
        doc.set_object(c_widget_id, cwidget);
    }
    // Root container field.
    let mut root = Dictionary::new();
    root.set("T", Object::string_literal("form"));
    root.set(
        "Kids",
        Object::Array(vec![
            Object::Reference(text_id),
            Object::Reference(check_id),
            Object::Reference(dupa_parent_id),
            Object::Reference(dupb_parent_id),
        ]),
    );
    doc.set_object(root_id, root);

    let mut page = Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set("Parent", Object::Reference(pages_id));
    page.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    );
    page.set(
        "Annots",
        Object::Array(vec![
            Object::Reference(text_id),
            Object::Reference(check_id),
            Object::Reference(dupa_id),
            Object::Reference(dupb_id),
            Object::Reference(ca_widget_id),
            Object::Reference(cb_widget_id),
        ]),
    );
    let mut font_dict = Dictionary::new();
    font_dict.set("Helv", Object::Reference(font_id));
    let mut resources = Dictionary::new();
    resources.set("Font", Object::Dictionary(font_dict));
    page.set("Resources", Object::Dictionary(resources));
    doc.set_object(page_id, page);

    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
    pages.set("Count", Object::Integer(1));
    doc.set_object(pages_id, pages);

    let mut acro_font = Dictionary::new();
    acro_font.set("Helv", Object::Reference(font_id));
    let mut acro_dr = Dictionary::new();
    acro_dr.set("Font", Object::Dictionary(acro_font));
    let mut acro = Dictionary::new();
    acro.set("Fields", Object::Array(vec![Object::Reference(root_id)]));
    acro.set("DA", Object::string_literal("/Helv 0 Tf 0 g"));
    acro.set("DR", Object::Dictionary(acro_dr));

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    catalog.set("AcroForm", Object::Dictionary(acro));
    doc.set_object(catalog_id, catalog);
    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc.save(path).map_err(err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("opentax-tax-pdf-{}-{name}", std::process::id()))
    }

    fn utf16be(s: &str) -> Vec<u8> {
        let mut bytes = vec![0xFE, 0xFF];
        for u in s.encode_utf16() {
            bytes.extend_from_slice(&u.to_be_bytes());
        }
        bytes
    }

    #[test]
    fn decode_utf16be_bom_names() {
        // The exact shape of IRS Designer field names (spike regression).
        assert_eq!(
            decode_pdf_string(&utf16be("topmostSubform[0].Page1[0]")),
            "topmostSubform[0].Page1[0]"
        );
        assert_eq!(decode_pdf_string(&utf16be("f1_14")), "f1_14");
        assert!(!decode_pdf_string(&utf16be("f1_14")).contains('\u{fffd}'));
        assert!(!decode_pdf_string(&utf16be("f1_14")).contains('\u{feff}'));
    }

    #[test]
    fn decode_utf16le_bom() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "f1_16".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_pdf_string(&bytes), "f1_16");
    }

    #[test]
    fn decode_plain_ascii_and_pdfdoc_encoding() {
        assert_eq!(decode_pdf_string(b"plain name"), "plain name");
        // Latin-1 range byte: 0xE9 = é in both Latin-1 and PDFDocEncoding.
        assert_eq!(decode_pdf_string(&[0xE9]), "é");
        // PDFDocEncoding-only assignments: 0xA0 = Euro, 0x83 = ellipsis.
        assert_eq!(decode_pdf_string(&[0xA0]), "€");
        assert_eq!(decode_pdf_string(&[0x83]), "…");
    }

    #[test]
    fn inspect_decodes_utf16be_field_name() {
        // Regression for the spike's fatal bug #1: with the old lossy
        // decoding this name came back as "\u{feff}\0n\0a\0m\0e…".
        let src = tmp("form-inspect.pdf");
        make_test_form(&src).unwrap();
        let fields = inspect_fields(&src).unwrap();
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"form.name_field"), "names: {names:?}");
        assert!(names.contains(&"form.agree"), "names: {names:?}");
        assert!(names.contains(&"form.a.dup"), "names: {names:?}");
        assert!(names.contains(&"form.b.dup"), "names: {names:?}");
        assert!(
            names
                .iter()
                .all(|n| !n.contains('\u{fffd}') && !n.contains('\0')),
            "names must be clean: {names:?}"
        );
    }

    #[test]
    fn pdfer_text_fill_round_trip_with_appearance_and_audit() {
        let src = tmp("form-text-src.pdf");
        let out = tmp("form-text-filled.pdf");
        make_test_form(&src).unwrap();
        let values = HashMap::from([("form.name_field".to_string(), "Spike Tester".to_string())]);
        let report = PdferBackend.fill_fields(&src, &values, &out).unwrap();
        assert_eq!(report.backend, "pdfer_forms");
        assert_eq!(report.changed_fields, vec!["form.name_field".to_string()]);
        assert!(report.unintended_changes.is_empty());
        assert!(
            report.appearance_verified >= 1,
            "pdfer must regenerate /AP for the changed field: {report:?}"
        );
        assert!(report.warnings.iter().any(|w| w.contains("XFA")));
        let after = inspect_fields(&out).unwrap();
        let f = after.iter().find(|f| f.name == "form.name_field").unwrap();
        assert_eq!(f.value.as_deref(), Some("Spike Tester"));
    }

    #[test]
    fn pdfer_checkbox_fill_sets_name_value() {
        let src = tmp("form-check-src.pdf");
        let out = tmp("form-check-filled.pdf");
        make_test_form(&src).unwrap();
        // Caller passes the on-state name, with or without the slash.
        let values = HashMap::from([("form.agree".to_string(), "1".to_string())]);
        let report = PdferBackend.fill_fields(&src, &values, &out).unwrap();
        assert_eq!(report.changed_fields, vec!["form.agree".to_string()]);
        let after = inspect_fields(&out).unwrap();
        let f = after.iter().find(|f| f.name == "form.agree").unwrap();
        // /V must be the NAME "1" — not a string, not mangled (the old
        // lopdf path wrote the string "/1"; see PDF_SPIKE.md).
        assert_eq!(f.value.as_deref(), Some("1"), "field: {f:?}");
    }

    #[test]
    fn pdfer_duplicate_leaf_fill_fails_audit() {
        // pdfer_forms matches on leaf names as a fallback; filling form.a.dup
        // also sets form.b.dup. The post-fill audit must catch it and fail,
        // and the output file must not be produced.
        let src = tmp("form-dup-src.pdf");
        let out = tmp("form-dup-filled.pdf");
        let _ = std::fs::remove_file(&out);
        make_test_form(&src).unwrap();
        let values = HashMap::from([("form.a.dup".to_string(), "X".to_string())]);
        let err = PdferBackend
            .fill_fields(&src, &values, &out)
            .expect_err("duplicate-leaf over-match must fail the audit");
        assert!(
            err.0.contains("form.b.dup"),
            "audit error must name the unintended sibling: {err}"
        );
        assert!(!out.exists(), "no output may be kept on audit failure");
    }

    #[test]
    fn button_duplicate_leaf_checkbox_fills_only_the_requested_field() {
        // The case pdfer cannot do: two /Btn fields share leaf "c[0]"
        // (form.a.c[0] and form.b.c[0]). The direct button pass resolves
        // the exact qualified name, so filling one must succeed, leave
        // the sibling untouched, and pass the audit with exactly one
        // changed field.
        let src = tmp("form-btn-dup-src.pdf");
        let out = tmp("form-btn-dup-filled.pdf");
        let _ = std::fs::remove_file(&out);
        make_test_form(&src).unwrap();
        let values = HashMap::from([("form.a.c[0]".to_string(), "1".to_string())]);
        let report = PdferBackend
            .fill_fields(&src, &values, &out)
            .expect("button fill must not be blocked by the sibling's leaf");
        assert_eq!(report.backend, "pdfer_forms+lopdf-buttons");
        assert_eq!(report.changed_fields, vec!["form.a.c[0]".to_string()]);
        assert!(report.unintended_changes.is_empty());
        assert_eq!(
            report.appearance_verified, 1,
            "selected widget's /AP /N carries the on-state: {report:?}"
        );
        assert!(out.exists());
        let after = inspect_fields(&out).unwrap();
        let a = after.iter().find(|f| f.name == "form.a.c[0]").unwrap();
        // /V must read back as the NAME "1".
        assert_eq!(a.value.as_deref(), Some("1"), "field: {a:?}");
        let b = after.iter().find(|f| f.name == "form.b.c[0]").unwrap();
        assert_eq!(b.value, None, "sibling must be untouched: {b:?}");
        // The selected widget's /AS is "1"; the sibling's stays "Off".
        let doc = Document::load(&out).unwrap();
        let fields = collect_fields(&doc).unwrap();
        for (fname, want_as) in [("form.a.c[0]", "1"), ("form.b.c[0]", "Off")] {
            let f = fields.iter().find(|f| f.name == fname).unwrap();
            let widgets = button_widgets(&doc, f);
            assert_eq!(widgets.len(), 1);
            let wdict = doc.get_dictionary(widgets[0]).unwrap();
            let as_state = wdict
                .get(b"AS")
                .ok()
                .and_then(|o| o.as_name().ok())
                .map(decode_pdf_name);
            assert_eq!(as_state.as_deref(), Some(want_as), "field {fname}");
        }
    }

    #[test]
    fn button_uncheck_sets_value_and_as_to_off() {
        let src = tmp("form-btn-uncheck-src.pdf");
        let on = tmp("form-btn-uncheck-on.pdf");
        let out = tmp("form-btn-uncheck-off.pdf");
        make_test_form(&src).unwrap();
        // Pre-set the (merged field+widget) checkbox on.
        let values = HashMap::from([("form.agree".to_string(), "1".to_string())]);
        PdferBackend.fill_fields(&src, &values, &on).unwrap();
        let mid = inspect_fields(&on).unwrap();
        assert_eq!(
            mid.iter()
                .find(|f| f.name == "form.agree")
                .unwrap()
                .value
                .as_deref(),
            Some("1")
        );
        // Uncheck it.
        let values = HashMap::from([("form.agree".to_string(), "Off".to_string())]);
        let report = PdferBackend.fill_fields(&on, &values, &out).unwrap();
        assert_eq!(report.changed_fields, vec!["form.agree".to_string()]);
        let after = inspect_fields(&out).unwrap();
        let f = after.iter().find(|f| f.name == "form.agree").unwrap();
        assert_eq!(f.value.as_deref(), Some("Off"), "field: {f:?}");
        let doc = Document::load(&out).unwrap();
        let fields = collect_fields(&doc).unwrap();
        let facc = fields.iter().find(|f| f.name == "form.agree").unwrap();
        let dict = doc.get_dictionary(facc.id).unwrap();
        let as_state = dict
            .get(b"AS")
            .ok()
            .and_then(|o| o.as_name().ok())
            .map(decode_pdf_name);
        assert_eq!(as_state.as_deref(), Some("Off"));
    }

    #[test]
    fn button_bad_on_state_errors_and_writes_nothing() {
        let src = tmp("form-btn-bad-src.pdf");
        let out = tmp("form-btn-bad-filled.pdf");
        let _ = std::fs::remove_file(&out);
        make_test_form(&src).unwrap();
        let values = HashMap::from([("form.a.c[0]".to_string(), "9".to_string())]);
        let err = PdferBackend
            .fill_fields(&src, &values, &out)
            .expect_err("unknown on-state must be an error, never a guess");
        assert!(err.0.contains("form.a.c[0]"), "{err}");
        assert!(
            err.0.contains("available on-states: 1"),
            "error must name the available states: {err}"
        );
        assert!(!out.exists(), "no output may be kept on failure");
    }

    #[test]
    fn pdfer_unknown_field_is_an_error_before_filling() {
        let src = tmp("form-unknown-src.pdf");
        let out = tmp("form-unknown-filled.pdf");
        make_test_form(&src).unwrap();
        let values = HashMap::from([("form.nope".to_string(), "X".to_string())]);
        let err = PdferBackend.fill_fields(&src, &values, &out).unwrap_err();
        assert!(err.0.contains("unknown field"), "{err}");
        assert!(!out.exists());
    }

    #[test]
    fn lopdf_backend_report_is_honest_about_appearances() {
        let src = tmp("form-lopdf-src.pdf");
        let out = tmp("form-lopdf-filled.pdf");
        make_test_form(&src).unwrap();
        let values = HashMap::from([("form.name_field".to_string(), "Hi".to_string())]);
        let report = LopdfBackend.fill_fields(&src, &values, &out).unwrap();
        assert_eq!(report.backend, "lopdf");
        assert_eq!(report.appearance_verified, 0);
        assert!(report.warnings.iter().any(|w| w.contains("/V only")));
    }

    #[test]
    fn acroform_round_trip_fill_and_reread() {
        let src = tmp("fixture.pdf");
        let out = tmp("filled.pdf");
        make_test_pdf(&src, "taxpayer_name").unwrap();

        let fields = inspect_fields(&src).unwrap();
        assert_eq!(fields.len(), 1, "fields: {fields:?}");
        assert_eq!(fields[0].name, "taxpayer_name");
        assert_eq!(fields[0].field_type.as_deref(), Some("Tx"));
        assert_eq!(fields[0].value, None);

        let values = HashMap::from([("taxpayer_name".to_string(), "Ray Li".to_string())]);
        let filled = fill_fields(&src, &values, &out).unwrap();
        assert_eq!(filled, vec!["taxpayer_name".to_string()]);

        let after = inspect_fields(&out).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].value.as_deref(), Some("Ray Li"));

        let backend = LopdfBackend;
        assert_eq!(backend.name(), "lopdf");
        let via_trait = backend.inspect_fields(&out).unwrap();
        assert_eq!(via_trait[0].value.as_deref(), Some("Ray Li"));
    }
}
