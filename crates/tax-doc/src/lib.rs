//! tax-doc: document ingest skeleton.
//!
//! Pipeline position (docs/ARCHITECTURE.md): classification and
//! deterministic validation live here; OCR is a pluggable fallback trait,
//! not v1 core; text extraction goes through tax-pdf's lopdf backend
//! (crude, position-free — documented limit).

use serde::Serialize;
use std::path::Path;
use tax_core::{Money, TaxParams};
use tax_forms::W2Input;

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocKind {
    W2,
    I1099Int,
    I1099Div,
    I1099B,
    I1099Nec,
    I1099R,
    Form1040,
}

impl DocKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DocKind::W2 => "w2",
            DocKind::I1099Int => "1099-int",
            DocKind::I1099Div => "1099-div",
            DocKind::I1099B => "1099-b",
            DocKind::I1099Nec => "1099-nec",
            DocKind::I1099R => "1099-r",
            DocKind::Form1040 => "f1040",
        }
    }
}

/// Deterministic title matching: tax forms print their own titles, so a
/// text-layer document classifies without ML. Returns `None` when no known
/// title is present (the caller routes to agent vision, never guesses).
pub fn classify_text(text: &str) -> Option<DocKind> {
    let t = text;
    // Most specific titles first (1099-B before generic 1040, etc.).
    if t.contains("1099-B") || t.contains("Proceeds From Broker") {
        return Some(DocKind::I1099B);
    }
    if t.contains("1099-INT") || t.contains("Interest Income") && t.contains("1099") {
        return Some(DocKind::I1099Int);
    }
    if t.contains("1099-DIV") || t.contains("Dividends and Distributions") {
        return Some(DocKind::I1099Div);
    }
    if t.contains("1099-NEC") || t.contains("Nonemployee Compensation") {
        return Some(DocKind::I1099Nec);
    }
    if t.contains("1099-R") || t.contains("Distributions From Pensions") {
        return Some(DocKind::I1099R);
    }
    if t.contains("Form W-2") || t.contains("Wage and Tax Statement") {
        return Some(DocKind::W2);
    }
    if t.contains("Form 1040") || t.contains("U.S. Individual Income Tax Return") {
        return Some(DocKind::Form1040);
    }
    None
}

// ---------------------------------------------------------------------------
// Text extraction (trait + lopdf-backed impl via tax-pdf)
// ---------------------------------------------------------------------------

pub trait TextExtractor {
    fn extract_text(&self, path: &Path) -> Result<String, String>;
}

/// Text extraction via tax-pdf's lopdf backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct LopdfTextExtractor;

impl TextExtractor for LopdfTextExtractor {
    fn extract_text(&self, path: &Path) -> Result<String, String> {
        tax_pdf::extract_text(path).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// OCR (pluggable fallback — NOT v1 core)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OcrWord {
    pub text: String,
    /// Bounding box in page points: [x0, y0, x1, y1].
    pub bbox: [f32; 4],
    /// Backend-reported confidence in [0, 1]. Not calibrated across backends.
    pub confidence: f32,
}

pub trait OcrBackend {
    fn name(&self) -> &'static str;
    fn available(&self) -> bool;
    fn ocr_page_image(&self, image_png: &[u8]) -> Result<Vec<OcrWord>, String>;
}

/// The v1 OCR backend: reports unavailable honestly. Real backends
/// (ocrs pure-Rust, external Tesseract/PaddleOCR-ONNX) implement this trait.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOcr;

impl OcrBackend for NoOcr {
    fn name(&self) -> &'static str {
        "none"
    }
    fn available(&self) -> bool {
        false
    }
    fn ocr_page_image(&self, _image_png: &[u8]) -> Result<Vec<OcrWord>, String> {
        Err(
            "no OCR backend is installed in v1; use the agent-vision path for scanned documents"
                .to_string(),
        )
    }
}

// ---------------------------------------------------------------------------
// Deterministic W-2 validation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub code: String,
    /// "error" | "warning" | "info"
    pub severity: String,
    pub message: String,
    pub field: Option<String>,
}

fn finding(code: &str, severity: &str, message: String, field: Option<&str>) -> Finding {
    Finding {
        code: code.to_string(),
        severity: severity.to_string(),
        message,
        field: field.map(str::to_string),
    }
}

/// Tolerance for rate checks: $2.00 — covers per-pay-period rounding.
const TOLERANCE_CENTS: i64 = 200;

fn rate_check(
    findings: &mut Vec<Finding>,
    code: &str,
    label: &str,
    tax: Option<Money>,
    wages: Option<Money>,
    rate_bp: u32,
    tax_field: &str,
) {
    match (tax, wages) {
        (Some(tax), Some(wages)) => {
            let expected = wages.mul_bp(rate_bp);
            let diff = (tax.cents() - expected.cents()).abs();
            if diff > TOLERANCE_CENTS {
                findings.push(finding(
                    code,
                    "warning",
                    format!(
                        "{label}: withheld {tax} differs from expected {expected} ({rate_bp} bp of wages {wages}) by more than tolerance"
                    ),
                    Some(tax_field),
                ));
            }
        }
        _ => findings.push(finding(
            &format!("{code}_skipped"),
            "info",
            format!("{label}: check skipped — required boxes were not provided"),
            Some(tax_field),
        )),
    }
}

/// Cross-check a W-2's Social Security and Medicare boxes against the
/// statutory rates. Pure and deterministic; never mutates the input and
/// never "fixes" a number — it only reports findings.
///
/// Rates: Social Security 6.2% (620 bp) of SS wages up to the wage base;
/// Medicare 1.45% (145 bp) of Medicare wages (the 0.9% Additional Medicare
/// Tax above the withholding threshold is not modeled in v1 — a mismatch in
/// that range is a warning to review, not an error).
pub fn validate_w2(w2: &W2Input, params: &TaxParams) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Social Security: rate applies only up to the wage base.
    let ss_wages_capped = w2
        .social_security_wages
        .map(|w| Money::from_cents(w.cents().min(params.social_security_wage_base_cents)));
    rate_check(
        &mut findings,
        "w2_ss_rate",
        "Social Security tax (Box 4 vs Box 3, 6.2% up to the wage base)",
        w2.social_security_tax,
        ss_wages_capped,
        620,
        "social_security_tax",
    );
    rate_check(
        &mut findings,
        "w2_medicare_rate",
        "Medicare tax (Box 6 vs Box 5, 1.45%)",
        w2.medicare_tax,
        w2.medicare_wages,
        145,
        "medicare_tax",
    );

    if w2.wages.cents() < 0 {
        findings.push(finding(
            "w2_negative_wages",
            "error",
            "W-2 Box 1 wages are negative".to_string(),
            Some("wages"),
        ));
    }
    if w2.federal_withholding.cents() < 0 {
        findings.push(finding(
            "w2_negative_withholding",
            "error",
            "W-2 Box 2 federal withholding is negative".to_string(),
            Some("federal_withholding"),
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w2_full() -> W2Input {
        W2Input {
            employer_ein: "12-3456789".into(),
            wages: Money::from_cents(5_000_000),
            federal_withholding: Money::from_cents(400_000),
            social_security_wages: Some(Money::from_cents(5_000_000)),
            // 6.2% of $50,000 = $3,100.00
            social_security_tax: Some(Money::from_cents(310_000)),
            medicare_wages: Some(Money::from_cents(5_000_000)),
            // 1.45% of $50,000 = $725.00
            medicare_tax: Some(Money::from_cents(72_500)),
        }
    }

    #[test]
    fn classify_titles() {
        assert_eq!(
            classify_text("Form W-2 Wage and Tax Statement 2025"),
            Some(DocKind::W2)
        );
        assert_eq!(
            classify_text("2025 Form 1099-INT Interest Income"),
            Some(DocKind::I1099Int)
        );
        assert_eq!(
            classify_text("Form 1099-B Proceeds From Broker and Barter Exchange Transactions"),
            Some(DocKind::I1099B)
        );
        assert_eq!(
            classify_text("Form 1099-DIV Dividends and Distributions"),
            Some(DocKind::I1099Div)
        );
        assert_eq!(
            classify_text("Form 1040 U.S. Individual Income Tax Return"),
            Some(DocKind::Form1040)
        );
        assert_eq!(classify_text("a grocery receipt"), None);
    }

    #[test]
    fn w2_checks_pass_on_consistent_w2() {
        let params = TaxParams::load_2025().unwrap();
        let findings = validate_w2(&w2_full(), &params);
        assert!(
            findings.iter().all(|f| f.severity == "info"),
            "unexpected findings: {findings:?}"
        );
    }

    #[test]
    fn w2_checks_flag_wrong_medicare() {
        let params = TaxParams::load_2025().unwrap();
        let mut w2 = w2_full();
        w2.medicare_tax = Some(Money::from_cents(200_000)); // $2,000 vs expected $725
        let findings = validate_w2(&w2, &params);
        assert!(findings
            .iter()
            .any(|f| f.code == "w2_medicare_rate" && f.severity == "warning"));
    }

    #[test]
    fn w2_checks_skip_when_boxes_missing() {
        let params = TaxParams::load_2025().unwrap();
        let mut w2 = w2_full();
        w2.social_security_wages = None;
        w2.social_security_tax = None;
        w2.medicare_wages = None;
        w2.medicare_tax = None;
        let findings = validate_w2(&w2, &params);
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().all(|f| f.severity == "info"));
    }

    #[test]
    fn no_ocr_is_honestly_unavailable() {
        let ocr = NoOcr;
        assert!(!ocr.available());
        assert!(ocr.ocr_page_image(&[]).is_err());
    }
}
