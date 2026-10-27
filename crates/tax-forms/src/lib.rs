//! tax-forms: Form 1040 (TY2025) minimal vertical slice.
//!
//! Money fields are integer cents end-to-end; in JSON they use the `Money`
//! representation from tax-core (a string of cents, e.g. `"5000000"` is
//! $50,000.00). See docs/KNOWN_LIMITS.md for what is simplified or absent.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use tax_core::FilingStatus;
use tax_core::{FactResult, Money, TaxParams};

pub mod worksheets;
pub use worksheets::{
    capital_loss_carryover, classify_dependent, combine_schedule_d, deductible_part_of_se_tax,
    line16_tax, qdcg_tax, schedule_8812, schedule_se_tax, tax_on_amount, DependentCreditClass,
    Schedule8812, ScheduleDCombination, ScheduleSe,
};

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

/// One 1099-NEC (Nonemployee Compensation). Money = cents.
///
/// `expenses` is NOT a box on Form 1099-NEC: it is the total of the
/// Schedule C expenses for that engagement, supplied from the filer's
/// records (v1 simplification — one expense total per engagement, no
/// per-category Schedule C; see docs/KNOWN_LIMITS.md). An absent field
/// means the engagement reported no expenses, following the template
/// convention for reported-none amounts. A net loss on an engagement is
/// allowed and flows through to Schedule 1 line 3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct I1099NecInput {
    pub payer_name: String,
    /// Payer's EIN (optional; identification only, never computed on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_ein: Option<String>,
    /// Box 1: nonemployee compensation → Schedule C gross receipts.
    pub nonemployee_compensation: Money,
    /// Box 4: federal income tax withheld → Form 1040 line 25d component.
    #[serde(default)]
    pub federal_withholding: Money,
    /// Total Schedule C expenses for this engagement (not a 1099-NEC box).
    #[serde(default)]
    pub expenses: Money,
}

impl I1099NecInput {
    /// Schedule C net profit (loss) for this engagement: Box 1 − expenses.
    pub fn net_profit(&self) -> Money {
        self.nonemployee_compensation - self.expenses
    }
}

/// A dependent's relationship to the taxpayer, for the credits on
/// Schedule 8812. Only the child-type relationships can qualify for the
/// child tax credit; `"other"` (any other qualifying relative) takes
/// the credit-for-other-dependents path only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependentRelationship {
    Child,
    Stepchild,
    FosterChild,
    Other,
}

fn default_months_lived_with_taxpayer() -> u32 {
    12
}

/// One dependent claimed on the return (Form 1040, Dependents section).
/// Dependents are not income documents: they feed only the Schedule
/// 8812 credits (child tax credit, credit for other dependents,
/// additional child tax credit). A dependent's SSN is optional input:
/// when stored (normalized to 9 digits, see [`normalize_ssn`]) the PDF
/// export prints it in the Dependents table question (3); the child
/// tax credit legally requires the child to have an SSN, which v1
/// models as a validation warning, not a denial (docs/KNOWN_LIMITS.md).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependentInput {
    /// Full name as it should print in the Dependents table.
    pub name: String,
    /// Social Security number, normalized digits-only when set via the
    /// CLI (`tax form add` / identity intake). Masked in every output;
    /// the full value appears only inside the filled PDF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssn: Option<String>,
    pub relationship: DependentRelationship,
    /// Age on December 31 of the tax year. The child tax credit
    /// requires the child to be *under* 17 at year-end (Schedule 8812
    /// instructions: "under age 17 at the end of 2025").
    pub age: u32,
    /// Months of the tax year the dependent lived with the taxpayer
    /// (0–12). The residency test modeled in v1 is "more than half the
    /// year", i.e. strictly more than 6 months.
    #[serde(default = "default_months_lived_with_taxpayer")]
    pub months_lived_with_taxpayer: u32,
}

/// Consistency findings for one dependent entry, mirroring
/// [`validate_1099_div`]: classification consequences worth surfacing
/// before the return is filed (the engine takes them into account in
/// [`worksheets::classify_dependent`]; these findings explain it).
pub fn validate_dependent(dep: &DependentInput) -> Vec<ConsistencyFinding> {
    let mut findings = Vec::new();
    if dep.relationship != DependentRelationship::Other && dep.age >= 17 {
        findings.push(ConsistencyFinding {
            code: "dependent_ctc_age",
            severity: "info",
            message: format!(
                "dependent {}: age {} on December 31 is 17 or older, so the child tax \
                 credit does not apply (the child must be under 17 at year-end); the \
                 credit for other dependents is taken instead, if the residency test \
                 is met",
                dep.name, dep.age
            ),
        });
    }
    if dep.relationship == DependentRelationship::Other {
        findings.push(ConsistencyFinding {
            code: "dependent_odc_only",
            severity: "info",
            message: format!(
                "dependent {}: relationship \"other\" takes the credit for other \
                 dependents path only — never the child tax credit",
                dep.name
            ),
        });
    }
    if classify_dependent(dep) == DependentCreditClass::ChildTaxCredit && dep.ssn.is_none() {
        findings.push(ConsistencyFinding {
            code: "dependent_ctc_ssn",
            severity: "warning",
            message: format!(
                "dependent {}: CTC requires the child to have an SSN (v1 still \
                 computes; a real filing would deny)",
                dep.name
            ),
        });
    }
    if dep.months_lived_with_taxpayer <= 6 {
        findings.push(ConsistencyFinding {
            code: "dependent_residency",
            severity: "warning",
            message: format!(
                "dependent {}: lived with the taxpayer {} month(s), which fails the \
                 more-than-half-the-year residency test as modeled in v1 — no credit \
                 is taken for this dependent",
                dep.name, dep.months_lived_with_taxpayer
            ),
        });
    }
    findings
}

/// Schedule C net profit (loss) over all 1099-NEC engagements
/// (Σ (Box 1 − expenses)) — Schedule 1 line 3, flowing to Form 1040
/// line 8. An empty set is a stated fact: net zero.
pub fn schedule_c_net_profit(necs: &[I1099NecInput]) -> Money {
    necs.iter().map(|n| n.net_profit()).sum()
}

/// Normalize an SSN as typed by a user: accepts "123-45-6789" or the
/// bare 9 digits (stray spaces are ignored). Returns the digits-only
/// form, or `None` when the input is not exactly 9 digits.
pub fn normalize_ssn(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| *c != '-' && *c != ' ').collect();
    if digits.len() == 9 && digits.chars().all(|c| c.is_ascii_digit()) {
        Some(digits)
    } else {
        None
    }
}

/// Mask an SSN for display: `"***-**-6789"`. Accepts the normalized
/// digits-only form or a dashed form; anything unrecognized masks to
/// `"***-**-****"` so a malformed value is never echoed in full.
pub fn mask_ssn(ssn: &str) -> String {
    match normalize_ssn(ssn) {
        Some(digits) => format!("***-**-{}", &digits[5..]),
        None => "***-**-****".to_string(),
    }
}

/// Light structural check for a YYYY-MM-DD date (shape and plausible
/// month/day ranges; not a calendar validation).
fn valid_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |lo: usize, hi: usize| -> Option<u32> { s[lo..hi].parse().ok() };
    match (num(0, 4), num(5, 7), num(8, 10)) {
        (Some(_), Some(m), Some(d)) => (1..=12).contains(&m) && (1..=31).contains(&d),
        _ => false,
    }
}

/// The identity facts of one person (the taxpayer, or the spouse):
/// what prints in the Form 1040 header / signature blocks, plus the
/// date of birth and blindness that drive the age-65/blind standard
/// deduction additions. Every field is optional — a partial identity
/// fills what is present in the PDF export and skips the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    /// SSN, stored normalized digits-only (see [`normalize_ssn`]).
    /// Local store only; masked in every CLI output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssn: Option<String>,
    /// Date of birth, YYYY-MM-DD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_of_birth: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupation: Option<String>,
    /// Legally blind (a standard-deduction condition).
    #[serde(default)]
    pub blind: bool,
}

impl PersonIdentity {
    /// True when the date of birth is before the params cutoff
    /// (TY2025: born before 1961-01-02 = considered 65). ISO dates
    /// compare lexicographically; an absent or malformed DOB is not
    /// age 65.
    pub fn is_age_65(&self, params: &TaxParams) -> bool {
        match &self.date_of_birth {
            Some(dob) => valid_iso_date(dob) && dob.as_str() < params.age_65_born_before(),
            None => false,
        }
    }

    /// Number of standard-deduction conditions this person carries:
    /// age 65 (0/1) + blind (0/1).
    pub fn standard_deduction_conditions(&self, params: &TaxParams) -> u32 {
        u32::from(self.is_age_65(params)) + u32::from(self.blind)
    }

    /// Validate and normalize in place: SSNs become digits-only, DOBs
    /// must be YYYY-MM-DD. `who` ("taxpayer" / "spouse") labels errors.
    fn normalized(mut self, who: &str) -> Result<PersonIdentity, String> {
        if let Some(ssn) = &self.ssn {
            self.ssn = Some(normalize_ssn(ssn).ok_or_else(|| {
                format!("{who} SSN must be 9 digits (\"123-45-6789\" or \"123456789\")")
            })?);
        }
        if let Some(dob) = &self.date_of_birth {
            if !valid_iso_date(dob) {
                return Err(format!("{who} date_of_birth must be YYYY-MM-DD"));
            }
        }
        Ok(self)
    }
}

/// Mailing address as printed on Form 1040 page 1. Every component is
/// optional; the export fills what is present.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailingAddress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
}

/// The return-level taxpayer identity block (set with
/// `tax return set-identity`). Optional as a whole: a return without
/// it computes exactly as before, and the PDF export reports the
/// identity fields as skipped instead of filling them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxpayerIdentity {
    #[serde(flatten)]
    pub person: PersonIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<MailingAddress>,
    /// Spouse identity; meaningful for MFJ/MFS (validation warns when
    /// present for single/HoH).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spouse: Option<PersonIdentity>,
    /// Someone can claim the taxpayer as a dependent (Form 1040 page 2,
    /// line 12a checkbox). The dependent-filer standard-deduction
    /// limitation is NOT computed in v1 (validation warns).
    #[serde(default)]
    pub can_be_claimed_as_dependent: bool,
}

impl TaxpayerIdentity {
    /// Validate and normalize (SSNs digits-only, DOB shape) for both
    /// the taxpayer and the spouse.
    pub fn normalized(self) -> Result<TaxpayerIdentity, String> {
        let person = self.person.normalized("taxpayer")?;
        let spouse = match self.spouse {
            Some(s) => Some(s.normalized("spouse")?),
            None => None,
        };
        Ok(TaxpayerIdentity {
            person,
            address: self.address,
            spouse,
            can_be_claimed_as_dependent: self.can_be_claimed_as_dependent,
        })
    }
}

/// The age-65/blind addition to the standard deduction for a return:
/// per-condition amount for the filing status times the number of
/// conditions. The taxpayer's conditions always count; the spouse's
/// count only for MFJ/MFS, where a spouse block exists. No identity (or
/// no DOB) means no additions — identity is optional in v1, so line 12
/// never becomes Incomplete for a missing date of birth.
pub fn additional_standard_deduction(
    identity: Option<&TaxpayerIdentity>,
    status: FilingStatus,
    params: &TaxParams,
) -> Money {
    let Some(identity) = identity else {
        return Money::ZERO;
    };
    let mut conditions = identity.person.standard_deduction_conditions(params);
    if matches!(status, FilingStatus::Mfj | FilingStatus::Mfs) {
        if let Some(spouse) = &identity.spouse {
            conditions += spouse.standard_deduction_conditions(params);
        }
    }
    let per_condition = params.standard_deduction_addition_per_condition(status);
    Money::from_cents(per_condition.cents() * conditions as i64)
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
    #[serde(default)]
    pub f1099_necs: Vec<I1099NecInput>,
    /// Dependents claimed on the return (Schedule 8812 credits only).
    #[serde(default)]
    pub dependents: Vec<DependentInput>,
    /// Taxpayer identity (optional in v1): drives the age-65/blind
    /// standard-deduction additions on line 12 and the identity fields
    /// of the PDF export. Absent = no additions, identity fields
    /// reported skipped on export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<TaxpayerIdentity>,
}

/// A computed return: line number (Form 1040 numbering) → fact result.
///
/// In addition to numbered 1040 lines, the map carries internal keys
/// from the Schedule D combination: `schd_net_short` and `schd_net_long`
/// (net short-term / long-term gain or loss aggregated from 1099-B
/// transactions), `schd_net_capital_gain` (the QDCG worksheet's line 3
/// input), and `schd_carryover_short` / `schd_carryover_long` (capital
/// loss carryovers to the next year, as positive amounts) — plus the
/// self-employment keys `schc_net_profit` (Schedule C net profit from
/// 1099-NEC engagements, Schedule 1 line 3), `schedse_net_earnings`
/// (Schedule SE line 4a net earnings) and `sched2_se_tax` (the Schedule
/// SE tax as it enters Schedule 2 line 4, equal to line 23) — plus the
/// Schedule 8812 keys `sched8812_tentative` (line 8 tentative credit
/// before the phaseout), `sched8812_credit_after_phaseout` (line 12)
/// and `sched8812_earned_income` (Part II-A line 18a, v1 definition).
/// They are not Form 1040 line numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputedReturn {
    pub lines: BTreeMap<String, FactResult<Money>>,
    /// True only when every computed line is `Complete`.
    pub complete: bool,
    /// Human-readable simplifications that apply to this computation.
    pub notes: Vec<String>,
    /// Which method figured line 16: `"qdcg"`, `"tax_table"`, or
    /// `"tax_computation"`; `None` when line 16 is not computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line16_method: Option<String>,
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
/// (direct + Σ 1099-DIV Box 1a), 7 capital gain/loss (the Schedule D
/// combination of the 1099-B nets, with the IRC §1211(b) loss limit),
/// 8 additional income from Schedule 1 (v1: the Schedule C net profit
/// from 1099-NEC engagements only), 9 total income, 10 adjustments to
/// income (v1: the deductible part of SE tax, Schedule 1 line 15), 11 AGI
/// (9 − 10), 12 standard deduction, 15 taxable income, 16 tax,
/// 19 nonrefundable child tax credit / credit for other dependents
/// (Schedule 8812 line 14), 21 total nonrefundable credits (= 19 in v1:
/// Schedule 3 line 14 is zero), 22 tax after nonrefundable credits
/// (max(0, 16 − 21)), 23 other taxes (v1: the Schedule SE tax, via
/// Schedule 2 line 4), 24 total tax (22 + 23), 25a W-2 withholding,
/// 25d total withheld (W-2 + 1099-INT/DIV/B/NEC Box 4), 28 additional
/// child tax credit (Schedule 8812 Part II-A), 32 total other payments
/// and refundable credits (= 28 in v1), 33 total payments (25d + 32),
/// 34 overpaid (refund) when payments exceed line 24, 37 amount owed
/// otherwise. Plus the internal `schd_*` / `schc_*` / `schedse_*` /
/// `sched2_*` / `sched8812_*` keys described on [`ComputedReturn`].
///
/// Line 16 is figured by the method the 1040 instructions prescribe
/// (see [`worksheets::line16_tax`]): the Qualified Dividends and Capital
/// Gain Tax Worksheet when there is preferential income, else the Tax
/// Table below $100,000 of taxable income, else the Tax Computation
/// Worksheet. Capital loss carryovers come from the Capital Loss
/// Carryover Worksheet (Instructions for Schedule D).
///
/// Completeness: a 1099-B transaction missing proceeds or basis makes
/// lines 7, 9, 11, 15, 16, 24, 34, 37 (and the `schd_*` keys)
/// `Incomplete`; independent lines stay `Complete` — in particular the
/// self-employment lines (8, 10, 23 and the `schc_*` / `schedse_*` /
/// `sched2_*` keys) depend only on 1099-NEC and W-2 inputs. The
/// Schedule 8812 credit lines (19, 21, 22, 28, 32 — and 33 through 32)
/// depend on AGI and line 16 when dependents are stored, so they are
/// `Incomplete` then too; with no dependents stored they are a stated
/// zero (the tentative credit is zero regardless of income).
///
/// Simplifications (docs/KNOWN_LIMITS.md): the only adjustment to income
/// is the deductible part of SE tax; no itemizing, no QBI. The only
/// credits are the Schedule 8812 family credits, in the simplified
/// form of [`worksheets::schedule_8812`] (MAGI = AGI; no Schedule 3
/// nonrefundable credits; Part II-B not implemented; the earned income
/// credit is a different credit and is NOT implemented). Schedule D is
/// the simplified two-net combination (no K-1 / Forms 6252/4684 /
/// capital gain distribution components, no 28%-rate or unrecaptured
/// §1250 amounts), so the Schedule D Tax Worksheet is never triggered;
/// NIIT (Form 8960) is not figured. Schedule SE is the simplified
/// computation in [`worksheets::schedule_se_tax`], with Σ W-2 Box 1
/// wages standing in for Box 3 Social Security wages.
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
    // Schedule D combination: line 16 of Schedule D, the §1211(b)
    // loss limit on line 7, and the QDCG worksheet's line 3 input.
    let schd = if b_incomplete {
        None
    } else {
        Some(combine_schedule_d(
            net_short,
            net_long,
            input.status,
            params,
        ))
    };
    let capital_gain_f = match &schd {
        Some(c) => FactResult::Complete(c.line7),
        None => FactResult::Incomplete,
    };
    let schd_gain_f = match &schd {
        Some(c) => FactResult::Complete(c.net_capital_gain),
        None => FactResult::Incomplete,
    };

    // 1099-NEC: Box 1 − expenses → Schedule C net profit (Schedule 1
    // line 3 → Form 1040 line 8); Box 4 → 25d. Schedule SE tax is
    // figured on the aggregate net profit; Σ W-2 Box 1 wages stand in
    // for Box 3 Social Security wages (documented proxy).
    let schc_net = schedule_c_net_profit(&input.f1099_necs);
    let nec_withholding: Money = input.f1099_necs.iter().map(|f| f.federal_withholding).sum();
    let se = schedule_se_tax(schc_net, wages, params);
    let se_deduction = deductible_part_of_se_tax(se.tax);

    let line2b = input.interest_income + int_interest;
    let line3b = input.ordinary_dividends + div_ordinary;
    let total_withholding =
        w2_withholding + int_withholding + div_withholding + b_withholding + nec_withholding;

    let complete = |v: Money| FactResult::Complete(v);
    let total_income_f = add_facts(
        add_facts(complete(wages), complete(line2b)),
        add_facts(
            add_facts(complete(line3b), capital_gain_f.clone()),
            complete(schc_net),
        ),
    );
    // Line 10 (adjustments) → line 11 AGI = line 9 − line 10.
    let agi_f = add_facts(total_income_f.clone(), complete(-se_deduction));
    // Line 12: base standard deduction plus the age-65/blind additions
    // from the stored identity (zero when no identity/DOB is stored).
    let deduction = params.standard_deduction(input.status)
        + additional_standard_deduction(input.identity.as_ref(), input.status, params);
    let taxable_f = agi_f
        .clone()
        .map(|agi| Money::from_cents((agi - deduction).cents().max(0)));
    // Line 16 by the prescribed method (QDCG worksheet / Tax Table /
    // Tax Computation Worksheet); the method travels with the result.
    let schd_gain = schd
        .as_ref()
        .map(|c| c.net_capital_gain)
        .unwrap_or(Money::ZERO);
    let tax_pair_f = taxable_f
        .clone()
        .map(|taxable| line16_tax(taxable, div_qualified, schd_gain, input.status, params));
    let (tax_f, line16_method) = match tax_pair_f {
        FactResult::Complete((t, m)) => (FactResult::Complete(t), Some(m.to_string())),
        FactResult::Placeholder((t, m)) => (FactResult::Placeholder(t), Some(m.to_string())),
        FactResult::Incomplete => (FactResult::Incomplete, None),
    };
    // Capital Loss Carryover Worksheet (needs the unfloored line 15:
    // AGI minus deductions before the zero floor).
    let (carry_short_f, carry_long_f) = match (&schd, agi_f.value()) {
        (Some(c), Some(agi)) => {
            let (cs, cl) = capital_loss_carryover(c, *agi - deduction);
            (complete(cs), complete(cl))
        }
        _ => (FactResult::Incomplete, FactResult::Incomplete),
    };
    // Schedule 8812 (family credits). Earned income (Part II-A line
    // 18a, v1 definition): total W-2 wages plus the Schedule C net
    // profit when it is positive. The worksheet needs MAGI (= AGI in
    // v1) and line 16 for the Credit Limit Worksheet A, so with
    // dependents stored the credit lines inherit their completeness;
    // with none stored the tentative credit is zero whatever the
    // income, so every credit line is a stated zero.
    let earned_income = wages + Money::from_cents(schc_net.cents().max(0));
    let s8812_f: FactResult<Schedule8812> = if input.dependents.is_empty() {
        FactResult::Complete(schedule_8812(
            &[],
            input.status,
            Money::ZERO,
            Money::ZERO,
            earned_income,
            params,
        ))
    } else {
        match (agi_f.clone(), tax_f.clone()) {
            (FactResult::Complete(agi), FactResult::Complete(tax)) => {
                FactResult::Complete(schedule_8812(
                    &input.dependents,
                    input.status,
                    agi,
                    tax,
                    earned_income,
                    params,
                ))
            }
            (FactResult::Incomplete, _) | (_, FactResult::Incomplete) => FactResult::Incomplete,
            (a, b) => FactResult::Placeholder(schedule_8812(
                &input.dependents,
                input.status,
                *a.value().expect("non-incomplete fact has a value"),
                *b.value().expect("non-incomplete fact has a value"),
                earned_income,
                params,
            )),
        }
    };
    let tentative_f = s8812_f.clone().map(|s| s.tentative_credit);
    let after_phaseout_f = s8812_f.clone().map(|s| s.credit_after_phaseout);
    // Line 19 (Schedule 8812 line 14); line 21 = line 19 in v1
    // (Schedule 3 line 14, other nonrefundable credits, is zero).
    let line19_f = s8812_f.clone().map(|s| s.nonrefundable_credit);
    let line21_f = line19_f.clone();
    // Line 22 = max(0, line 16 − line 21); line 24 = line 22 + line 23.
    let line22_f = match (tax_f.clone(), line21_f.clone()) {
        (FactResult::Complete(t), FactResult::Complete(c)) => {
            FactResult::Complete(Money::from_cents((t - c).cents().max(0)))
        }
        (FactResult::Incomplete, _) | (_, FactResult::Incomplete) => FactResult::Incomplete,
        (a, b) => FactResult::Placeholder(Money::from_cents(
            (*a.value().expect("non-incomplete fact has a value")
                - *b.value().expect("non-incomplete fact has a value"))
            .cents()
            .max(0),
        )),
    };
    let total_tax_f = add_facts(line22_f.clone(), complete(se.tax));
    // Line 28 (ACTC); line 32 = line 28 in v1; line 33 = 25d + 32.
    let line28_f = s8812_f.map(|s| s.additional_child_tax_credit);
    let line32_f = line28_f.clone();
    let payments_f = add_facts(complete(total_withholding), line32_f.clone());
    // Overpayment / amount owed compare line 33 payments against
    // line 24, per the form.
    let (overpaid_f, owed_f) = match (total_tax_f.clone(), payments_f.clone()) {
        (
            FactResult::Complete(tax) | FactResult::Placeholder(tax),
            FactResult::Complete(pay) | FactResult::Placeholder(pay),
        ) => {
            let placeholder = matches!(total_tax_f, FactResult::Placeholder(_))
                || matches!(payments_f, FactResult::Placeholder(_));
            let wrap = |v: Money| {
                if placeholder {
                    FactResult::Placeholder(v)
                } else {
                    FactResult::Complete(v)
                }
            };
            if pay.cents() > tax.cents() {
                (wrap(pay - tax), wrap(Money::ZERO))
            } else {
                (wrap(Money::ZERO), wrap(tax - pay))
            }
        }
        _ => (FactResult::Incomplete, FactResult::Incomplete),
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
    lines.insert("schd_net_capital_gain".into(), schd_gain_f);
    lines.insert("schd_carryover_short".into(), carry_short_f);
    lines.insert("schd_carryover_long".into(), carry_long_f);
    lines.insert("8".into(), complete(schc_net));
    lines.insert("schc_net_profit".into(), complete(schc_net));
    lines.insert("schedse_net_earnings".into(), complete(se.net_earnings));
    lines.insert("sched2_se_tax".into(), complete(se.tax));
    lines.insert("9".into(), total_income_f);
    lines.insert("10".into(), complete(se_deduction));
    lines.insert("11".into(), agi_f);
    lines.insert("12".into(), complete(deduction));
    lines.insert("15".into(), taxable_f);
    lines.insert("16".into(), tax_f);
    lines.insert("sched8812_tentative".into(), tentative_f);
    lines.insert("sched8812_credit_after_phaseout".into(), after_phaseout_f);
    lines.insert("sched8812_earned_income".into(), complete(earned_income));
    lines.insert("19".into(), line19_f);
    lines.insert("21".into(), line21_f);
    lines.insert("22".into(), line22_f);
    lines.insert("23".into(), complete(se.tax));
    lines.insert("24".into(), total_tax_f);
    lines.insert("25a".into(), complete(w2_withholding));
    lines.insert("25d".into(), complete(total_withholding));
    lines.insert("28".into(), line28_f);
    lines.insert("32".into(), line32_f);
    lines.insert("33".into(), payments_f);
    lines.insert("34".into(), overpaid_f);
    lines.insert("37".into(), owed_f);

    let complete = lines.values().all(|f| f.is_complete());
    let line16_note = match line16_method.as_deref() {
        Some("qdcg") => "Line 16 was figured with the Qualified Dividends and \
             Capital Gain Tax Worksheet: qualified dividends (line 3a) and the \
             Schedule D capital gain are taxed at the 0%/15%/20% rates."
            .to_string(),
        Some("tax_table") => "Line 16 was figured with the TY2025 Tax Table \
             (taxable income under $100,000)."
            .to_string(),
        Some("tax_computation") => "Line 16 was figured with the Tax \
             Computation Worksheet (ordinary brackets; taxable income \
             $100,000 or more)."
            .to_string(),
        _ => "Line 16 was not figured (the return is incomplete).".to_string(),
    };
    let mut notes = vec![
        line16_note,
        "Line 7 is the Schedule D combination of the 1099-B nets; a net \
         capital loss is limited to $3,000 ($1,500 if married filing \
         separately) under IRC 1211(b), and the unused loss carries over \
         (internal keys schd_carryover_short / schd_carryover_long)."
            .to_string(),
        "The only adjustment to income (line 10) is the deductible part of \
         self-employment tax; no itemized deductions in v1. The only \
         credits are the Schedule 8812 family credits (lines 19/21 \
         nonrefundable, line 28 additional child tax credit, line 32)."
            .to_string(),
    ];
    let std_ded_addition =
        additional_standard_deduction(input.identity.as_ref(), input.status, params);
    if std_ded_addition.cents() > 0 {
        notes.push(format!(
            "Line 12 includes a {std_ded_addition} additional standard deduction \
             for age 65 / blindness conditions from the stored identity \
             (per-condition amount for filing status {} from params/2025.json).",
            input.status.as_str()
        ));
    }
    if !input.dependents.is_empty() {
        notes.push(
            "Schedule 8812 is the simplified computation: MAGI is taken \
             to be AGI, the nonrefundable credit is limited by line 16 \
             (Credit Limit Worksheet A), and the additional child tax \
             credit uses Part II-A only (the three-or-more-children \
             Part II-B formula is not implemented). The earned income \
             credit (EIC) is a different credit and is not implemented."
                .to_string(),
        );
    }
    if !input.f1099_necs.is_empty() {
        notes.push(
            "Schedule C net profit from 1099-NEC engagements flows through \
             Schedule 1 line 3 to line 8. Schedule SE tax (line 23, and \
             line 24 = 16 + 23) is figured on 92.35% of the net profit; \
             one-half of it is deducted on line 10. The Schedule SE \
             wage-base room uses W-2 Box 1 wages as a proxy for Box 3 \
             Social Security wages (docs/KNOWN_LIMITS.md)."
                .to_string(),
        );
    }
    ComputedReturn {
        complete,
        lines,
        line16_method,
        notes,
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
    /// For dependents-table entries (`dependent_*` kinds): which
    /// dependent column (1–4) this field belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependent_index: Option<u32>,
    /// For identity entries (`identity_*` kinds): which stored
    /// identity fact feeds this field — `"taxpayer.first_name"`,
    /// `"spouse.ssn"`, `"address.city"`, `"taxpayer.age_65"`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
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
    /// Identity-section values carry `"identity"` here and are told
    /// apart by `section`.
    pub line: String,
    /// Qualified AcroForm field name.
    pub field: String,
    /// The exact string to write (formatted amount, or checkbox on-state).
    pub value: String,
    /// What reports may show instead of `value` — set for SSN fields
    /// (masked `"***-**-6789"`) so the full SSN never appears in an
    /// export report; only the filled PDF carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_value: Option<String>,
    /// `"identity"` for values sourced from the stored taxpayer
    /// identity block (header fields, occupations, the standard
    /// deduction checkboxes); `None` for line-driven values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

impl FillValue {
    fn line_value(line: String, field: String, value: String) -> FillValue {
        FillValue {
            line,
            field,
            value,
            display_value: None,
            section: None,
        }
    }

    fn identity(field: String, value: String, display_value: Option<String>) -> FillValue {
        FillValue {
            line: "identity".to_string(),
            field,
            value,
            display_value,
            section: Some("identity".to_string()),
        }
    }
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
    /// Non-fatal export caveats (e.g. more dependents stored than the
    /// form's four table columns).
    pub warnings: Vec<String>,
}

/// Identity pieces the PDF export reports as skipped when NO identity
/// block is stored at all (the pre-identity behavior, unchanged). With
/// a partial identity stored, only the pieces actually absent are
/// reported, per field.
const IDENTITY_SKIPS: &[(&str, &str)] = &[
    ("taxpayer_name", "not stored in return"),
    ("taxpayer_ssn", "not stored in return"),
    ("home_address", "not stored in return"),
];

/// Resolve one identity map key against the stored identity: the text
/// to fill, or `None` when that piece is absent.
fn identity_text_value(identity: &TaxpayerIdentity, key: &str) -> Option<String> {
    let person = &identity.person;
    let spouse = identity.spouse.as_ref();
    let address = identity.address.as_ref();
    let nonempty = |o: &Option<String>| o.clone().filter(|s| !s.is_empty());
    match key {
        "taxpayer.first_name" => nonempty(&person.first_name),
        "taxpayer.last_name" => nonempty(&person.last_name),
        "taxpayer.occupation" => nonempty(&person.occupation),
        "spouse.first_name" => spouse.and_then(|s| nonempty(&s.first_name)),
        "spouse.last_name" => spouse.and_then(|s| nonempty(&s.last_name)),
        "spouse.occupation" => spouse.and_then(|s| nonempty(&s.occupation)),
        "address.street" => address.and_then(|a| nonempty(&a.street)),
        "address.apt" => address.and_then(|a| nonempty(&a.apt)),
        "address.city" => address.and_then(|a| nonempty(&a.city)),
        "address.state" => address.and_then(|a| nonempty(&a.state)),
        "address.zip" => address.and_then(|a| nonempty(&a.zip)),
        _ => None,
    }
}

/// Resolve an identity SSN map key: the normalized 9 digits to write
/// into the comb field, or `None` when absent/unparseable.
fn identity_ssn_value(identity: &TaxpayerIdentity, key: &str) -> Option<String> {
    let raw = match key {
        "taxpayer.ssn" => &identity.person.ssn,
        "spouse.ssn" => &identity.spouse.as_ref()?.ssn,
        _ => return None,
    };
    raw.as_deref().and_then(normalize_ssn)
}

/// Resolve an identity checkbox map key: whether the box is checked.
fn identity_checkbox_value(identity: &TaxpayerIdentity, key: &str, params: &TaxParams) -> bool {
    match key {
        "taxpayer.can_be_claimed_as_dependent" => identity.can_be_claimed_as_dependent,
        "taxpayer.age_65" => identity.person.is_age_65(params),
        "taxpayer.blind" => identity.person.blind,
        "spouse.age_65" => identity
            .spouse
            .as_ref()
            .map(|s| s.is_age_65(params))
            .unwrap_or(false),
        "spouse.blind" => identity.spouse.as_ref().map(|s| s.blind).unwrap_or(false),
        _ => false,
    }
}

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
    // No identity: the params only feed identity checkbox resolution,
    // which never runs; the 2025 params are the bundled set regardless.
    let params = TaxParams::load_2025().expect("bundled TY2025 params must parse");
    build_fill_values_with_dependents(lines, status, &[], None, &params)
}

/// [`build_fill_values`], plus the page-1 Dependents table for the
/// stored dependents: each dependent's name (first whitespace-separated
/// word → the (1) First name field, the remainder → the (2) Last name
/// field), the (3) SSN field when an SSN is stored for that dependent,
/// and the question-(7) credit checkbox matching the dependent's
/// Schedule 8812 classification ([`worksheets::classify_dependent`]:
/// child tax credit → on-state `"1"`, credit for other dependents →
/// `"2"`, no credit → neither box). The table has four columns: a fifth
/// or later dependent is not printed (a warning is recorded; the
/// credits are still computed for all dependents).
///
/// When a taxpayer `identity` is stored, the header fields (names,
/// SSNs, address), the page-2 occupations and the standard-deduction
/// checkboxes (12a can-be-claimed, 12d/12e age-65/blind) are filled
/// from it; pieces that are absent are reported under `skipped`
/// individually. With no identity at all, the historical three
/// aggregate skips (taxpayer_name / taxpayer_ssn / home_address) are
/// reported instead. SSN fill values carry a masked `display_value`:
/// reports must use it, never the raw digits.
pub fn build_fill_values_with_dependents(
    lines: &BTreeMap<String, FactResult<Money>>,
    status: FilingStatus,
    dependents: &[DependentInput],
    identity: Option<&TaxpayerIdentity>,
    params: &TaxParams,
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
        values.push(FillValue::line_value(
            entry.line.clone(),
            entry.qname.clone(),
            format_amount(*m),
        ));
    }
    // Filing-status checkbox for the return's status.
    for entry in &map.entries {
        if entry.kind == "checkbox" && entry.status.as_deref() == Some(status.as_str()) {
            values.push(FillValue::line_value(
                entry.line.clone(),
                entry.qname.clone(),
                entry
                    .value
                    .clone()
                    .expect("checkbox map entries carry an on-state value"),
            ));
        }
    }

    // Dependents table (page 1): names, the (3) SSN when stored, and
    // the question-(7) credit checkbox, for the first four stored
    // dependents (the form has exactly four columns).
    let mut warnings: Vec<String> = Vec::new();
    if dependents.len() > 4 {
        warnings.push(format!(
            "{} dependents are stored but the Form 1040 Dependents table has only \
             four columns; the first four are printed (credits are still computed \
             for all {})",
            dependents.len(),
            dependents.len()
        ));
    }
    let dep_entry = |kind: &str, index: u32| -> Option<&FieldMapEntry> {
        map.entries
            .iter()
            .find(|e| e.kind == kind && e.dependent_index == Some(index))
    };
    for (i, dep) in dependents.iter().take(4).enumerate() {
        let index = (i + 1) as u32;
        let mut words = dep.name.split_whitespace();
        let first = words.next().unwrap_or("");
        let last = words.collect::<Vec<_>>().join(" ");
        if !first.is_empty() {
            if let Some(e) = dep_entry("dependent_first_name", index) {
                values.push(FillValue::line_value(
                    format!("dependent_{index}_first_name"),
                    e.qname.clone(),
                    first.to_string(),
                ));
            }
        }
        if !last.is_empty() {
            if let Some(e) = dep_entry("dependent_last_name", index) {
                values.push(FillValue::line_value(
                    format!("dependent_{index}_last_name"),
                    e.qname.clone(),
                    last,
                ));
            }
        }
        if let Some(digits) = dep.ssn.as_deref().and_then(normalize_ssn) {
            if let Some(e) = dep_entry("dependent_ssn", index) {
                let mut fv = FillValue::line_value(
                    format!("dependent_{index}_ssn"),
                    e.qname.clone(),
                    digits.clone(),
                );
                fv.display_value = Some(mask_ssn(&digits));
                values.push(fv);
            }
        }
        let credit_kind = match classify_dependent(dep) {
            DependentCreditClass::ChildTaxCredit => Some("dependent_credit_ctc"),
            DependentCreditClass::OtherDependent => Some("dependent_credit_odc"),
            DependentCreditClass::None => None,
        };
        if let Some(kind) = credit_kind {
            if let Some(e) = dep_entry(kind, index) {
                values.push(FillValue::line_value(
                    format!("dependent_{index}_credit"),
                    e.qname.clone(),
                    e.value
                        .clone()
                        .expect("dependent credit map entries carry an on-state value"),
                ));
            }
        }
    }

    // Identity section: header fields, occupations, and the standard
    // deduction checkboxes, from the stored identity block.
    let mut skipped: Vec<SkippedField> = Vec::new();
    match identity {
        None => {
            skipped.extend(IDENTITY_SKIPS.iter().map(|(field, reason)| SkippedField {
                field: field.to_string(),
                reason: reason.to_string(),
            }));
        }
        Some(identity) => {
            for entry in &map.entries {
                let Some(key) = entry.key.as_deref() else {
                    continue;
                };
                // Spouse pieces are reported/filled only when a spouse
                // is relevant: MFJ/MFS, or a spouse block is stored.
                let spouse_piece = key.starts_with("spouse.");
                if spouse_piece
                    && !matches!(status, FilingStatus::Mfj | FilingStatus::Mfs)
                    && identity.spouse.is_none()
                {
                    continue;
                }
                match entry.kind.as_str() {
                    "identity_text" => match identity_text_value(identity, key) {
                        Some(v) => values.push(FillValue::identity(entry.qname.clone(), v, None)),
                        None => {
                            // An absent apartment number is ordinary,
                            // not a gap worth reporting.
                            if key != "address.apt" {
                                skipped.push(SkippedField {
                                    field: key.replace('.', "_"),
                                    reason: "not stored in return".to_string(),
                                });
                            }
                        }
                    },
                    "identity_ssn" => match identity_ssn_value(identity, key) {
                        Some(digits) => values.push(FillValue::identity(
                            entry.qname.clone(),
                            digits.clone(),
                            Some(mask_ssn(&digits)),
                        )),
                        None => skipped.push(SkippedField {
                            field: key.replace('.', "_"),
                            reason: "not stored in return".to_string(),
                        }),
                    },
                    "identity_checkbox" if identity_checkbox_value(identity, key, params) => {
                        values.push(FillValue::identity(
                            entry.qname.clone(),
                            entry
                                .value
                                .clone()
                                .expect("identity checkbox entries carry an on-state"),
                            None,
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    // A dependent among the first four without a stored SSN leaves the
    // (3) SSN cell blank; report it once, as before.
    if dependents
        .iter()
        .take(4)
        .any(|d| d.ssn.as_deref().and_then(normalize_ssn).is_none())
    {
        skipped.push(SkippedField {
            field: "dependent_ssn".to_string(),
            reason: "not stored in return".to_string(),
        });
    }
    Ok(FillPlan {
        values,
        skipped,
        warnings,
    })
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
            f1099_necs: vec![],
            dependents: vec![],
            identity: None,
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
        // No preferential income and taxable < $100,000: the Tax Table
        // applies — row 34,250-34,300, single column = $3,875.
        assert_eq!(line("16"), 387_500, "tax $3,875 (Tax Table)");
        assert_eq!(r.line16_method.as_deref(), Some("tax_table"));
        assert_eq!(line("25d"), 400_000);
        assert_eq!(line("34"), 12_500, "refund $125.00");
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
        // Tax Table tax $3,875 - $3,000 withheld = $875 owed.
        assert_eq!(r.lines["37"].value().copied().unwrap().cents(), 87_500);
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
    /// 53000 − 15750 = $37,250. Line 16 by the QDCG worksheet: Schedule D
    /// line 15 is a $500 loss, so the QDCG capital gain (line 3) is 0 and
    /// the only preferential income is the $600 of qualified dividends,
    /// taxed at 0% (taxable is under the $48,350 threshold); the tax is
    /// the Tax Table on the $36,650 ordinary part = $4,163; owed
    /// 4163 − 4000 = $163.
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
        assert_eq!(line("schd_net_capital_gain"), 0, "Sch D line 15 is a loss");
        assert_eq!(line("schd_carryover_short"), 0);
        assert_eq!(line("schd_carryover_long"), 0);
        assert_eq!(line("7"), 150_000, "net capital gain $1,500");
        assert_eq!(line("9"), 5_300_000, "total income $53,000");
        assert_eq!(line("11"), 5_300_000, "AGI $53,000");
        assert_eq!(line("12"), 1_575_000);
        assert_eq!(line("15"), 3_725_000, "taxable $37,250");
        assert_eq!(line("16"), 416_300, "tax $4,163.00 (QDCG worksheet)");
        assert_eq!(r.line16_method.as_deref(), Some("qdcg"));
        assert_eq!(line("25a"), 400_000);
        assert_eq!(line("25d"), 400_000);
        assert_eq!(line("33"), 400_000);
        assert_eq!(line("34"), 0, "nothing overpaid");
        assert_eq!(line("37"), 16_300, "amount owed $163.00");
    }

    fn case003_input() -> Form1040Input {
        let mut input = golden_input();
        input.w2s[0].wages = Money::from_cents(6_000_000);
        input.w2s[0].federal_withholding = Money::from_cents(600_000);
        input.f1099_bs = vec![I1099BInput {
            payer_name: "Example Broker".into(),
            payer_ein: None,
            transactions: vec![
                BrokerTxn {
                    description: "100 sh EXAMPLE".into(),
                    proceeds: Some(Money::from_cents(400_000)),
                    cost_basis: Some(Money::from_cents(1_000_000)),
                    adjustment: Money::ZERO,
                    term: TxnTerm::Short,
                },
                BrokerTxn {
                    description: "50 sh EXAMPLE".into(),
                    proceeds: Some(Money::from_cents(600_000)),
                    cost_basis: Some(Money::from_cents(1_000_000)),
                    adjustment: Money::ZERO,
                    term: TxnTerm::Long,
                },
            ],
            federal_withholding: Money::ZERO,
        }];
        input
    }

    /// Golden case 003 (benchmark/cases/f1040/2025/003-capital-loss-limit):
    /// ST loss −$6,000 and LT loss −$4,000 combine to −$10,000; line 7
    /// takes only the $3,000 §1211(b) allowance; the Capital Loss
    /// Carryover Worksheet consumes the allowance against the short-term
    /// loss first, carrying ST $3,000 / LT $4,000. Taxable $41,250 →
    /// Tax Table.
    #[test]
    fn golden_capital_loss_limit() {
        let params = TaxParams::load_2025().unwrap();
        let r = compute_1040(&case003_input(), &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("schd_net_short"), -600_000);
        assert_eq!(line("schd_net_long"), -400_000);
        assert_eq!(line("7"), -300_000, "loss limited to $3,000");
        assert_eq!(line("schd_carryover_short"), 300_000);
        assert_eq!(line("schd_carryover_long"), 400_000);
        assert_eq!(line("9"), 5_700_000, "total income $57,000");
        assert_eq!(line("11"), 5_700_000);
        assert_eq!(line("15"), 4_125_000, "taxable $41,250");
        // Tax Table row 41,250-41,300, single = $4,715 (asserted from
        // the extracted table; see benchmark case 003 _sources).
        assert_eq!(line("16"), 471_500, "tax $4,715 (Tax Table)");
        assert_eq!(r.line16_method.as_deref(), Some("tax_table"));
        assert_eq!(line("34"), 128_500, "refund $1,285");
        assert_eq!(line("37"), 0);
    }

    fn case004_input() -> Form1040Input {
        let mut input = golden_input();
        input.w2s[0].wages = Money::from_cents(8_000_000);
        input.w2s[0].federal_withholding = Money::from_cents(900_000);
        input.f1099_divs = vec![I1099DivInput {
            payer_name: "Example Fund".into(),
            payer_ein: None,
            total_ordinary_dividends: Money::from_cents(500_000),
            qualified_dividends: Money::from_cents(500_000),
            federal_withholding: Money::ZERO,
        }];
        input.f1099_bs = vec![I1099BInput {
            payer_name: "Example Broker".into(),
            payer_ein: None,
            transactions: vec![BrokerTxn {
                description: "100 sh EXAMPLE".into(),
                proceeds: Some(Money::from_cents(1_600_000)),
                cost_basis: Some(Money::from_cents(600_000)),
                adjustment: Money::ZERO,
                term: TxnTerm::Long,
            }],
            federal_withholding: Money::ZERO,
        }];
        input
    }

    /// Golden case 004 (benchmark/cases/f1040/2025/004-qdcg-showcase):
    /// preferential income $15,000 ($5,000 qualified dividends + $10,000
    /// LT gain) on taxable $79,250. Ordinary part $64,250 already exceeds
    /// the $48,350 zero-rate threshold, so all $15,000 is taxed at 15%:
    /// Tax Table on $64,250 ($9,055) + $2,250 = $11,305; owed $2,305.
    #[test]
    fn golden_qdcg_showcase() {
        let params = TaxParams::load_2025().unwrap();
        let r = compute_1040(&case004_input(), &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("3a"), 500_000);
        assert_eq!(line("3b"), 500_000);
        assert_eq!(line("schd_net_long"), 1_000_000);
        assert_eq!(line("schd_net_capital_gain"), 1_000_000);
        assert_eq!(line("7"), 1_000_000);
        assert_eq!(line("9"), 9_500_000, "total income $95,000");
        assert_eq!(line("11"), 9_500_000);
        assert_eq!(line("15"), 7_925_000, "taxable $79,250");
        assert_eq!(line("16"), 1_130_500, "tax $11,305.00 (QDCG worksheet)");
        assert_eq!(r.line16_method.as_deref(), Some("qdcg"));
        assert_eq!(line("34"), 0);
        assert_eq!(line("37"), 230_500, "amount owed $2,305.00");
    }

    fn case005_input() -> Form1040Input {
        let mut input = golden_input();
        input.f1099_necs = vec![I1099NecInput {
            payer_name: "Example Client LLC".into(),
            payer_ein: None,
            nonemployee_compensation: Money::from_cents(2_000_000),
            federal_withholding: Money::ZERO,
            expenses: Money::from_cents(200_000),
        }];
        input
    }

    /// Golden case 005 (benchmark/cases/f1040/2025/005-w2-nec): W-2
    /// $50,000 plus one 1099-NEC engagement (Box 1 $20,000, expenses
    /// $2,000). Hand-derived (see the case's _sources): Sch C net
    /// $18,000 → line 8; SE earnings 18000 × 0.9235 = $16,623.00;
    /// SS part 16623 × 12.4% = $2,061.25 (W-2 proxy wages $50,000 leave
    /// the full earnings under the $176,100 base); Medicare 16623 ×
    /// 2.9% = $482.07; SE tax $2,543.32 → line 23; half = $1,271.66 →
    /// line 10; AGI $66,728.34; taxable $50,978.34 → Tax Table row
    /// 50,950–51,000 single = $6,129 (line 16); line 24 = $8,672.32;
    /// owed 8672.32 − 4000 = $4,672.32.
    #[test]
    fn golden_w2_with_nec() {
        let params = TaxParams::load_2025().unwrap();
        let r = compute_1040(&case005_input(), &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("1a"), 5_000_000);
        assert_eq!(line("schc_net_profit"), 1_800_000, "Sch C net $18,000");
        assert_eq!(line("8"), 1_800_000, "line 8 = Schedule 1 business income");
        assert_eq!(line("9"), 6_800_000, "total income $68,000");
        assert_eq!(
            line("schedse_net_earnings"),
            1_662_300,
            "SE earnings $16,623.00"
        );
        assert_eq!(line("sched2_se_tax"), 254_332);
        assert_eq!(line("23"), 254_332, "SE tax $2,543.32");
        assert_eq!(line("10"), 127_166, "deductible half of SE tax $1,271.66");
        assert_eq!(line("11"), 6_672_834, "AGI $66,728.34");
        assert_eq!(line("12"), 1_575_000);
        assert_eq!(line("15"), 5_097_834, "taxable $50,978.34");
        assert_eq!(line("16"), 612_900, "tax $6,129.00 (Tax Table)");
        assert_eq!(r.line16_method.as_deref(), Some("tax_table"));
        assert_eq!(line("24"), 867_232, "total tax $8,672.32 = 16 + 23");
        assert_eq!(line("25a"), 400_000);
        assert_eq!(line("25d"), 400_000);
        assert_eq!(line("33"), 400_000);
        assert_eq!(line("34"), 0, "nothing overpaid");
        assert_eq!(line("37"), 467_232, "amount owed $4,672.32");
    }

    #[test]
    fn case005_json_round_trip() {
        let json = r#"{
            "status": "single",
            "w2s": [{"employer_ein": "12-3456789", "wages": "5000000", "federal_withholding": "400000"}],
            "interest_income": "0",
            "ordinary_dividends": "0",
            "f1099_necs": [{"payer_name": "Example Client LLC", "nonemployee_compensation": "2000000", "federal_withholding": "0", "expenses": "200000"}]
        }"#;
        let input: Form1040Input = serde_json::from_str(json).unwrap();
        assert_eq!(input, case005_input());
    }

    #[test]
    fn nec_loss_flows_to_line_8_with_no_se_tax() {
        let params = TaxParams::load_2025().unwrap();
        let mut input = golden_input();
        input.f1099_necs = vec![I1099NecInput {
            payer_name: "Client".into(),
            payer_ein: None,
            nonemployee_compensation: Money::from_cents(500_000),
            federal_withholding: Money::ZERO,
            expenses: Money::from_cents(800_000),
        }];
        let r = compute_1040(&input, &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("schc_net_profit"), -300_000, "Sch C loss -$3,000");
        assert_eq!(line("8"), -300_000, "the loss flows to line 8 in full");
        assert_eq!(line("schedse_net_earnings"), 0, "no SE earnings on a loss");
        assert_eq!(line("23"), 0, "no SE tax on a loss");
        assert_eq!(line("10"), 0);
        assert_eq!(line("9"), 4_700_000, "total income reduced by the loss");
        assert_eq!(line("24"), line("16"), "line 24 = 16 when SE tax is 0");
    }

    #[test]
    fn nec_multiple_engagements_aggregate() {
        let params = TaxParams::load_2025().unwrap();
        let mut input = golden_input();
        input.f1099_necs = vec![
            I1099NecInput {
                payer_name: "Client A".into(),
                payer_ein: None,
                nonemployee_compensation: Money::from_cents(1_000_000),
                federal_withholding: Money::from_cents(10_000),
                expenses: Money::from_cents(100_000),
            },
            I1099NecInput {
                payer_name: "Client B".into(),
                payer_ein: None,
                nonemployee_compensation: Money::from_cents(500_000),
                federal_withholding: Money::ZERO,
                expenses: Money::from_cents(50_000),
            },
        ];
        let r = compute_1040(&input, &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        // Nets: (10000 - 1000) + (5000 - 500) = $13,500.
        assert_eq!(line("schc_net_profit"), 1_350_000);
        assert_eq!(line("8"), 1_350_000);
        // SE earnings: 13500 x 0.9235 = $12,467.25.
        assert_eq!(line("schedse_net_earnings"), 1_246_725);
        // NEC Box 4 withholding joins line 25d.
        assert_eq!(line("25d"), 410_000);
    }

    #[test]
    fn nec_se_tax_capped_by_w2_wages_over_base() {
        let params = TaxParams::load_2025().unwrap();
        let mut input = case005_input();
        input.w2s[0].wages = Money::from_cents(20_000_000); // $200,000 > base
        let r = compute_1040(&input, &params);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        // SS room exhausted by the W-2 (Box 1 proxy): only Medicare.
        assert_eq!(line("23"), 48_207, "Medicare part only, $482.07");
        assert_eq!(line("10"), 24_104, "half of $482.07, rounded half up");
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
            "schd_net_capital_gain",
            "schd_carryover_short",
            "schd_carryover_long",
            "9",
            "11",
            "15",
            "16",
            "24",
            "34",
            "37",
        ] {
            assert_eq!(
                r.lines[line],
                FactResult::Incomplete,
                "line {line} must be incomplete"
            );
        }
        // Independent lines stay complete and exact — including the
        // self-employment lines, which depend only on NEC/W-2 inputs.
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("1a"), 5_000_000);
        assert_eq!(line("2b"), 50_000);
        assert_eq!(line("3a"), 60_000);
        assert_eq!(line("3b"), 100_000);
        assert_eq!(line("8"), 0);
        assert_eq!(line("10"), 0);
        assert_eq!(line("12"), 1_575_000);
        assert_eq!(line("23"), 0);
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
        // 28 mapped amount fields: 1a 1z 2a 2b 3a 3b 7 8 9 10 11a 11b 12e
        // 14 15 16 19 21 22 23 24 25a 25d 28 32 33 34 37 — engine line 11
        // feeds both 11a and 11b, line 12 feeds 12e and 14, and line 24
        // is fed by the engine's own line 24 (= 22 + 23).
        assert_eq!(amounts.len(), 28);
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
        assert_eq!(src("24").as_deref(), Some("24"));
        assert_eq!(src("14").as_deref(), Some("12"));
    }

    /// A complete lines map with benign values; 34/37 set per test.
    fn complete_lines(overpaid: i64, owed: i64) -> BTreeMap<String, FactResult<Money>> {
        let mut lines = BTreeMap::new();
        for l in [
            "1a", "1z", "2a", "2b", "3a", "3b", "7", "8", "9", "10", "11", "12", "15", "16", "19",
            "21", "22", "23", "24", "25a", "25d", "28", "32", "33",
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

    fn dependent(
        name: &str,
        relationship: DependentRelationship,
        age: u32,
        months: u32,
    ) -> DependentInput {
        DependentInput {
            name: name.into(),
            ssn: None,
            relationship,
            age,
            months_lived_with_taxpayer: months,
        }
    }

    fn case006_input() -> Form1040Input {
        let mut input = golden_input();
        input.status = FilingStatus::Mfj;
        input.w2s[0].wages = Money::from_cents(9_000_000);
        input.w2s[0].federal_withholding = Money::from_cents(800_000);
        input.dependents = vec![
            dependent("Kid One", DependentRelationship::Child, 7, 12),
            dependent("Kid Two", DependentRelationship::Child, 4, 12),
            dependent("Relative Three", DependentRelationship::Other, 19, 12),
        ];
        input
    }

    /// Golden case 006 (benchmark/cases/f1040/2025/006-family-mfj):
    /// MFJ, W-2 $90,000 / $8,000 withheld; children 7 and 4 (CTC) and an
    /// other dependent 19 (ODC). Hand-derived in the case's _sources:
    /// taxable $58,500 → Tax Table MFJ $6,546; tentative credit
    /// 2 × $2,200 + $500 = $4,900, no phaseout (MAGI < $400,000),
    /// nonrefundable = min(4900, 6546) = $4,900; line 22 = $1,646;
    /// ACTC $0 (fully absorbed); refund $6,354.
    #[test]
    fn golden_family_mfj() {
        let params = TaxParams::load_2025().unwrap();
        let r = compute_1040(&case006_input(), &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("11"), 9_000_000, "AGI $90,000");
        assert_eq!(line("15"), 5_850_000, "taxable $58,500");
        assert_eq!(line("16"), 654_600, "tax $6,546 (Tax Table MFJ)");
        assert_eq!(r.line16_method.as_deref(), Some("tax_table"));
        assert_eq!(line("sched8812_tentative"), 490_000);
        assert_eq!(line("sched8812_credit_after_phaseout"), 490_000);
        assert_eq!(line("sched8812_earned_income"), 9_000_000);
        assert_eq!(line("19"), 490_000, "nonrefundable CTC/ODC $4,900");
        assert_eq!(line("21"), 490_000);
        assert_eq!(line("22"), 164_600, "6546 - 4900");
        assert_eq!(line("23"), 0);
        assert_eq!(line("24"), 164_600, "total tax = 22 + 23");
        assert_eq!(line("28"), 0, "no ACTC: credit fully absorbed");
        assert_eq!(line("32"), 0);
        assert_eq!(line("33"), 800_000, "25d + 32");
        assert_eq!(line("34"), 635_400, "refund $6,354");
        assert_eq!(line("37"), 0);
    }

    fn case007_input() -> Form1040Input {
        let mut input = golden_input();
        input.status = FilingStatus::Hoh;
        input.w2s[0].wages = Money::from_cents(3_000_000);
        input.w2s[0].federal_withholding = Money::from_cents(100_000);
        input.dependents = vec![dependent("Kid One", DependentRelationship::Child, 5, 12)];
        input
    }

    /// Golden case 007 (benchmark/cases/f1040/2025/007-actc-hoh): HoH,
    /// W-2 $30,000 / $1,000 withheld, one child age 5. Hand-derived in
    /// the case's _sources: taxable $6,375 → Tax Table HoH $638;
    /// tentative CTC $2,200; nonrefundable limited to $638 (Credit
    /// Limit Worksheet A = line 16); ACTC = 2200 − 638 = $1,562 (under
    /// the $1,700 cap and the $4,125 earned-income measure); refund
    /// = $1,000 withholding + $1,562 ACTC = $2,562.
    #[test]
    fn golden_actc_hoh() {
        let params = TaxParams::load_2025().unwrap();
        let r = compute_1040(&case007_input(), &params);
        assert!(r.complete);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("12"), 2_362_500, "HoH standard deduction $23,625");
        assert_eq!(line("15"), 637_500, "taxable $6,375");
        assert_eq!(line("16"), 63_800, "tax $638 (Tax Table HoH)");
        assert_eq!(line("sched8812_tentative"), 220_000);
        assert_eq!(line("19"), 63_800, "nonrefundable limited to line 16");
        assert_eq!(line("21"), 63_800);
        assert_eq!(line("22"), 0);
        assert_eq!(line("24"), 0);
        assert_eq!(line("28"), 156_200, "ACTC $1,562");
        assert_eq!(line("32"), 156_200);
        assert_eq!(line("33"), 256_200, "25d 1000 + 32 1562");
        assert_eq!(line("34"), 256_200, "refund $2,562");
        assert_eq!(line("37"), 0);
    }

    #[test]
    fn case006_json_round_trip() {
        // Note: case 006's input omits months_lived_with_taxpayer, which
        // must default to 12.
        let json = r#"{
            "status": "mfj",
            "w2s": [{"employer_ein": "12-3456789", "wages": "9000000", "federal_withholding": "800000"}],
            "interest_income": "0",
            "ordinary_dividends": "0",
            "dependents": [
                {"name": "Kid One", "relationship": "child", "age": 7},
                {"name": "Kid Two", "relationship": "child", "age": 4},
                {"name": "Relative Three", "relationship": "other", "age": 19}
            ]
        }"#;
        let input: Form1040Input = serde_json::from_str(json).unwrap();
        assert_eq!(input, case006_input());
    }

    #[test]
    fn no_dependents_keeps_pre_credit_behavior() {
        // Cases 001-005 have no dependents: lines 19/21/28/32 are stated
        // zeros, 22 = 16, 24 = 16 + 23, 33 = 25d — the pre-8812 wiring.
        let params = TaxParams::load_2025().unwrap();
        let r = compute_1040(&golden_input(), &params);
        let line = |n: &str| r.lines[n].value().copied().unwrap().cents();
        assert_eq!(line("sched8812_tentative"), 0);
        assert_eq!(line("sched8812_credit_after_phaseout"), 0);
        assert_eq!(line("19"), 0);
        assert_eq!(line("21"), 0);
        assert_eq!(line("22"), line("16"));
        assert_eq!(line("24"), line("16") + line("23"));
        assert_eq!(line("28"), 0);
        assert_eq!(line("32"), 0);
        assert_eq!(line("33"), line("25d"));
    }

    #[test]
    fn dependents_with_incomplete_1099b_poison_credit_lines() {
        let params = TaxParams::load_2025().unwrap();
        let mut input = case002_input();
        input.dependents = vec![dependent("Kid One", DependentRelationship::Child, 5, 12)];
        input.f1099_bs[0].transactions[0].cost_basis = None;
        let r = compute_1040(&input, &params);
        assert!(!r.complete);
        for line in ["19", "21", "22", "24", "28", "32", "33", "34", "37"] {
            assert_eq!(
                r.lines[line],
                FactResult::Incomplete,
                "line {line} must be incomplete"
            );
        }
        // Earned income depends only on W-2/NEC entries: still exact.
        assert_eq!(
            r.lines["sched8812_earned_income"].value().copied().unwrap(),
            Money::from_cents(5_000_000)
        );
    }

    #[test]
    fn dependent_validation_findings() {
        let f = validate_dependent(&dependent("A", DependentRelationship::Child, 17, 12));
        assert!(f
            .iter()
            .any(|x| x.code == "dependent_ctc_age" && x.severity == "info"));
        let f = validate_dependent(&dependent("B", DependentRelationship::Child, 10, 6));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, "dependent_residency");
        assert_eq!(f[0].severity, "warning");
        let f = validate_dependent(&dependent("C", DependentRelationship::Other, 40, 12));
        assert!(f
            .iter()
            .any(|x| x.code == "dependent_odc_only" && x.severity == "info"));
        // A CTC-eligible child without an SSN: the SSN warning (v1
        // computes anyway; a real filing would deny the credit).
        let f = validate_dependent(&dependent("D", DependentRelationship::Child, 10, 12));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, "dependent_ctc_ssn");
        assert_eq!(f[0].severity, "warning");
        assert!(f[0]
            .message
            .contains("CTC requires the child to have an SSN"));
        // With an SSN stored, an ordinary qualifying child raises nothing.
        let mut with_ssn = dependent("E", DependentRelationship::Child, 10, 12);
        with_ssn.ssn = Some("123456789".to_string());
        assert!(validate_dependent(&with_ssn).is_empty());
    }

    #[test]
    fn fill_plan_dependents_table() {
        let params = TaxParams::load_2025().unwrap();
        let input = case006_input();
        let r = compute_1040(&input, &params);
        let plan = build_fill_values_with_dependents(
            &r.lines,
            FilingStatus::Mfj,
            &input.dependents,
            None,
            &params,
        )
        .unwrap();
        let get = |l: &str| plan.values.iter().find(|v| v.line == l);
        // Names: first word -> (1) first name, remainder -> (2) last name.
        assert_eq!(get("dependent_1_first_name").unwrap().value, "Kid");
        assert_eq!(get("dependent_1_last_name").unwrap().value, "One");
        assert!(get("dependent_1_first_name")
            .unwrap()
            .field
            .ends_with("Row1[0].f1_31[0]"));
        // Credit checkboxes: children -> CTC on-state "1"; the age-19
        // other dependent -> ODC on-state "2", on the Row7 fields.
        let c1 = get("dependent_1_credit").unwrap();
        assert_eq!(c1.value, "1");
        assert!(c1.field.ends_with("Dependent1[0].c1_28[0]"));
        let c3 = get("dependent_3_credit").unwrap();
        assert_eq!(c3.value, "2");
        assert!(c3.field.ends_with("Dependent3[0].c1_30[1]"));
        // Credit amount lines are part of the plan too.
        assert_eq!(
            plan.values.iter().find(|v| v.line == "19").unwrap().value,
            "4900.00"
        );
        assert_eq!(
            plan.values.iter().find(|v| v.line == "22").unwrap().value,
            "1646.00"
        );
        // Dependent SSNs are reported skipped, not invented.
        assert!(plan
            .skipped
            .iter()
            .any(|s| s.field == "dependent_ssn" && s.reason == "not stored in return"));
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn fill_plan_dependents_over_four_and_no_credit() {
        let params = TaxParams::load_2025().unwrap();
        let mut input = case007_input();
        input.dependents = vec![
            dependent("A One", DependentRelationship::Child, 5, 12),
            dependent("B Two", DependentRelationship::Child, 6, 12),
            dependent("C Three", DependentRelationship::Child, 7, 12),
            dependent("D Four", DependentRelationship::Child, 8, 12),
            dependent("E Five", DependentRelationship::Child, 9, 12),
            dependent("F Six", DependentRelationship::Child, 3, 4),
        ];
        let r = compute_1040(&input, &params);
        let plan = build_fill_values_with_dependents(
            &r.lines,
            FilingStatus::Hoh,
            &input.dependents,
            None,
            &params,
        )
        .unwrap();
        // Only the first four columns are printed; the warning says so.
        assert!(plan
            .values
            .iter()
            .any(|v| v.line == "dependent_4_first_name"));
        assert!(!plan
            .values
            .iter()
            .any(|v| v.line.starts_with("dependent_5")));
        assert!(!plan
            .values
            .iter()
            .any(|v| v.line.starts_with("dependent_6")));
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains("four columns"));
        // A residency-failing dependent (the sixth here is beyond the
        // table; use the same shape within four) gets a name but no
        // credit box.
        let mut input2 = case007_input();
        input2.dependents = vec![dependent("Short Stay", DependentRelationship::Child, 5, 4)];
        let r2 = compute_1040(&input2, &params);
        let plan2 = build_fill_values_with_dependents(
            &r2.lines,
            FilingStatus::Hoh,
            &input2.dependents,
            None,
            &params,
        )
        .unwrap();
        assert!(plan2
            .values
            .iter()
            .any(|v| v.line == "dependent_1_first_name"));
        assert!(!plan2.values.iter().any(|v| v.line == "dependent_1_credit"));
    }

    // -- Identity / age-65-blind standard deduction -------------------

    fn person(dob: Option<&str>, blind: bool) -> PersonIdentity {
        PersonIdentity {
            date_of_birth: dob.map(str::to_string),
            blind,
            ..Default::default()
        }
    }

    #[test]
    fn ssn_normalize_and_mask() {
        assert_eq!(normalize_ssn("123-45-6789").as_deref(), Some("123456789"));
        assert_eq!(normalize_ssn("123456789").as_deref(), Some("123456789"));
        assert_eq!(normalize_ssn("123-45-678"), None);
        assert_eq!(normalize_ssn("12345678a"), None);
        assert_eq!(mask_ssn("123456789"), "***-**-6789");
        assert_eq!(mask_ssn("123-45-6789"), "***-**-6789");
        assert_eq!(mask_ssn("garbage"), "***-**-****");
    }

    #[test]
    fn std_ded_addition_amounts() {
        let params = TaxParams::load_2025().unwrap();
        // No identity -> no additions.
        assert_eq!(
            additional_standard_deduction(None, FilingStatus::Single, &params),
            Money::ZERO
        );
        // Single, 65+ (born 1960-06-15): +$2,000 (TY2025 unmarried amount).
        let id = TaxpayerIdentity {
            person: person(Some("1960-06-15"), false),
            ..Default::default()
        };
        assert_eq!(
            additional_standard_deduction(Some(&id), FilingStatus::Single, &params).cents(),
            200_000
        );
        // Cutoff boundary: born 1961-01-01 is 65; born 1961-01-02 is not.
        let id = TaxpayerIdentity {
            person: person(Some("1961-01-01"), false),
            ..Default::default()
        };
        assert_eq!(
            additional_standard_deduction(Some(&id), FilingStatus::Hoh, &params).cents(),
            200_000
        );
        let id = TaxpayerIdentity {
            person: person(Some("1961-01-02"), false),
            ..Default::default()
        };
        assert_eq!(
            additional_standard_deduction(Some(&id), FilingStatus::Single, &params),
            Money::ZERO
        );
        // MFJ, both spouses both conditions: 4 x $1,600 = $6,400.
        let id = TaxpayerIdentity {
            person: person(Some("1955-01-01"), true),
            spouse: Some(person(Some("1958-01-01"), true)),
            ..Default::default()
        };
        assert_eq!(
            additional_standard_deduction(Some(&id), FilingStatus::Mfj, &params).cents(),
            640_000
        );
        // DOB absent -> no age addition (blind still counts).
        let id = TaxpayerIdentity {
            person: person(None, true),
            ..Default::default()
        };
        assert_eq!(
            additional_standard_deduction(Some(&id), FilingStatus::Single, &params).cents(),
            200_000
        );
        let id = TaxpayerIdentity::default();
        assert_eq!(
            additional_standard_deduction(Some(&id), FilingStatus::Single, &params),
            Money::ZERO
        );
        // Spouse conditions are ignored for single (no spouse counts).
        let id = TaxpayerIdentity {
            person: person(None, false),
            spouse: Some(person(Some("1950-01-01"), true)),
            ..Default::default()
        };
        assert_eq!(
            additional_standard_deduction(Some(&id), FilingStatus::Single, &params),
            Money::ZERO
        );
    }

    #[test]
    fn line12_includes_age_blind_additions() {
        let params = TaxParams::load_2025().unwrap();
        // Single filer, 65+: line 12 = 15,750 + 2,000.
        let mut input = golden_input();
        input.identity = Some(TaxpayerIdentity {
            person: person(Some("1959-03-01"), false),
            ..Default::default()
        });
        let r = compute_1040(&input, &params);
        assert_eq!(r.lines["12"].value().copied().unwrap().cents(), 1_775_000);
        // The golden input without identity keeps the base amount.
        let r = compute_1040(&golden_input(), &params);
        assert_eq!(r.lines["12"].value().copied().unwrap().cents(), 1_575_000);
    }

    #[test]
    fn fill_plan_identity_section() {
        let params = TaxParams::load_2025().unwrap();
        let mut input = case006_input();
        input.identity = Some(TaxpayerIdentity {
            person: PersonIdentity {
                first_name: Some("Spike".into()),
                last_name: Some("Tester".into()),
                ssn: Some("123456789".into()),
                date_of_birth: Some("1958-05-01".into()),
                occupation: Some("Engineer".into()),
                blind: false,
            },
            address: Some(MailingAddress {
                street: Some("123 Main St".into()),
                apt: None,
                city: Some("Springfield".into()),
                state: Some("IL".into()),
                zip: Some("62704".into()),
            }),
            spouse: Some(PersonIdentity {
                first_name: Some("Partner".into()),
                last_name: Some("Tester".into()),
                ssn: Some("987654321".into()),
                date_of_birth: Some("1960-11-15".into()),
                occupation: None,
                blind: true,
            }),
            can_be_claimed_as_dependent: false,
        });
        input.dependents[0].ssn = Some("111223333".into());
        let r = compute_1040(&input, &params);
        let plan = build_fill_values_with_dependents(
            &r.lines,
            FilingStatus::Mfj,
            &input.dependents,
            input.identity.as_ref(),
            &params,
        )
        .unwrap();
        let by_field = |suffix: &str| {
            plan.values
                .iter()
                .find(|v| v.field.ends_with(suffix))
                .unwrap_or_else(|| panic!("no fill value for field {suffix}"))
        };
        // Header fields.
        assert_eq!(by_field("f1_14[0]").value, "Spike");
        assert_eq!(by_field("f1_15[0]").value, "Tester");
        assert_eq!(by_field("f1_17[0]").value, "Partner");
        assert_eq!(by_field("f1_22[0]").value, "Springfield");
        assert_eq!(by_field("f2_40[0]").value, "Engineer");
        // SSN fields: raw digits for the PDF, masked display value,
        // identity section tag.
        let ssn = by_field("f1_16[0]");
        assert_eq!(ssn.value, "123456789");
        assert_eq!(ssn.display_value.as_deref(), Some("***-**-6789"));
        assert_eq!(ssn.section.as_deref(), Some("identity"));
        assert_eq!(
            by_field("f1_19[0]").display_value.as_deref(),
            Some("***-**-4321")
        );
        // Age/blind checkboxes: taxpayer 65 (c2_5), spouse 65 (c2_7)
        // and spouse blind (c2_8) checked; taxpayer blind (c2_6) not.
        assert_eq!(by_field("c2_5[0]").value, "1");
        assert_eq!(by_field("c2_7[0]").value, "1");
        assert_eq!(by_field("c2_8[0]").value, "1");
        assert!(!plan.values.iter().any(|v| v.field.ends_with("c2_6[0]")));
        // Dependent 1 SSN filled masked; dependents 2/3 have none, so
        // the aggregate dependent_ssn skip remains.
        let dssn = plan
            .values
            .iter()
            .find(|v| v.line == "dependent_1_ssn")
            .unwrap();
        assert_eq!(dssn.value, "111223333");
        assert_eq!(dssn.display_value.as_deref(), Some("***-**-3333"));
        assert!(dssn.field.ends_with("Row3[0].f1_39[0]"));
        assert!(plan.skipped.iter().any(|s| s.field == "dependent_ssn"));
        // Identity skips: only pieces actually absent — the spouse
        // occupation is not stored; names/SSNs/address are all filled.
        assert!(plan.skipped.iter().any(|s| s.field == "spouse_occupation"));
        assert!(!plan.skipped.iter().any(|s| s.field == "taxpayer_ssn"));
        assert!(!plan.skipped.iter().any(|s| s.field == "taxpayer_name"));
        assert!(!plan.skipped.iter().any(|s| s.field == "home_address"));
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
