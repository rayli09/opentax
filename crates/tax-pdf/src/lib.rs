//! tax-pdf: AcroForm inspection and filling on `lopdf` (pure Rust, MIT).
//!
//! KNOWN WEAK LINK: [`fill_fields`] sets the field value `/V` but does not
//! regenerate widget appearance streams (`/AP`), so filled values may not
//! render in every viewer. This is the subject of the planned fill spike
//! (see docs/ARCHITECTURE.md); a PDFium backend is planned behind the
//! [`PdfBackend`] trait. Verify filled output independently — never trust
//! the filling library to grade its own work.

use lopdf::{Dictionary, Document, Object, ObjectId};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

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

/// A PDF backend. lopdf (pure Rust) is the default; a PDFium backend can be
/// added behind this trait without changing callers.
pub trait PdfBackend {
    fn name(&self) -> &'static str;
    fn inspect_fields(&self, path: &Path) -> PdfResult<Vec<FieldInfo>>;
    fn fill_fields(
        &self,
        input: &Path,
        values: &HashMap<String, String>,
        output: &Path,
    ) -> PdfResult<Vec<String>>;
    fn extract_text(&self, path: &Path) -> PdfResult<String>;
}

/// Default backend: pure-Rust lopdf.
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
    ) -> PdfResult<Vec<String>> {
        fill_fields(input, values, output)
    }
    fn extract_text(&self, path: &Path) -> PdfResult<String> {
        extract_text(path)
    }
}

// ---------------------------------------------------------------------------
// Field tree walking
// ---------------------------------------------------------------------------

struct FieldAcc {
    name: String,
    field_type: Option<String>,
    value: Option<String>,
    id: ObjectId,
}

/// Collect fields from a Fields/Kids array, joining partial names.
fn walk_fields(
    doc: &Document,
    refs: &[Object],
    parent_name: &str,
    inherited_ft: Option<String>,
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
            .map(|s| String::from_utf8_lossy(s).into_owned());
        let name = match (&partial, parent_name.is_empty()) {
            (Some(p), false) => format!("{parent_name}.{p}"),
            (Some(p), true) => p.clone(),
            (None, _) => parent_name.to_string(),
        };
        let ft = dict
            .get(b"FT")
            .ok()
            .and_then(|o| o.as_name().ok())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .or_else(|| inherited_ft.clone());
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
                id,
            });
        }
        if !kids.is_empty() && !kid_is_widget_only {
            walk_fields(doc, &kids, &name, ft, out);
        }
    }
}

fn object_to_string(o: &Object) -> Option<String> {
    match o {
        Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
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
    walk_fields(doc, &refs, "", None, &mut out);
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("opentax-tax-pdf-{}-{name}", std::process::id()))
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
