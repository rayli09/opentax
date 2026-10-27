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

/// 1099-INT input shape (template/ingest contract in v1; compute does not
/// yet consume it — see `tax node list`, `compute: false`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct I1099IntInput {
    pub payer_name: String,
    /// Box 1: interest income.
    pub interest_income: Money,
    /// Box 4: federal income tax withheld.
    #[serde(default)]
    pub federal_withholding: Money,
}

/// 1099-DIV input shape (template/ingest contract in v1; compute: false).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct I1099DivInput {
    pub payer_name: String,
    /// Box 1a: total ordinary dividends.
    pub total_ordinary_dividends: Money,
    /// Box 1b: qualified dividends.
    #[serde(default)]
    pub qualified_dividends: Money,
    /// Box 4: federal income tax withheld.
    #[serde(default)]
    pub federal_withholding: Money,
}

/// 1099-B input shape (template/ingest contract in v1; compute: false).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct I1099BInput {
    pub payer_name: String,
    /// Sum of proceeds (Box 1d) across transactions, cents.
    pub total_proceeds: Money,
    /// Sum of cost or other basis (Box 1e) across transactions, cents.
    pub total_basis: Money,
    /// Box 4: federal income tax withheld.
    #[serde(default)]
    pub federal_withholding: Money,
}

/// The complete set of inputs the v1 1040 slice consumes. Every field is
/// required: an absent field is a deserialization error, not a zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Form1040Input {
    pub status: FilingStatus,
    #[serde(default)]
    pub w2s: Vec<W2Input>,
    /// Schedule B / line 2b: taxable interest (explicitly provided, may be 0).
    pub interest_income: Money,
    /// Line 3b: ordinary dividends.
    pub ordinary_dividends: Money,
}

/// A computed return: line number (Form 1040 numbering) → fact result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputedReturn {
    pub lines: BTreeMap<String, FactResult<Money>>,
    /// True only when every computed line is `Complete`.
    pub complete: bool,
    /// Human-readable simplifications that apply to this computation.
    pub notes: Vec<String>,
}

/// Compute the v1 slice of Form 1040.
///
/// Lines computed (2025 Form 1040 numbering):
/// 1a/1z total amount from Form W-2 (wages), 2b taxable interest,
/// 3b ordinary dividends, 9 total income, 11 AGI, 12 standard deduction,
/// 15 taxable income, 16 tax, 25a W-2 withholding, 25d total withheld,
/// 33 total other payments and refundable credits (simplified: = 25d),
/// 34 overpaid (refund) when payments exceed tax, 37 amount owed otherwise.
///
/// Simplifications (docs/KNOWN_LIMITS.md): no adjustments to income (AGI =
/// total income), no itemizing, no credits, and line 16 is *bracket tax* on
/// taxable income for all amounts — the Tax Table (< $100k) and the
/// Qualified Dividends and Capital Gain Tax Worksheet are out of scope v1.
pub fn compute_1040(input: &Form1040Input, params: &TaxParams) -> ComputedReturn {
    let wages: Money = input.w2s.iter().map(|w| w.wages).sum();
    let withholding: Money = input.w2s.iter().map(|w| w.federal_withholding).sum();

    let total_income = wages + input.interest_income + input.ordinary_dividends;
    let agi = total_income;
    let deduction = params.standard_deduction(input.status);
    let taxable = Money::from_cents((agi - deduction).cents().max(0));
    let tax = bracket_tax(taxable, &params.bracket_table(input.status));
    let payments = withholding;
    let (overpaid, owed) = if payments.cents() > tax.cents() {
        (payments - tax, Money::ZERO)
    } else {
        (Money::ZERO, tax - payments)
    };

    let mut lines: BTreeMap<String, FactResult<Money>> = BTreeMap::new();
    let mut put = |line: &str, v: Money| {
        lines.insert(line.to_string(), FactResult::Complete(v));
    };
    put("1a", wages);
    put("1z", wages);
    put("2b", input.interest_income);
    put("3b", input.ordinary_dividends);
    put("9", total_income);
    put("11", agi);
    put("12", deduction);
    put("15", taxable);
    put("16", tax);
    put("25a", withholding);
    put("25d", withholding);
    put("33", payments);
    put("34", overpaid);
    put("37", owed);

    ComputedReturn {
        complete: true,
        lines,
        notes: vec![
            "Line 16 is bracket tax (ordinary brackets) for all taxable income; \
             Tax Table and QDCG worksheet are out of scope in v1."
                .to_string(),
            "AGI equals total income: no adjustments, credits, or itemized deductions in v1."
                .to_string(),
        ],
    }
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
}
