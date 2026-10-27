//! Throwaway debug dump: print the extraction report + words for a PDF.
use tax_core::TaxParams;
use tax_doc::{extract_document, extract_words};

fn main() {
    let path = std::env::args().nth(1).expect("usage: dbg-extract <pdf>");
    let p = std::path::Path::new(&path);
    if std::env::var("WORDS").is_ok() {
        for w in extract_words(p).unwrap() {
            println!(
                "p{} base={:.1} x=[{:.1},{:.1}] {:?}",
                w.page, w.baseline, w.x0, w.x1, w.text
            );
        }
        return;
    }
    let params = TaxParams::load_2025().unwrap();
    let report = extract_document(p, &params).unwrap();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
