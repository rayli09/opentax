//! tax-forms: Form 1040 (TY2025) minimal vertical slice.
//!
//! Money fields are integer cents end-to-end; in JSON they use the `Money`
//! representation from tax-core (a string of cents, e.g. `"5000000"` is
//! $50,000.00). See docs/KNOWN_LIMITS.md for what is simplified or absent.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use tax_core::FilingStatus;
use tax_core::{bracket_tax, FactResult, Money, TaxParams};

/// One W-2 (Form W-2, Wage and Tax Statement). Money = cents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct W2Input {
    /// Box c: employer EIN, e.g. "12-3456789".
    pub employer_ein: String,
    /// Box 1: wages, tips, other compensation.
    pub wages: Money,
    /// Box 2: federal income tax withheld.
    pub federal_withholding: Money,
    /// Box 3: Social Security wages (optional in v1; used by tax-doc checks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_security_wages: Option<Money>,
    /// Box 4: Social Security tax withheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_security_tax: Option<Money>,
    /// Box 5: Medicare wages and tips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medicare_wages: Option<Money>,
    /// Box 6: Medicare tax withheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medicare_tax: Option<Money>,
}

/// One 1099-INT (Interest Income). Money = cents.
///
/// Field names follow the pre-existing ingest template; amounts that a
/// payer reports as none are represented by an absent field deserializing
/// to zero (the template's convention — a reported fact, not a missing
/// input).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct I1099IntInput {
    pub payer_name: String,
    /// Payer's EIN (optional; identification only, never computed on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_ein: Option<String>,
    /// Box 1: interest income → Form 1040 line 2b.
    pub interest_income: Money,
    /// Box 8: tax-exempt interest → Form 1040 line 2a.
    #[serde(default)]
    pub tax_exempt_interest: Money,
    /// Box 4: federal income tax withheld → Form 1040 line 25d component.
    #[serde(default)]
    pub federal_withholding: Money,
}

/// One 1099-DIV (Dividends and Distributions). Money = cents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct I1099DivInput {
    pub payer_name: String,
    /// Payer's EIN (optional; identification only, never computed on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_ein: Option<String>,
    /// Box 1a: total ordinary dividends → Form 1040 line 3b.
    pub total_ordinary_dividends: Money,
    /// Box 1b: qualified dividends → Form 1040 line 3a.
    #[serde(default)]
    pub qualified_dividends: Money,
    /// Box 4: federal income tax withheld → Form 1040 line 25d component.
    #[serde(default)]
    pub federal_withholding: Money,
}

/// Whether a sale is short-term or long-term (1099-B / Schedule D).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TxnTerm {
    Short,
    Long,
}

/// One transaction from a 1099-B (Proceeds From Broker and Barter
/// Exchange Transactions). Money = cents.
///
/// `proceeds`/`cost_basis` are `Option`s on purpose: a transaction whose
/// Box 1d or 1e was not transcribed is *incomplete* and must poison every
/// line that depends on it (missing ≠ 0). `adjustment` is different: an
/// absent adjustment means the broker reported none (Box 1g = 0), which
/// is a reported fact, so it defaults to zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerTxn {
    /// Description of property (Box 1a), e.g. "100 sh XYZ".
    pub description: String,
    /// Box 1d: proceeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proceeds: Option<Money>,
    /// Box 1e: cost or other basis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_basis: Option<Money>,
    /// Box 1g: adjustments (e.g. wash-sale loss disallowed), as reported.
    #[serde(default)]
    pub adjustment: Money,
    /// Short-term or long-term, per the broker's reporting.
    pub term: TxnTerm,
}

impl BrokerTxn {
    /// Gain (loss) = proceeds − basis + adjustment, when fully reported.
    pub fn gain(&self) -> Option<Money> {
        match (self.proceeds, self.cost_basis) {
            (Some(p), Some(b)) => Some(p - b + self.adjustment),
            _ => None,
        }
    }
}

/// One 1099-B: a broker's transactions for the year. Money = cents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct I1099BInput {
    pub payer_name: String,
    /// Broker's EIN (optional; identification only, never computed on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_ein: Option<String>,
    pub transactions: Vec<BrokerTxn>,
    /// Box 4: federal income tax withheld → Form 1040 line 25d component.
    #[serde(default)]
    pub federal_withholding: Money,
}

/// The complete set of inputs the v1 1040 slice consumes. Scalar income
/// fields are required: an absent field is a deserialization error, not a
/// zero. Document collections default to empty (no such documents is a
/// stated fact about the input set, as with `w2s`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Form1040Input {
    pub status: FilingStatus,
    #[serde(default)]
    pub w2s: Vec<W2Input>,
    /// Schedule B / line 2b: taxable interest entered directly (in
    /// addition to any 1099-INT Box 1 amounts).
    pub interest_income: Money,
    /// Line 3b: ordinary dividends entered directly (in addition to any
    /// 1099-DIV Box 1a amounts).
    pub ordinary_dividends: Money,
    #[serde(default)]
    pub f1099_ints: Vec<I1099IntInput>,
    #[serde(default)]
    pub f1099_divs: Vec<I1099DivInput>,
    #[serde(default)]
    pub f1099_bs: Vec<I1099BInput>,
}

/// A computed return: line number (Form 1040 numbering) → fact result.
///
/// In addition to numbered 1040 lines, the map carries two internal keys
/// that feed a future Schedule D: `schd_net_short` and `schd_net_long`
/// (net short-term / long-term gain or loss aggregated from 1099-B
/// transactions). They are not Form 1040 line numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputedReturn {
    pub lines: BTreeMap<String, FactResult<Money>>,
    /// True only when every computed line is `Complete`.
    pub complete: bool,
    /// Human-readable simplifications that apply to this computation.
    pub notes: Vec<String>,
}

/// Add two money facts, propagating completeness (Fact Graph semantics).
fn add_facts(a: FactResult<Money>, b: FactResult<Money>) -> FactResult<Money> {
    use FactResult::*;
    match (a, b) {
        (Incomplete, _) | (_, Incomplete) => Incomplete,
        (Placeholder(x), Placeholder(y)) => Placeholder(x + y),
        (Placeholder(x), Complete(y)) | (Complete(x), Placeholder(y)) => Placeholder(x + y),
        (Complete(x), Complete(y)) => Complete(x + y),
    }
}

/// A deterministic consistency finding about a single document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsistencyFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
}

/// Consistency checks for one 1099-DIV. Qualified dividends (Box 1b) are
/// by definition a subset of total ordinary dividends (Box 1a), so 1b > 1a
/// is always a transcription or payer error worth surfacing.
pub fn validate_1099_div(div: &I1099DivInput) -> Vec<ConsistencyFinding> {
    let mut findings = Vec::new();
    if div.qualified_dividends > div.total_ordinary_dividends {
        findings.push(ConsistencyFinding {
            code: "div_qualified_exceeds_ordinary",
            severity: "warning",
            message: format!(
                "1099-DIV from {}: qualified dividends (Box 1b, {}) exceed total ordinary \
                 dividends (Box 1a, {}); qualified dividends are a subset of ordinary \
                 dividends — check the transcription",
                div.payer_name, div.qualified_dividends, div.total_ordinary_dividends
            ),
        });
    }
    findings
}

/// Compute the v1 slice of Form 1040.
///
/// Lines computed (2025 Form 1040 numbering):
/// 1a/1z total amount from Form W-2 (wages), 2a tax-exempt interest
/// (Σ 1099-INT Box 8), 2b taxable interest (direct + Σ 1099-INT Box 1),
/// 3a qualified dividends (Σ 1099-DIV Box 1b), 3b ordinary dividends
/// (direct + Σ 1099-DIV Box 1a), 7 capital gain/loss (net short + net
/// long from 1099-B transactions), 9 total income, 11 AGI, 12 standard
/// deduction, 15 taxable income, 16 tax, 25a W-2 withholding, 25d total
/// withheld (W-2 + 1099-INT/DIV/B Box 4), 33 total other payments and
/// refundable credits (simplified: = 25d), 34 overpaid (refund) when
/// payments exceed tax, 37 amount owed otherwise. Plus the internal
/// `schd_net_short` / `schd_net_long` keys described on
/// [`ComputedReturn`].
///
/// Completeness: a 1099-B transaction missing proceeds or basis makes
/// lines 7, 9, 11, 15, 16, 34, 37 (and both `schd_*` keys) `Incomplete`;
/// independent lines stay `Complete`.
///
/// Simplifications (docs/KNOWN_LIMITS.md): no adjustments to income (AGI =
/// total income), no itemizing, no credits, no Schedule D worksheet, and
/// line 16 is *bracket tax* on taxable income for all amounts — the Tax
/// Table (< $100k) and the Qualified Dividends and Capital Gain Tax
/// Worksheet are out of scope v1, so qualified dividends and capital
/// gains are taxed at ordinary rates here.
pub fn compute_1040(input: &Form1040Input, params: &TaxParams) -> ComputedReturn {
    let wages: Money = input.w2s.iter().map(|w| w.wages).sum();
    let w2_withholding: Money = input.w2s.iter().map(|w| w.federal_withholding).sum();

    // 1099-INT: Box 1 → 2b, Box 8 → 2a, Box 4 → 25d.
    let int_interest: Money = input.f1099_ints.iter().map(|f| f.interest_income).sum();
    let tax_exempt: Money = input.f1099_ints.iter().map(|f| f.tax_exempt_interest).sum();
    let int_withholding: Money = input.f1099_ints.iter().map(|f| f.federal_withholding).sum();

    // 1099-DIV: Box 1a → 3b, Box 1b → 3a, Box 4 → 25d.
    let div_ordinary: Money = input
        .f1099_divs
        .iter()
        .map(|f| f.total_ordinary_dividends)
        .sum();
    let div_qualified: Money = input.f1099_divs.iter().map(|f| f.qualified_dividends).sum();
    let div_withholding: Money = input.f1099_divs.iter().map(|f| f.federal_withholding).sum();

    // 1099-B: per-transaction gains, split by term. One transaction with
    // unreported proceeds or basis poisons both nets (missing ≠ 0).
    let b_withholding: Money = input.f1099_bs.iter().map(|f| f.federal_withholding).sum();
    let mut net_short = Money::ZERO;
    let mut net_long = Money::ZERO;
    let mut b_incomplete = false;
    for form in &input.f1099_bs {
        for txn in &form.transactions {
            match txn.gain() {
                Some(g) => match txn.term {
                    TxnTerm::Short => net_short = net_short + g,
                    TxnTerm::Long => net_long = net_long + g,
                },
                None => b_incomplete = true,
            }
        }
    }
    let net_short_f = if b_incomplete {
        FactResult::Incomplete
    } else {
        FactResult::Complete(net_short)
    };
    let net_long_f = if b_incomplete {
        FactResult::Incomplete
    } else {
        FactResult::Complete(net_long)
    };
    let capital_gain_f = add_facts(net_short_f.clone(), net_long_f.clone());

    let line2b = input.interest_income + int_interest;
    let line3b = input.ordinary_dividends + div_ordinary;
    let total_withholding = w2_withholding + int_withholding + div_withholding + b_withholding;

    let complete = |v: Money| FactResult::Complete(v);
    let total_income_f = add_facts(
        add_facts(complete(wages), complete(line2b)),
        add_facts(complete(line3b), capital_gain_f.clone()),
    );
    let agi_f = total_income_f.clone();
    let deduction = params.standard_deduction(input.status);
    let taxable_f = agi_f
        .clone()
        .map(|agi| Money::from_cents((agi - deduction).cents().max(0)));
    let tax_f = taxable_f
        .clone()
        .map(|t| bracket_tax(t, &params.bracket_table(input.status)));
    let payments_f = complete(total_withholding);
    let (overpaid_f, owed_f) = match tax_f.clone() {
        FactResult::Complete(tax) | FactResult::Placeholder(tax) => {
            let wrap = |v: Money| match tax_f {
                FactResult::Placeholder(_) => FactResult::Placeholder(v),
                _ => FactResult::Complete(v),
            };
            if total_withholding.cents() > tax.cents() {
                (wrap(total_withholding - tax), wrap(Money::ZERO))
            } else {
                (wrap(Money::ZERO), wrap(tax - total_withholding))
            }
        }
        FactResult::Incomplete => (FactResult::Incomplete, FactResult::Incomplete),
    };

    let mut lines: BTreeMap<String, FactResult<Money>> = BTreeMap::new();
    lines.insert("1a".into(), complete(wages));
    lines.insert("1z".into(), complete(wages));
    lines.insert("2a".into(), complete(tax_exempt));
    lines.insert("2b".into(), complete(line2b));
    lines.insert("3a".into(), complete(div_qualified));
    lines.insert("3b".into(), complete(line3b));
    lines.insert("7".into(), capital_gain_f);
    lines.insert("schd_net_short".into(), net_short_f);
    lines.insert("schd_net_long".into(), net_long_f);
    lines.insert("9".into(), total_income_f);
    lines.insert("11".into(), agi_f);
    lines.insert("12".into(), complete(deduction));
    lines.insert("15".into(), taxable_f);
    lines.insert("16".into(), tax_f);
    lines.insert("25a".into(), complete(w2_withholding));
    lines.insert("25d".into(), complete(total_withholding));
    lines.insert("33".into(), payments_f);
    lines.insert("34".into(), overpaid_f);
    lines.insert("37".into(), owed_f);

    let complete = lines.values().all(|f| f.is_complete());
    ComputedReturn {
        complete,
        lines,
        notes: vec![
            "Line 16 is bracket tax (ordinary brackets) on all taxable income, \
             including qualified dividends (line 3a) and capital gains (line 7): \
             the Qualified Dividends and Capital Gain Tax Worksheet and the Tax \
             Table are out of scope in v1, so those amounts are taxed at \
             ordinary rates here."
                .to_string(),
            "Line 7 is the direct net of 1099-B transactions; the Schedule D \
             worksheet is not produced in v1."
                .to_string(),
            "AGI equals total income: no adjustments, credits, or itemized deductions in v1."
                .to_string(),
        ],
    }
}

/// Format a money amount for a PDF form field: exact decimal dollars, no
/// thousands separators, always two decimals (`423150` cents ->
/// `"4231.50"`, `-50000` -> `"-500.00"`, `0` -> `"0.00"`). Whole-dollar
/// rounding is NOT applied (see docs/KNOWN_LIMITS.md).
pub fn format_amount(m: Money) -> String {
    m.to_string()
}

// ---------------------------------------------------------------------------
// Form 1040 (TY2025) line -> AcroForm field map (PDF export)
// ---------------------------------------------------------------------------

/// One entry of the 1040 field map (`forms/f1040-2025-field-map.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldMapEntry {
    /// Form line this field prints on the paper form (engine numbering;
    /// for TY2025 the paper form prints some of these with suffixes —
    /// 7a, 11a/11b, 12e — see `note`). `"filing_status"` for checkboxes.
    pub line: String,
    /// Engine line whose value feeds this field, when it differs from
    /// `line` (e.g. form line 24 is fed by engine line 16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_line: Option<String>,
    /// Fully qualified AcroForm field name in the IRS TY2025 template.
    pub qname: String,
    /// `"amount"` or `"checkbox"`.
    pub kind: String,
    /// For checkboxes: the `FilingStatus::as_str()` this box represents
    /// (`"qss"` is recorded for completeness; the engine has no QSS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// For checkboxes: the field's on-state name (`"1"`..`"5"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// How this assignment was verified (geometric row match).
    pub note: String,
}

impl FieldMapEntry {
    /// The engine line this entry draws its value from.
    pub fn source_line(&self) -> &str {
        self.source_line.as_deref().unwrap_or(&self.line)
    }
}

/// The TY2025 Form 1040 line -> field map, as data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct F1040FieldMap {
    pub form: String,
    pub year: u32,
    /// How the map was derived (geometric verification method).
    pub derivation: String,
    pub entries: Vec<FieldMapEntry>,
}

const F1040_2025_FIELD_MAP_JSON: &str = include_str!("../forms/f1040-2025-field-map.json");

/// The parsed TY2025 Form 1040 field map (parsed once, leaked to 'static —
/// the map is immutable process-lifetime data).
pub fn f1040_2025_field_map() -> &'static F1040FieldMap {
    static MAP: std::sync::OnceLock<F1040FieldMap> = std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        serde_json::from_str(F1040_2025_FIELD_MAP_JSON)
            .expect("bundled f1040-2025-field-map.json must parse")
    })
}

/// One value to write into the template PDF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FillValue {
    /// Form line (or `"filing_status"`) this value belongs to.
    pub line: String,
    /// Qualified AcroForm field name.
    pub field: String,
    /// The exact string to write (formatted amount, or checkbox on-state).
    pub value: String,
}

/// A form field the export deliberately does not fill, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedField {
    pub field: String,
    pub reason: String,
}

/// The plan for one PDF export: the values to fill, plus what was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FillPlan {
    pub values: Vec<FillValue>,
    pub skipped: Vec<SkippedField>,
}

/// Identity fields a stored return does not carry today; the PDF export
/// never invents them, it reports them as skipped.
const IDENTITY_SKIPS: &[(&str, &str)] = &[
    ("taxpayer_name", "not stored in return"),
    ("taxpayer_ssn", "not stored in return"),
    ("home_address", "not stored in return"),
];

/// Turn computed 1040 lines + filing status into template fill values.
///
/// Returns `Err(incomplete_lines)` — the deduplicated engine line keys, in
/// map order — when any mapped line is missing, `Incomplete`, or only a
/// `Placeholder`: an incomplete return is never exported (and never
/// zero-filled). Lines 34 and 37 are mutually exclusive on the form: only
/// the one that is > 0 is filled; when both are 0, neither is.
pub fn build_fill_values(
    lines: &BTreeMap<String, FactResult<Money>>,
    status: FilingStatus,
) -> Result<FillPlan, Vec<String>> {
    let map = f1040_2025_field_map();
    let mut incomplete: Vec<String> = Vec::new();
    let mut amounts: Vec<(FieldMapEntry, Money)> = Vec::new();
    for entry in &map.entries {
        if entry.kind != "amount" {
            continue;
        }
        let src = entry.source_line();
        match lines.get(src) {
            Some(FactResult::Complete(m)) => amounts.push((entry.clone(), *m)),
            _ => {
                if !incomplete.iter().any(|l| l == src) {
                    incomplete.push(src.to_string());
                }
            }
        }
    }
    if !incomplete.is_empty() {
        return Err(incomplete);
    }

    // Lines 34 (overpaid) and 37 (owed) are mutually exclusive on the form.
    let val = |line: &str| -> Option<Money> {
        amounts
            .iter()
            .find(|(e, _)| e.line == line)
            .map(|(_, m)| *m)
    };
    let keep_34 = val("34").map(|m| m.cents() > 0).unwrap_or(false);
    let keep_37 = val("37").map(|m| m.cents() > 0).unwrap_or(false);

    let mut values: Vec<FillValue> = Vec::new();
    for (entry, m) in &amounts {
        if entry.line == "34" && !keep_34 {
            continue;
        }
        if entry.line == "37" && !keep_37 {
            continue;
        }
        values.push(FillValue {
            line: entry.line.clone(),
            field: entry.qname.clone(),
            value: format_amount(*m),
        });
    }
    // Filing-status checkbox for the return's status.
    for entry in &map.entries {
        if entry.kind == "checkbox" && entry.status.as_deref() == Some(status.as_str()) {
            values.push(FillValue {
                line: entry.line.clone(),
                field: entry.qname.clone(),
                value: entry
                    .value
                    .clone()
                    .expect("checkbox map entries carry an on-state value"),
            });
        }
    }

    let skipped = IDENTITY_SKIPS
        .iter()
        .map(|(field, reason)| SkippedField {
            field: field.to_string(),
            reason: reason.to_string(),
        })
        .collect();
    Ok(FillPlan { values, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden_input() -> Form1040Input {
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
            f1099_ints: vec![],
            f1099_divs: vec![],
            f1099_bs: vec![],
        }
    }

    #[test]
    fn golden_single_w2_minimal() {
        let params = TaxParams::load_2025().unwrap();
        let r = compute_1040(&golden_input(), &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("1a"), 5_000_000);
        assert_eq!(line("9"), 5_000_000);
        assert_eq!(line("11"), 5_000_000, "AGI $50,000");
        assert_eq!(line("12"), 1_575_000, "standard deduction $15,750");
        assert_eq!(line("15"), 3_425_000, "taxable $34,250");
        // 11925*10% + (34250-11925)*12% = 1192.50 + 2679.00 = $3,871.50
        assert_eq!(line("16"), 387_150, "tax $3,871.50");
        assert_eq!(line("25d"), 400_000);
        assert_eq!(line("34"), 12_850, "refund $128.50");
        assert_eq!(line("37"), 0);
    }

    #[test]
    fn golden_case_json_round_trip() {
        let json = r#"{
            "status": "single",
            "w2s": [{"employer_ein": "12-3456789", "wages": "5000000", "federal_withholding": "400000"}],
            "interest_income": "0",
            "ordinary_dividends": "0"
        }"#;
        let input: Form1040Input = serde_json::from_str(json).unwrap();
        assert_eq!(input, golden_input());
    }

    #[test]
    fn amount_owed_when_tax_exceeds_withholding() {
        let params = TaxParams::load_2025().unwrap();
        let mut input = golden_input();
        input.w2s[0].federal_withholding = Money::from_cents(300_000);
        let r = compute_1040(&input, &params);
        assert_eq!(r.lines["34"].value().copied().unwrap(), Money::ZERO);
        assert_eq!(r.lines["37"].value().copied().unwrap().cents(), 87_150);
    }

    fn case002_input() -> Form1040Input {
        let mut input = golden_input();
        input.f1099_ints = vec![I1099IntInput {
            payer_name: "Example Bank".into(),
            payer_ein: None,
            interest_income: Money::from_cents(50_000),
            tax_exempt_interest: Money::ZERO,
            federal_withholding: Money::ZERO,
        }];
        input.f1099_divs = vec![I1099DivInput {
            payer_name: "Example Fund".into(),
            payer_ein: None,
            total_ordinary_dividends: Money::from_cents(100_000),
            qualified_dividends: Money::from_cents(60_000),
            federal_withholding: Money::ZERO,
        }];
        input.f1099_bs = vec![I1099BInput {
            payer_name: "Example Broker".into(),
            payer_ein: None,
            transactions: vec![
                BrokerTxn {
                    description: "100 sh EXAMPLE".into(),
                    proceeds: Some(Money::from_cents(800_000)),
                    cost_basis: Some(Money::from_cents(600_000)),
                    adjustment: Money::ZERO,
                    term: TxnTerm::Short,
                },
                BrokerTxn {
                    description: "50 sh EXAMPLE".into(),
                    proceeds: Some(Money::from_cents(300_000)),
                    cost_basis: Some(Money::from_cents(350_000)),
                    adjustment: Money::ZERO,
                    term: TxnTerm::Long,
                },
            ],
            federal_withholding: Money::ZERO,
        }];
        input
    }

    /// Golden case 002 (benchmark/cases/f1040/2025/002-w2-1099s): W-2 plus
    /// one each of 1099-INT/DIV/B. All amounts verified by hand:
    /// total income 50000 + 500 + 1000 + (2000 − 500) = $53,000; taxable
    /// 53000 − 15750 = $37,250; bracket tax 11925×10% + 25325×12%
    /// = 1192.50 + 3039.00 = $4,231.50; owed 4231.50 − 4000 = $231.50.
    #[test]
    fn golden_w2_with_1099s() {
        let params = TaxParams::load_2025().unwrap();
        let r = compute_1040(&case002_input(), &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("1a"), 5_000_000);
        assert_eq!(line("2a"), 0, "no tax-exempt interest reported");
        assert_eq!(line("2b"), 50_000, "1099-INT Box 1 $500");
        assert_eq!(line("3a"), 60_000, "1099-DIV Box 1b $600");
        assert_eq!(line("3b"), 100_000, "1099-DIV Box 1a $1,000");
        assert_eq!(line("schd_net_short"), 200_000, "ST gain $2,000");
        assert_eq!(line("schd_net_long"), -50_000, "LT loss −$500");
        assert_eq!(line("7"), 150_000, "net capital gain $1,500");
        assert_eq!(line("9"), 5_300_000, "total income $53,000");
        assert_eq!(line("11"), 5_300_000, "AGI $53,000");
        assert_eq!(line("12"), 1_575_000);
        assert_eq!(line("15"), 3_725_000, "taxable $37,250");
        assert_eq!(line("16"), 423_150, "tax $4,231.50");
        assert_eq!(line("25a"), 400_000);
        assert_eq!(line("25d"), 400_000);
        assert_eq!(line("33"), 400_000);
        assert_eq!(line("34"), 0, "nothing overpaid");
        assert_eq!(line("37"), 23_150, "amount owed $231.50");
    }

    #[test]
    fn case002_json_round_trip() {
        let json = r#"{
            "status": "single",
            "w2s": [{"employer_ein": "12-3456789", "wages": "5000000", "federal_withholding": "400000"}],
            "interest_income": "0",
            "ordinary_dividends": "0",
            "f1099_ints": [{"payer_name": "Example Bank", "interest_income": "50000"}],
            "f1099_divs": [{"payer_name": "Example Fund", "total_ordinary_dividends": "100000", "qualified_dividends": "60000"}],
            "f1099_bs": [{"payer_name": "Example Broker", "transactions": [
                {"description": "100 sh EXAMPLE", "proceeds": "800000", "cost_basis": "600000", "term": "short"},
                {"description": "50 sh EXAMPLE", "proceeds": "300000", "cost_basis": "350000", "term": "long"}
            ]}]
        }"#;
        let input: Form1040Input = serde_json::from_str(json).unwrap();
        assert_eq!(input, case002_input());
    }

    #[test]
    fn div_qualified_exceeding_ordinary_is_flagged() {
        let mut div = I1099DivInput {
            payer_name: "Bad Fund".into(),
            payer_ein: None,
            total_ordinary_dividends: Money::from_cents(100_000),
            qualified_dividends: Money::from_cents(60_000),
            federal_withholding: Money::ZERO,
        };
        assert!(validate_1099_div(&div).is_empty());
        div.qualified_dividends = Money::from_cents(100_001);
        let findings = validate_1099_div(&div);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "div_qualified_exceeds_ordinary");
        assert_eq!(findings[0].severity, "warning");
    }

    #[test]
    fn incomplete_1099b_poisons_only_dependent_lines() {
        let params = TaxParams::load_2025().unwrap();
        let mut input = case002_input();
        input.f1099_bs[0].transactions[0].cost_basis = None;
        let r = compute_1040(&input, &params);
        assert!(!r.complete);
        for line in [
            "7",
            "schd_net_short",
            "schd_net_long",
            "9",
            "11",
            "15",
            "16",
            "34",
            "37",
        ] {
            assert_eq!(
                r.lines[line],
                FactResult::Incomplete,
                "line {line} must be incomplete"
            );
        }
        // Independent lines stay complete and exact.
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("1a"), 5_000_000);
        assert_eq!(line("2b"), 50_000);
        assert_eq!(line("3a"), 60_000);
        assert_eq!(line("3b"), 100_000);
        assert_eq!(line("12"), 1_575_000);
        assert_eq!(line("25d"), 400_000);
        assert_eq!(line("33"), 400_000);
    }

    #[test]
    fn withholding_from_1099s_flows_to_25d() {
        let params = TaxParams::load_2025().unwrap();
        let mut input = golden_input();
        input.f1099_ints = vec![I1099IntInput {
            payer_name: "Bank".into(),
            payer_ein: None,
            interest_income: Money::ZERO,
            tax_exempt_interest: Money::from_cents(70_000),
            federal_withholding: Money::from_cents(10_000),
        }];
        input.f1099_divs = vec![I1099DivInput {
            payer_name: "Fund".into(),
            payer_ein: None,
            total_ordinary_dividends: Money::ZERO,
            qualified_dividends: Money::ZERO,
            federal_withholding: Money::from_cents(20_000),
        }];
        input.f1099_bs = vec![I1099BInput {
            payer_name: "Broker".into(),
            payer_ein: None,
            transactions: vec![],
            federal_withholding: Money::from_cents(30_000),
        }];
        let r = compute_1040(&input, &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("25a"), 400_000, "25a is W-2 withholding only");
        assert_eq!(line("25d"), 460_000, "25d adds INT/DIV/B Box 4 amounts");
        assert_eq!(line("2a"), 70_000, "1099-INT Box 8 lands on line 2a");
        assert_eq!(line("2b"), 0, "tax-exempt interest is not taxable interest");
    }

    // -- PDF export: field map + fill values -------------------------------

    #[test]
    fn format_amount_exact_decimals() {
        assert_eq!(format_amount(Money::from_cents(423_150)), "4231.50");
        assert_eq!(format_amount(Money::from_cents(-50_000)), "-500.00");
        assert_eq!(format_amount(Money::ZERO), "0.00");
        assert_eq!(format_amount(Money::from_cents(5)), "0.05");
        assert_eq!(format_amount(Money::from_cents(-5)), "-0.05");
        // No thousands separators, ever.
        assert_eq!(format_amount(Money::from_cents(123_456_789)), "1234567.89");
    }

    #[test]
    fn field_map_loads_with_qnames() {
        let map = f1040_2025_field_map();
        assert_eq!(map.form, "f1040");
        assert_eq!(map.year, 2025);
        assert!(!map.derivation.is_empty());
        let amounts: Vec<_> = map.entries.iter().filter(|e| e.kind == "amount").collect();
        // 20 mapped amount fields: 1a 1z 2a 2b 3a 3b 7 9 11a 11b 12e 14 15
        // 16 24 25a 25d 33 34 37 — engine line 11 feeds both 11a and 11b,
        // line 12 feeds 12e and 14, and line 16 feeds 16 and 24.
        assert_eq!(amounts.len(), 20);
        for e in &map.entries {
            assert!(e.qname.starts_with("topmostSubform[0]."), "{}", e.qname);
            assert!(!e.note.is_empty(), "every entry documents its evidence");
        }
        let src = |line: &str| {
            map.entries
                .iter()
                .find(|e| e.kind == "amount" && e.line == line)
                .map(|e| e.source_line().to_string())
        };
        assert_eq!(src("24").as_deref(), Some("16"));
        assert_eq!(src("14").as_deref(), Some("12"));
    }

    /// A complete lines map with benign values; 34/37 set per test.
    fn complete_lines(overpaid: i64, owed: i64) -> BTreeMap<String, FactResult<Money>> {
        let mut lines = BTreeMap::new();
        for l in [
            "1a", "1z", "2a", "2b", "3a", "3b", "7", "9", "11", "12", "15", "16", "25a", "25d",
            "33",
        ] {
            lines.insert(l.to_string(), FactResult::Complete(Money::from_cents(100)));
        }
        lines.insert(
            "34".to_string(),
            FactResult::Complete(Money::from_cents(overpaid)),
        );
        lines.insert(
            "37".to_string(),
            FactResult::Complete(Money::from_cents(owed)),
        );
        lines
    }

    #[test]
    fn fill_values_owed_fills_37_only() {
        let plan = build_fill_values(&complete_lines(0, 23_150), FilingStatus::Single).unwrap();
        let by_line = |l: &str| plan.values.iter().find(|v| v.line == l);
        assert!(
            by_line("34").is_none(),
            "line 34 stays blank when 37 is owed"
        );
        assert_eq!(by_line("37").unwrap().value, "231.50");
        // All other mapped lines are filled, including complete zeros set
        // to 100 cents here and the derived fields 11b/14/24.
        assert_eq!(by_line("11").unwrap().value, "1.00");
        assert!(plan
            .values
            .iter()
            .any(|v| v.field.ends_with("f2_01[0]") && v.value == "1.00"));
        assert!(plan
            .values
            .iter()
            .any(|v| v.line == "14" && v.value == "1.00"));
        assert!(plan
            .values
            .iter()
            .any(|v| v.line == "24" && v.value == "1.00"));
        // Identity fields are reported skipped, never invented.
        assert_eq!(plan.skipped.len(), 3);
        assert!(plan
            .skipped
            .iter()
            .all(|s| s.reason == "not stored in return"));
    }

    #[test]
    fn fill_values_refund_fills_34_only_and_zero_zero_neither() {
        let plan = build_fill_values(&complete_lines(12_850, 0), FilingStatus::Single).unwrap();
        assert_eq!(
            plan.values.iter().find(|v| v.line == "34").unwrap().value,
            "128.50"
        );
        assert!(plan.values.iter().all(|v| v.line != "37"));
        let plan = build_fill_values(&complete_lines(0, 0), FilingStatus::Single).unwrap();
        assert!(plan.values.iter().all(|v| v.line != "34" && v.line != "37"));
    }

    #[test]
    fn fill_values_incomplete_line_blocks_export() {
        let mut lines = complete_lines(0, 0);
        lines.insert("7".to_string(), FactResult::Incomplete);
        lines.remove("25d");
        let err = build_fill_values(&lines, FilingStatus::Single).unwrap_err();
        assert_eq!(err, vec!["7".to_string(), "25d".to_string()]);
    }

    #[test]
    fn status_checkbox_mapping() {
        let cases = [
            (FilingStatus::Single, "c1_8[0]", "1"),
            (FilingStatus::Mfj, "c1_8[1]", "2"),
            (FilingStatus::Mfs, "c1_8[2]", "3"),
            (FilingStatus::Hoh, "c1_8[0]", "4"),
        ];
        for (status, leaf, on_state) in cases {
            let plan = build_fill_values(&complete_lines(0, 0), status).unwrap();
            let cb: Vec<_> = plan
                .values
                .iter()
                .filter(|v| v.line == "filing_status")
                .collect();
            assert_eq!(cb.len(), 1, "exactly one status box for {status:?}");
            assert!(cb[0].field.ends_with(leaf), "{} vs {leaf}", cb[0].field);
            assert_eq!(cb[0].value, on_state);
            if status == FilingStatus::Hoh {
                // HOH shares the leaf name with Single but lives directly
                // under Page1[0], not under Checkbox_ReadOrder[0].
                assert!(!cb[0].field.contains("Checkbox_ReadOrder"));
            } else {
                assert!(cb[0].field.contains("Checkbox_ReadOrder"));
            }
        }
    }
}
