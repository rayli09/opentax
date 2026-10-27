//! Integration test: the tax-forms fill-values builder feeding the
//! tax-pdf PdferBackend against a synthetic PDF that carries a few of the
//! REAL TY2025 1040 qualified field names (same names as the field map).

use std::collections::HashMap;
use std::path::PathBuf;

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use tax_core::{FilingStatus, Money, TaxParams};
use tax_forms::{
    build_fill_values, compute_1040, BrokerTxn, Form1040Input, I1099BInput, I1099DivInput,
    I1099IntInput, TxnTerm, W2Input,
};
use tax_pdf::{LopdfBackend, PdfBackend, PdferBackend};

const Q_2B: &str = "topmostSubform[0].Page1[0].f1_59[0]";
const Q_11A: &str = "topmostSubform[0].Page1[0].f1_75[0]";
const Q_SINGLE: &str = "topmostSubform[0].Page1[0].Checkbox_ReadOrder[0].c1_8[0]";

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("opentax-pdf-export-{}-{name}", std::process::id()))
}

/// Build a one-page form: root `topmostSubform[0]` -> `Page1[0]` -> two
/// merged text field+widgets (f1_59[0], f1_75[0]) and a
/// `Checkbox_ReadOrder[0]` container holding a Btn field `c1_8[0]` with a
/// widget kid whose /AP /N has Off and 1 states (the real form's shape).
fn make_synthetic(path: &PathBuf) {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let page_id = doc.new_object_id();
    let catalog_id = doc.new_object_id();
    let font_id = doc.new_object_id();
    let root_id = doc.new_object_id();
    let page1_id = doc.new_object_id();
    let f59_id = doc.new_object_id();
    let f75_id = doc.new_object_id();
    let ro_id = doc.new_object_id();
    let cb_field_id = doc.new_object_id();
    let cb_widget_id = doc.new_object_id();
    let off_id = doc.new_object_id();
    let on_id = doc.new_object_id();

    let mut font = Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"Type1".to_vec()));
    font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
    font.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    doc.set_object(font_id, font);

    let text_field = |doc: &mut Document, id: ObjectId, t: &str, y: i64| {
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"Annot".to_vec()));
        d.set("Subtype", Object::Name(b"Widget".to_vec()));
        d.set("FT", Object::Name(b"Tx".to_vec()));
        d.set("T", Object::string_literal(t));
        d.set(
            "Rect",
            Object::Array(vec![
                Object::Integer(400),
                Object::Integer(y),
                Object::Integer(576),
                Object::Integer(y + 12),
            ]),
        );
        d.set("P", Object::Reference(page_id));
        d.set("Parent", Object::Reference(page1_id));
        d.set("F", Object::Integer(4));
        doc.set_object(id, d);
    };
    text_field(&mut doc, f59_id, "f1_59[0]", 500);
    text_field(&mut doc, f75_id, "f1_75[0]", 480);

    for sid in [off_id, on_id] {
        let mut sdict = Dictionary::new();
        sdict.set("Type", Object::Name(b"XObject".to_vec()));
        sdict.set("Subtype", Object::Name(b"Form".to_vec()));
        sdict.set(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(8),
                Object::Integer(8),
            ]),
        );
        doc.set_object(sid, Object::Stream(Stream::new(sdict, b"q\nQ\n".to_vec())));
    }
    let mut cb_field = Dictionary::new();
    cb_field.set("FT", Object::Name(b"Btn".to_vec()));
    cb_field.set("T", Object::string_literal("c1_8[0]"));
    cb_field.set("Parent", Object::Reference(ro_id));
    cb_field.set("Kids", Object::Array(vec![Object::Reference(cb_widget_id)]));
    doc.set_object(cb_field_id, cb_field);
    let mut cb_widget = Dictionary::new();
    cb_widget.set("Type", Object::Name(b"Annot".to_vec()));
    cb_widget.set("Subtype", Object::Name(b"Widget".to_vec()));
    cb_widget.set(
        "Rect",
        Object::Array(vec![
            Object::Integer(97),
            Object::Integer(578),
            Object::Integer(105),
            Object::Integer(586),
        ]),
    );
    cb_widget.set("P", Object::Reference(page_id));
    cb_widget.set("Parent", Object::Reference(cb_field_id));
    cb_widget.set("F", Object::Integer(4));
    let mut ap_n = Dictionary::new();
    ap_n.set("Off", Object::Reference(off_id));
    ap_n.set("1", Object::Reference(on_id));
    let mut ap = Dictionary::new();
    ap.set("N", Object::Dictionary(ap_n));
    cb_widget.set("AP", Object::Dictionary(ap));
    cb_widget.set("AS", Object::Name(b"Off".to_vec()));
    doc.set_object(cb_widget_id, cb_widget);

    let mut ro = Dictionary::new();
    ro.set("T", Object::string_literal("Checkbox_ReadOrder[0]"));
    ro.set("Parent", Object::Reference(page1_id));
    ro.set("Kids", Object::Array(vec![Object::Reference(cb_field_id)]));
    doc.set_object(ro_id, ro);

    let mut page1 = Dictionary::new();
    page1.set("T", Object::string_literal("Page1[0]"));
    page1.set("Parent", Object::Reference(root_id));
    page1.set(
        "Kids",
        Object::Array(vec![
            Object::Reference(f59_id),
            Object::Reference(f75_id),
            Object::Reference(ro_id),
        ]),
    );
    doc.set_object(page1_id, page1);

    let mut root = Dictionary::new();
    root.set("T", Object::string_literal("topmostSubform[0]"));
    root.set("Kids", Object::Array(vec![Object::Reference(page1_id)]));
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
            Object::Reference(f59_id),
            Object::Reference(f75_id),
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
    doc.save(path).unwrap();
}

/// Golden case 002 inputs (benchmark/cases/f1040/2025/002-w2-1099s).
fn case002_input() -> Form1040Input {
    Form1040Input {
        status: FilingStatus::Single,
        w2s: vec![W2Input {
            employer_ein: "12-3456789".into(),
            wages: Money::from_cents(5_000_000),
            federal_withholding: Money::from_cents(400_000),
            social_security_wages: None,
            social_security_tax: None,
            medicare_wages: None,
            medicare_tax: None,
        }],
        interest_income: Money::ZERO,
        ordinary_dividends: Money::ZERO,
        f1099_ints: vec![I1099IntInput {
            payer_name: "Bank".into(),
            payer_ein: None,
            interest_income: Money::from_cents(50_000),
            tax_exempt_interest: Money::ZERO,
            federal_withholding: Money::ZERO,
        }],
        f1099_divs: vec![I1099DivInput {
            payer_name: "Broker".into(),
            payer_ein: None,
            total_ordinary_dividends: Money::from_cents(100_000),
            qualified_dividends: Money::from_cents(60_000),
            federal_withholding: Money::ZERO,
        }],
        f1099_bs: vec![I1099BInput {
            payer_name: "Broker".into(),
            payer_ein: None,
            transactions: vec![
                BrokerTxn {
                    description: "ST".into(),
                    proceeds: Some(Money::from_cents(800_000)),
                    cost_basis: Some(Money::from_cents(600_000)),
                    adjustment: Money::ZERO,
                    term: TxnTerm::Short,
                },
                BrokerTxn {
                    description: "LT".into(),
                    proceeds: Some(Money::from_cents(300_000)),
                    cost_basis: Some(Money::from_cents(350_000)),
                    adjustment: Money::ZERO,
                    term: TxnTerm::Long,
                },
            ],
            federal_withholding: Money::ZERO,
        }],
    }
}

#[test]
fn builder_plus_backend_fill_synthetic_with_real_qnames() {
    let params = TaxParams::load_2025().unwrap();
    let computed = compute_1040(&case002_input(), &params);
    assert!(computed.complete);
    let plan = build_fill_values(&computed.lines, FilingStatus::Single).unwrap();

    // The synthetic carries only three of the mapped fields; fill just
    // those (the backend audit would rightly refuse unknown fields).
    let wanted: HashMap<String, String> = plan
        .values
        .iter()
        .filter(|v| [Q_2B, Q_11A, Q_SINGLE].contains(&v.field.as_str()))
        .map(|v| (v.field.clone(), v.value.clone()))
        .collect();
    assert_eq!(wanted.len(), 3);
    assert_eq!(wanted[Q_2B], "500.00");
    assert_eq!(wanted[Q_11A], "53000.00");
    assert_eq!(wanted[Q_SINGLE], "1");

    let input = tmp("synthetic.pdf");
    let output = tmp("synthetic-filled.pdf");
    make_synthetic(&input);
    let report = PdferBackend
        .fill_fields(&input, &wanted, &output)
        .expect("fill must succeed");
    assert!(report.unintended_changes.is_empty());
    assert_eq!(report.changed_fields.len(), 3);

    let fields = LopdfBackend.inspect_fields(&output).unwrap();
    let get = |q: &str| {
        fields
            .iter()
            .find(|f| f.name == q)
            .and_then(|f| f.value.clone())
    };
    assert_eq!(get(Q_2B).as_deref(), Some("500.00"));
    assert_eq!(get(Q_11A).as_deref(), Some("53000.00"));
    assert_eq!(get(Q_SINGLE).as_deref(), Some("1"));
}
