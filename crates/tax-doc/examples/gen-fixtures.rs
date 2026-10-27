//! Synthetic born-digital fixture generator for the tax-doc extractor
//! tests. All names, SSNs, EINs and amounts are invented test data.
//!
//! Run:  cargo run -p tax-doc --example gen-fixtures -- <output-dir>
//! (default output dir: crates/tax-doc/tests/fixtures)
//!
//! The PDFs are drawn with lopdf content streams: base-14 Helvetica
//! (no /Widths array — the extractor must fall back to AFM metrics,
//! exactly like real issuer PDFs), one Tj per printed element, values
//! as separate absolutely-positioned strings — the way overlay/print
//! programs that produce W-2 and 1099 copies actually emit them.

use lopdf::{Dictionary, Document, Object, Stream};
use std::path::PathBuf;

const PAGE_H: f32 = 792.0;

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

fn num(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// A page under construction. Coordinates are top-down (like a layout
/// tool): `top` is the distance from the top edge to the text baseline.
struct Page {
    ops: String,
    annots: Vec<lopdf::ObjectId>,
}

impl Page {
    fn new() -> Page {
        Page {
            ops: String::new(),
            annots: Vec::new(),
        }
    }

    fn text(&mut self, x: f32, top: f32, font: &str, size: f32, s: &str) {
        self.ops.push_str(&format!(
            "BT /{font} {} Tf 1 0 0 1 {} {} Tm ({}) Tj ET\n",
            num(size),
            num(x),
            num(PAGE_H - top),
            esc(s)
        ));
    }

    /// A TJ line with kerning adjustments, as some generators emit.
    fn text_tj(&mut self, x: f32, top: f32, font: &str, size: f32, parts: &[(&str, i32)]) {
        let mut arr = String::from("[");
        for (i, (s, adj)) in parts.iter().enumerate() {
            if i > 0 {
                arr.push_str(&format!(" {} ", adj));
            }
            arr.push_str(&format!("({})", esc(s)));
        }
        arr.push(']');
        self.ops.push_str(&format!(
            "BT /{font} {} Tf 1 0 0 1 {} {} Tm {arr} TJ ET\n",
            num(size),
            num(x),
            num(PAGE_H - top)
        ));
    }

    fn rect(&mut self, x: f32, top: f32, w: f32, h: f32) {
        self.ops.push_str(&format!(
            "0 0 0 RG 0.7 w {} {} {} {} re S\n",
            num(x),
            num(PAGE_H - top - h),
            num(w),
            num(h)
        ));
    }

    fn filled_rect(&mut self, x: f32, top: f32, w: f32, h: f32) {
        self.ops.push_str(&format!(
            "0.85 g {} {} {} {} re f 0 g\n",
            num(x),
            num(PAGE_H - top - h),
            num(w),
            num(h)
        ));
    }
}

struct Field {
    name: &'static str,
    value: &'static str,
    x: f32,
    top: f32,
}

fn build_pdf(path: &PathBuf, pages: Vec<Page>) -> Result<(), String> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let catalog_id = doc.new_object_id();

    let mut font1 = Dictionary::new();
    font1.set("Type", Object::Name(b"Font".to_vec()));
    font1.set("Subtype", Object::Name(b"Type1".to_vec()));
    font1.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
    font1.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    let font1_id = doc.add_object(font1);
    let mut font2 = Dictionary::new();
    font2.set("Type", Object::Name(b"Font".to_vec()));
    font2.set("Subtype", Object::Name(b"Type1".to_vec()));
    font2.set("BaseFont", Object::Name(b"Helvetica-Bold".to_vec()));
    font2.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    let font2_id = doc.add_object(font2);

    let mut page_ids = Vec::new();
    for page in &pages {
        let content_id = doc.add_object(Stream::new(Dictionary::new(), page.ops.clone().into_bytes()));
        let page_id = doc.new_object_id();
        let mut fonts = Dictionary::new();
        fonts.set("F1", Object::Reference(font1_id));
        fonts.set("F2", Object::Reference(font2_id));
        let mut resources = Dictionary::new();
        resources.set("Font", Object::Dictionary(fonts));
        let mut p = Dictionary::new();
        p.set("Type", Object::Name(b"Page".to_vec()));
        p.set("Parent", Object::Reference(pages_id));
        p.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
        );
        p.set("Resources", Object::Dictionary(resources));
        p.set("Contents", Object::Reference(content_id));
        if !page.annots.is_empty() {
            p.set(
                "Annots",
                Object::Array(
                    page.annots
                        .iter()
                        .map(|id| Object::Reference(*id))
                        .collect(),
                ),
            );
        }
        doc.set_object(page_id, p);
        page_ids.push(page_id);
    }

    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set(
        "Kids",
        Object::Array(page_ids.iter().map(|id| Object::Reference(*id)).collect()),
    );
    pages.set("Count", Object::Integer(page_ids.len() as i64));
    doc.set_object(pages_id, pages);

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    doc.set_object(catalog_id, catalog);
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc.save(path).map(|_| ()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// W-2 grid layout (IRS-style boxes). `origin` is the top of one copy;
// `blank` lists box numbers whose value cell stays empty.
// ---------------------------------------------------------------------------

fn w2_grid_copy(page: &mut Page, origin: f32, blank: &[&str]) {
    page.text(36.0, origin + 18.0, "F2", 13.0, "Form W-2 Wage and Tax Statement 2025");
    page.text(
        36.0,
        origin + 34.0,
        "F1",
        8.0,
        "Copy B - To Be Filed With Employee's FEDERAL Tax Return",
    );
    // Boxes a / b
    page.text(36.0, origin + 56.0, "F1", 7.5, "a Employee's social security number");
    page.text(330.0, origin + 56.0, "F1", 7.5, "b Employer identification number (EIN)");
    page.text(36.0, origin + 74.0, "F1", 10.0, "123-45-6789");
    page.text(330.0, origin + 74.0, "F1", 10.0, "12-3456789");
    // Boxes c / e
    page.text(36.0, origin + 96.0, "F1", 7.5, "c Employer's name, address, and ZIP code");
    page.text(330.0, origin + 96.0, "F1", 7.5, "e Employee's name, address, and ZIP code");
    page.text(36.0, origin + 114.0, "F2", 10.0, "Acme Widgets Inc");
    page.text(36.0, origin + 128.0, "F1", 9.0, "1200 Industrial Pkwy");
    page.text(36.0, origin + 142.0, "F1", 9.0, "Springfield, IL 62704");
    page.text(330.0, origin + 114.0, "F1", 10.0, "Spike Tester");
    page.text(330.0, origin + 128.0, "F1", 9.0, "44 Elm Street");
    page.text(330.0, origin + 142.0, "F1", 9.0, "Springfield, IL 62704");
    // Grid boxes 1-6: (col x, box no, label, value)
    let boxes: [(&str, &str, &str); 6] = [
        ("1", "Wages, tips, other compensation", "52,340.75"),
        ("2", "Federal income tax withheld", "4,825.50"),
        ("3", "Social security wages", "55,000.00"),
        ("4", "Social security tax withheld", "3,410.00"),
        ("5", "Medicare wages and tips", "55,000.00"),
        ("6", "Medicare tax withheld", "797.50"),
    ];
    for (i, (bno, label, value)) in boxes.iter().enumerate() {
        let col_x = if i % 2 == 0 { 36.0 } else { 330.0 };
        let col_w = if i % 2 == 0 { 270.0 } else { 246.0 };
        let row_top = origin + 156.0 + (i / 2) as f32 * 46.0;
        page.rect(col_x, row_top, col_w, 40.0);
        page.text(col_x + 6.0, row_top + 13.0, "F1", 8.0, bno);
        page.text(col_x + 20.0, row_top + 13.0, "F1", 8.0, label);
        if !blank.contains(bno) {
            page.text(col_x + 20.0, row_top + 32.0, "F1", 12.0, value);
        }
    }
}

fn w2_adp() -> Page {
    let mut page = Page::new();
    page.text(48.0, 40.0, "F2", 14.0, "Form W-2 Wage and Tax Statement");
    page.text(48.0, 58.0, "F1", 9.0, "2025 - Globex Corporation Payroll Services");
    page.text(48.0, 88.0, "F1", 8.0, "Employer's name, address, and ZIP code");
    page.text(48.0, 106.0, "F2", 10.5, "Globex Corporation");
    page.text(48.0, 120.0, "F1", 9.0, "9 Market Street");
    page.text(48.0, 134.0, "F1", 9.0, "San Francisco, CA 94103");
    page.text(48.0, 160.0, "F1", 9.0, "Employer identification number (EIN)");
    page.text(330.0, 160.0, "F1", 10.0, "47-2091853");
    page.text(48.0, 186.0, "F1", 9.0, "Employee's social security number:");
    page.text(280.0, 186.0, "F1", 10.0, "123-45-6789");
    let rows: [(&str, &str); 6] = [
        ("1 Wages, tips, other compensation", "98,765.43"),
        ("2 Federal income tax withheld", "15,210.99"),
        ("3 Social security wages", "98,765.43"),
        ("4 Social security tax withheld", "6,123.46"),
        ("5 Medicare wages and tips", "98,765.43"),
        ("6 Medicare tax withheld", "1,432.10"),
    ];
    for (i, (label, value)) in rows.iter().enumerate() {
        let top = 226.0 + i as f32 * 28.0;
        page.text(48.0, top, "F1", 9.5, label);
        page.text(430.0, top, "F1", 11.0, value);
    }
    page.text_tj(
        48.0,
        420.0,
        "F1",
        9.0,
        &[
            ("This infor", -25),
            ("mation is being furnished ", 15),
            ("to the Internal Revenue Service.", 0),
        ],
    );
    page
}

fn payer_block(page: &mut Page) {
    page.text(36.0, 64.0, "F1", 7.5, "PAYER'S name, street address, city or town,");
    page.text(
        36.0,
        76.0,
        "F1",
        7.5,
        "state or province, country, ZIP or foreign postal code, and telephone no.",
    );
}

fn recipient_block(page: &mut Page, name: &str) {
    page.text(36.0, 170.0, "F1", 7.5, "RECIPIENT'S name");
    page.text(36.0, 188.0, "F1", 10.0, name);
    page.text(36.0, 208.0, "F1", 7.5, "RECIPIENT'S TIN");
    page.text(36.0, 224.0, "F1", 10.0, "123-45-6789");
}

fn box_row(page: &mut Page, top: f32, label: &str, value: Option<&str>) {
    page.rect(330.0, top - 4.0, 246.0, 34.0);
    page.text(338.0, top + 10.0, "F1", 8.0, label);
    if let Some(v) = value {
        page.text(338.0, top + 27.0, "F1", 11.0, v);
    }
}

fn int_fixture() -> Page {
    let mut page = Page::new();
    page.text(36.0, 30.0, "F2", 13.0, "Form 1099-INT Interest Income 2025");
    page.text(36.0, 46.0, "F1", 8.0, "Copy B - For Recipient");
    payer_block(&mut page);
    page.text(36.0, 96.0, "F2", 10.5, "First Test Bank");
    page.text(36.0, 110.0, "F1", 9.0, "400 Bank Plaza");
    page.text(36.0, 124.0, "F1", 9.0, "Denver, CO 80202");
    page.text(36.0, 144.0, "F1", 8.0, "PAYER'S TIN");
    page.text(120.0, 144.0, "F1", 9.0, "84-0192837");
    recipient_block(&mut page, "Spike Tester");
    box_row(&mut page, 64.0, "1 Interest income", Some("1,234.56"));
    box_row(&mut page, 112.0, "2 Early withdrawal penalty", None);
    box_row(&mut page, 160.0, "4 Federal income tax withheld", None);
    box_row(&mut page, 208.0, "8 Tax-exempt interest", Some("250.00"));
    page
}

fn div_fixture() -> Page {
    let mut page = Page::new();
    page.text(36.0, 30.0, "F2", 13.0, "Form 1099-DIV Dividends and Distributions 2025");
    page.text(36.0, 46.0, "F1", 8.0, "Copy B - For Recipient");
    payer_block(&mut page);
    page.text(36.0, 96.0, "F2", 10.5, "Test Brokerage LLC");
    page.text(36.0, 110.0, "F1", 9.0, "77 Wall Street");
    page.text(36.0, 124.0, "F1", 9.0, "New York, NY 10005");
    page.text(36.0, 144.0, "F1", 8.0, "PAYER'S TIN");
    page.text(120.0, 144.0, "F1", 9.0, "47-8812093");
    recipient_block(&mut page, "Spike Tester");
    box_row(&mut page, 64.0, "1a Total ordinary dividends", Some("2,500.00"));
    box_row(&mut page, 112.0, "1b Qualified dividends", Some("2,100.25"));
    box_row(&mut page, 160.0, "4 Federal income tax withheld", Some("125.00"));
    page
}

fn nec_fixture() -> Page {
    let mut page = Page::new();
    page.text(36.0, 30.0, "F2", 13.0, "Form 1099-NEC Nonemployee Compensation 2025");
    page.text(36.0, 46.0, "F1", 8.0, "Copy B - For Recipient");
    payer_block(&mut page);
    page.text(36.0, 96.0, "F2", 10.5, "Client Co LLC");
    page.text(36.0, 110.0, "F1", 9.0, "12 Harbor Way");
    page.text(36.0, 124.0, "F1", 9.0, "Seattle, WA 98101");
    page.text(36.0, 144.0, "F1", 8.0, "PAYER'S TIN");
    page.text(120.0, 144.0, "F1", 9.0, "83-2910475");
    recipient_block(&mut page, "Spike Tester");
    box_row(&mut page, 64.0, "1 Nonemployee compensation", Some("18,750.00"));
    box_row(&mut page, 112.0, "4 Federal income tax withheld", None);
    page
}

fn main() -> Result<(), String> {
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("crates/tax-doc/tests/fixtures"));
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let at = |name: &str| out_dir.join(name);

    // 1. W-2, IRS grid layout, two identical copies on one page.
    {
        let mut page = Page::new();
        w2_grid_copy(&mut page, 6.0, &[]);
        w2_grid_copy(&mut page, 396.0, &[]);
        build_pdf(&at("w2-grid.pdf"), vec![page])?;
    }
    // 2. W-2, payroll-provider layout (label left, value right).
    build_pdf(&at("w2-adp.pdf"), vec![w2_adp()])?;
    // 3. W-2 grid, Box 2 and Box 6 values blank -> partial extraction.
    {
        let mut page = Page::new();
        w2_grid_copy(&mut page, 6.0, &["2", "6"]);
        build_pdf(&at("w2-partial.pdf"), vec![page])?;
    }
    // 4-6. 1099-INT / DIV / NEC.
    build_pdf(&at("int.pdf"), vec![int_fixture()])?;
    build_pdf(&at("div.pdf"), vec![div_fixture()])?;
    build_pdf(&at("nec.pdf"), vec![nec_fixture()])?;
    // 7. Image-only page (no text ops at all) -> needs_ocr.
    {
        let mut page = Page::new();
        page.filled_rect(100.0, 200.0, 412.0, 200.0);
        page.rect(36.0, 36.0, 540.0, 720.0);
        build_pdf(&at("image-only.pdf"), vec![page])?;
    }
    // 8. Plain non-tax text -> unrecognized.
    {
        let mut page = Page::new();
        page.text(36.0, 40.0, "F2", 13.0, "CORNER GROCERY STORE");
        page.text(36.0, 70.0, "F1", 10.0, "Milk 3.50");
        page.text(36.0, 88.0, "F1", 10.0, "Bread 2.25");
        page.text(36.0, 106.0, "F1", 10.0, "Total 5.75");
        page.text(36.0, 140.0, "F1", 10.0, "Thank you, come again!");
        build_pdf(&at("receipt.pdf"), vec![page])?;
    }
    // 9. A 1040 title page: recognized, but not an extractable kind.
    {
        let mut page = Page::new();
        page.text(36.0, 40.0, "F2", 16.0, "Form 1040");
        page.text(36.0, 62.0, "F1", 11.0, "U.S. Individual Income Tax Return");
        page.text(36.0, 84.0, "F1", 10.0, "2025");
        page.text(36.0, 120.0, "F1", 9.0, "Your first name and middle initial");
        build_pdf(&at("f1040-title.pdf"), vec![page])?;
    }
    // 10. W-2 as an AcroForm (fillable-copy issuer style).
    {
        let mut page = Page::new();
        page.text(36.0, 36.0, "F2", 13.0, "Form W-2 Wage and Tax Statement 2025");
        page.text(36.0, 58.0, "F1", 9.0, "Fillable copy issued by Acme Payroll (values in form fields)");
        let fields: Vec<Field> = vec![
            Field { name: "EmployerEIN", value: "12-3456789", x: 36.0, top: 90.0 },
            Field { name: "EmployerName", value: "Acme Widgets Inc", x: 300.0, top: 90.0 },
            Field { name: "WagesBox1", value: "52,340.75", x: 36.0, top: 130.0 },
            Field { name: "FederalBox2", value: "4,825.50", x: 300.0, top: 130.0 },
            Field { name: "SSWagesBox3", value: "55,000.00", x: 36.0, top: 170.0 },
            Field { name: "SSTaxBox4", value: "3,410.00", x: 300.0, top: 170.0 },
            Field { name: "MedWagesBox5", value: "55,000.00", x: 36.0, top: 210.0 },
            Field { name: "MedTaxBox6", value: "797.50", x: 300.0, top: 210.0 },
        ];
        build_pdf_acro(&at("w2-acroform.pdf"), page, &fields)?;
    }
    println!("fixtures written to {}", out_dir.display());
    Ok(())
}

/// A PDF whose values live in AcroForm text fields (merged
/// field+widget annotations), like issuer fillable copies.
fn build_pdf_acro(path: &PathBuf, mut page: Page, fields: &[Field]) -> Result<(), String> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let catalog_id = doc.new_object_id();
    let page_id = doc.new_object_id();

    let mut font1 = Dictionary::new();
    font1.set("Type", Object::Name(b"Font".to_vec()));
    font1.set("Subtype", Object::Name(b"Type1".to_vec()));
    font1.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
    let font1_id = doc.add_object(font1);
    let mut font2 = Dictionary::new();
    font2.set("Type", Object::Name(b"Font".to_vec()));
    font2.set("Subtype", Object::Name(b"Type1".to_vec()));
    font2.set("BaseFont", Object::Name(b"Helvetica-Bold".to_vec()));
    let font2_id = doc.add_object(font2);

    let mut field_ids = Vec::new();
    for f in fields {
        let fid = doc.new_object_id();
        let mut d = Dictionary::new();
        d.set("Type", Object::Name(b"Annot".to_vec()));
        d.set("Subtype", Object::Name(b"Widget".to_vec()));
        d.set("FT", Object::Name(b"Tx".to_vec()));
        d.set("T", Object::string_literal(f.name.to_string()));
        d.set("V", Object::string_literal(f.value.to_string()));
        d.set(
            "Rect",
            Object::Array(vec![
                Object::Real(f.x),
                Object::Real(PAGE_H - f.top - 16.0),
                Object::Real(f.x + 200.0),
                Object::Real(PAGE_H - f.top),
            ]),
        );
        d.set("P", Object::Reference(page_id));
        d.set("F", Object::Integer(4));
        doc.set_object(fid, d);
        field_ids.push(fid);
        page.annots.push(fid);
    }

    let content_id = doc.add_object(Stream::new(Dictionary::new(), page.ops.into_bytes()));
    let mut fonts = Dictionary::new();
    fonts.set("F1", Object::Reference(font1_id));
    fonts.set("F2", Object::Reference(font2_id));
    let mut resources = Dictionary::new();
    resources.set("Font", Object::Dictionary(fonts));
    let mut p = Dictionary::new();
    p.set("Type", Object::Name(b"Page".to_vec()));
    p.set("Parent", Object::Reference(pages_id));
    p.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    );
    p.set("Resources", Object::Dictionary(resources));
    p.set("Contents", Object::Reference(content_id));
    p.set(
        "Annots",
        Object::Array(field_ids.iter().map(|id| Object::Reference(*id)).collect()),
    );
    doc.set_object(page_id, p);

    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
    pages.set("Count", Object::Integer(1));
    doc.set_object(pages_id, pages);

    let mut acro = Dictionary::new();
    acro.set(
        "Fields",
        Object::Array(field_ids.iter().map(|id| Object::Reference(*id)).collect()),
    );
    acro.set("DA", Object::string_literal("/Helv 0 Tf 0 g"));
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    catalog.set("AcroForm", Object::Dictionary(acro));
    doc.set_object(catalog_id, catalog);
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc.save(path).map(|_| ()).map_err(|e| e.to_string())
}
