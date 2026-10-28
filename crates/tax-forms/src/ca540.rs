//! California Form 540 (TY2025), full-year resident — the California
//! companion to [`crate::compute_1040`].
//!
//! The computation starts from the computed federal return: Form 540
//! line 13 is federal AGI, and every California amount derives from
//! the federal facts the v1 engine already produces, plus the
//! California-only facts on the stored documents (W-2 boxes 16/17,
//! 1099-NEC box 5). Line keys in [`CaComputed::lines`] are the PRINTED
//! Form 540 line numbers, plus the internal keys
//! `caleitc_earned_income` (FTB 3514 line 19) and
//! `caleitc_qualifying_children` (a count, expressed in the money map
//! as count × $1.00, like the federal `eic_qualifying_children`).
//!
//! Schedule CA (540) scope — read this before trusting lines 14/16:
//! Part I adjustments are scoped to the facts the federal engine
//! produces, and for those facts California law is the same as federal
//! law (2025 Instructions for Schedule CA (540), Part I, Section C:
//! "California law is the same as federal law with the exception of
//! the following"). In particular the deductible part of
//! self-employment tax (federal line 10) is NOT added back: the
//! Section C line 15 adjustment exists only when the taxpayer is an
//! independent contractor for federal purposes but an employee for
//! California purposes, a classification difference v1 does not model
//! (a validate finding flags returns with 1099-NEC entries). Every
//! other Section A/B/C amount for the v1 persona (wages, interest,
//! dividends, capital gains — California taxes them at ordinary rates
//! but the amount carries over unchanged — and Schedule C net) is
//! California-conforming as-is, so lines 14 and 16 are a stated zero
//! for in-scope returns. Facts the engine cannot see (HSA
//! contributions, state-tax refunds, Social Security benefits, and
//! the rest of Schedule CA) are out of scope: see
//! docs/KNOWN_LIMITS.md.
//!
//! What is computed: exemptions and the AGI Limitation Worksheet
//! (lines 7–11, 32), the income chain (13–19), tax by the California
//! Tax Table at or below $100,000 of taxable income and by the Tax
//! Rate Schedules above it (line 31), tax after exemption credits
//! (33, 35, 48), the Behavioral Health Services Tax (line 62), total
//! tax (64), California withholding (line 71), the California EITC
//! (line 75; FTB 3514 logic in [`cal_eitc`]) and the Young Child Tax
//! Credit (line 76; FTB 3514 Part VII), total payments (78), and the
//! refund / amount-owed pair (97 / 100). Lines 91–95 (use tax,
//! Individual Shared Responsibility) are not modeled — lines 97/100
//! assume both are zero, as does line 78 for the unmodeled payment
//! lines 72–74 and 77 (estimated payments, real-estate withholding,
//! film credit, Foster Youth Tax Credit). The Nonrefundable Renter's
//! Credit (line 46) and all other credits are out of v1 scope.

use std::collections::BTreeMap;
use tax_core::{bracket_tax, CaEitcTable, CaParams, CaTaxTable, FactResult, FilingStatus, Money};

use crate::worksheets::classify_eic_child;
use crate::{ComputedReturn, Form1040Input, TaxpayerIdentity};
use tax_core::TaxParams;

/// A deterministic finding about the California section, mirroring
/// [`crate::ConsistencyFinding`] with an optional store entry label
/// (`w2_01`, `1099nec_01`, …) in the CLI's naming convention.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CaFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

/// A computed California Form 540 section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaComputed {
    /// Printed Form 540 line number → fact result, plus the internal
    /// `caleitc_*` keys described in the module docs.
    pub lines: BTreeMap<String, FactResult<Money>>,
    /// True only when every computed line is `Complete`.
    pub complete: bool,
    /// The line keys whose facts are `Incomplete`.
    pub incomplete: Vec<String>,
    /// Human-readable simplifications that apply to this computation.
    pub notes: Vec<String>,
    /// Deterministic findings for `return validate`.
    pub findings: Vec<CaFinding>,
    /// Which method figured line 31: `"ca_tax_table"` or
    /// `"ca_rate_schedule"`; `None` when line 31 is not computed.
    pub line31_method: Option<String>,
    /// The CalEITC detail (FTB 3514), when the credit's inputs were
    /// determinable enough to run the worksheet.
    pub caleitc: Option<CaEitc>,
}

/// The California earned income tax credit computation (form
/// FTB 3514), following the booklet's Steps 1–6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaEitc {
    /// Qualifying children counted (the federal EIC child tests of
    /// [`classify_eic_child`]; California additionally requires the
    /// child to have lived in California more than half the year,
    /// which the full-year-resident state block stands in for).
    pub qualifying_children: u32,
    /// California earned income (FTB 3514 line 19): W-2 box 16 wages
    /// plus business income (Worksheet 3: Schedule C net minus the
    /// deductible part of self-employment tax).
    pub earned_income: Money,
    /// Investment income (FTB 3514 Worksheet 1, in v1 scope: federal
    /// lines 2a + 2b + 3b plus the positive part of line 7).
    pub investment_income: Money,
    /// The credit for Form 540 line 75 (zero when disqualified).
    pub credit: Money,
    /// Why the credit is zero, when a disqualifier decided it:
    /// `"mfs"`, `"agi_limit"`, `"investment_income_limit"`,
    /// `"no_earned_income"`, `"childless_age_not_met"`, or
    /// `"childless_age_unknown"` (the no-children age-18 test cannot
    /// be determined because no date of birth is stored — v1 takes no
    /// credit rather than guessing). `None` when the Step 6 worksheet
    /// produced the amount.
    pub disqualifier: Option<&'static str>,
}

fn combine(
    a: &FactResult<Money>,
    b: &FactResult<Money>,
    f: impl Fn(Money, Money) -> Money,
) -> FactResult<Money> {
    use FactResult::*;
    match (a, b) {
        (Incomplete, _) | (_, Incomplete) => Incomplete,
        (Placeholder(x), Placeholder(y)) => Placeholder(f(*x, *y)),
        (Placeholder(x), Complete(y)) | (Complete(x), Placeholder(y)) => Placeholder(f(*x, *y)),
        (Complete(x), Complete(y)) => Complete(f(*x, *y)),
    }
}

fn map_fact(fr: &FactResult<Money>, f: impl Fn(Money) -> Money) -> FactResult<Money> {
    fr.clone().map(f)
}

fn fact(lines: &BTreeMap<String, FactResult<Money>>, key: &str) -> FactResult<Money> {
    lines.get(key).cloned().unwrap_or(FactResult::Incomplete)
}

/// The Form 540 exemption counts (lines 7–10 boxes): personal, blind,
/// senior, dependents.
///
/// Personal exemptions follow the form: one per filer (two if
/// married filing jointly) — reduced by the line 6 rule when the
/// taxpayer can be claimed as a dependent (line 7 instructions: enter
/// -0- for single / MFS / head of household; on a joint return, 1
/// when only one spouse can be claimed). Blind and senior counts come
/// from the stored identity (the taxpayer's, plus the spouse's for
/// MFJ/MFS); a claimable taxpayer's own blind/senior counts are
/// dropped ("Do not claim this credit if someone else can claim you
/// as a dependent", line 8 instructions). Age 65 uses the shared
/// TY2025 cutoff ([`crate::PersonIdentity::is_age_65`]: born before
/// January 2, 1961 — the booklet's line 9 test is "65 years of age or
/// older by December 31, 2025"). Dependent exemptions count every
/// stored dependent entry — the dependents claimed in the Form 540
/// Dependents section.
pub(crate) fn exemption_counts(input: &Form1040Input, params: &TaxParams) -> (u32, u32, u32, u32) {
    let status = input.status;
    let identity = input.identity.as_ref();
    let claimable = identity
        .map(|i| i.can_be_claimed_as_dependent)
        .unwrap_or(false);
    let personal = match (status, claimable) {
        (FilingStatus::Mfj, false) => 2,
        (FilingStatus::Mfj, true) => 1,
        (_, false) => 1,
        (_, true) => 0,
    };
    let (mut blind, mut senior) = (0u32, 0u32);
    if let Some(identity) = identity {
        let mut persons: Vec<&crate::PersonIdentity> = Vec::new();
        if !claimable {
            persons.push(&identity.person);
        }
        if matches!(status, FilingStatus::Mfj | FilingStatus::Mfs) {
            if let Some(spouse) = &identity.spouse {
                persons.push(spouse);
            }
        }
        for p in persons {
            blind += u32::from(p.blind);
            senior += u32::from(p.is_age_65(params));
        }
    }
    (personal, blind, senior, input.dependents.len() as u32)
}

/// Form 540 line 32: the exemption credits of line 11 after the AGI
/// Limitation Worksheet (2025 Form 540 booklet, instructions for
/// line 32). The worksheet is figured in whole dollars: steps of the
/// federal AGI over the status threshold ($2,500, or $1,250 if MFS),
/// rounded up, times $6, reduced per exemption — the lines 7–9 group
/// (scaled by its exemption count) and the line 10 dependents
/// (scaled by the dependent count) separately, each floored at zero.
fn exemption_credits_after_limitation(
    federal_agi: Money,
    status: FilingStatus,
    counts: (u32, u32, u32, u32),
    line_amounts: (Money, Money, Money, Money),
    ca: &CaParams,
) -> Money {
    let total = line_amounts.0 + line_amounts.1 + line_amounts.2 + line_amounts.3;
    let threshold = ca.exemption_phaseout_threshold(status);
    if federal_agi.cents() <= threshold.cents() {
        return total;
    }
    let step = ca.exemption_phaseout_step(status);
    let excess_dollars = federal_agi.cents().div_euclid(100) - threshold.cents() / 100;
    let step_dollars = step.cents() / 100;
    let steps = (excess_dollars + step_dollars - 1).div_euclid(step_dollars);
    let per_exemption = Money::from_cents(steps * ca.exemption_credits.phaseout_per_step_cents);
    let personal_count = i64::from(counts.0 + counts.1 + counts.2);
    let personal_total = line_amounts.0 + line_amounts.1 + line_amounts.2;
    let personal_after =
        Money::from_cents((personal_total.cents() - per_exemption.cents() * personal_count).max(0));
    let dependent_after = Money::from_cents(
        (line_amounts.3.cents() - per_exemption.cents() * i64::from(counts.3)).max(0),
    );
    personal_after + dependent_after
}

/// Form 540 line 31: the California Tax Table at or below $100,000
/// of taxable income, the Tax Rate Schedules above it.
fn ca_tax(status: FilingStatus, taxable: Money, ca: &CaParams) -> (Money, &'static str) {
    match CaTaxTable::load_2025().tax(status, taxable) {
        Some(tax) => (tax, "ca_tax_table"),
        None => (
            bracket_tax(taxable, &ca.bracket_table(status)),
            "ca_rate_schedule",
        ),
    }
}

/// The no-children CalEITC age outcome (FTB 3514 Step 4, question b):
/// `Some(true)` when a stored date of birth shows a filer (or, on a
/// joint return, the spouse) at least 18 at the end of 2025 — born on
/// or before January 1, 2008, the form's own wording — `Some(false)`
/// when every stored date of birth is younger, and `None` when no
/// usable date of birth is stored.
fn ca_childless_age_outcome(
    identity: Option<&TaxpayerIdentity>,
    status: FilingStatus,
    ca: &CaParams,
) -> Option<bool> {
    let cutoff = ca.caleitc.childless_born_on_or_before.as_str();
    let in_range = |dob: &str| crate::valid_iso_date(dob) && dob <= cutoff;
    let identity = identity?;
    let taxpayer = identity.person.date_of_birth.as_deref().map(in_range);
    if status != FilingStatus::Mfj {
        return taxpayer;
    }
    let spouse = identity
        .spouse
        .as_ref()
        .and_then(|s| s.date_of_birth.as_deref())
        .map(in_range);
    match (taxpayer, spouse) {
        (Some(a), Some(b)) => Some(a || b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Figure the California earned income tax credit (form FTB 3514,
/// Steps 1–6; Step 7 is the part-year computation and does not apply
/// to full-year residents).
///
/// Gates, in the booklet's order: married filing separately takes no
/// credit in v1 (the separated-spouse exception is not modeled, as
/// federally); federal AGI must be less than $32,901 (Step 1,
/// question a); investment income must not exceed $4,814 (Step 2,
/// Worksheet 1); with no qualifying children the filer must be at
/// least 18 (Step 4, question b — the with-children path of Step 3
/// has no filer age test). Earned income (Step 5) is W-2 box 16
/// California wages plus Worksheet 3 business income (Schedule C net
/// minus the deductible part of self-employment tax). The Step 6
/// worksheet looks the credit up in the California EITC Table on
/// earned income; when federal AGI differs, it also looks it up on
/// AGI — unless AGI is below the Part II threshold for the child
/// count — and takes the smaller.
///
/// Returns the credit detail plus a flag for the one input that can
/// make the credit `Incomplete` rather than zero or an amount: a W-2
/// whose box 16 was not transcribed (box 16 is a reported fact the
/// engine cannot approximate with box 1 — missing ≠ 0).
#[allow(clippy::too_many_arguments)]
pub fn cal_eitc(
    input: &Form1040Input,
    federal_agi: Money,
    investment_income: Money,
    business_income: Money,
    ca: &CaParams,
) -> (CaEitc, bool) {
    let children = input
        .dependents
        .iter()
        .filter(|d| classify_eic_child(d))
        .count() as u32;
    let wages_incomplete = input.w2s.iter().any(|w| w.state_wages.is_none());
    let box16_wages: Money = input.w2s.iter().filter_map(|w| w.state_wages).sum();
    let earned_income = box16_wages + business_income;
    let done = |credit: Money, disqualifier: Option<&'static str>| {
        (
            CaEitc {
                qualifying_children: children,
                earned_income,
                investment_income,
                credit,
                disqualifier,
            },
            wages_incomplete,
        )
    };
    if input.status == FilingStatus::Mfs {
        return done(Money::ZERO, Some("mfs"));
    }
    if federal_agi.cents() >= ca.caleitc.agi_limit_cents {
        return done(Money::ZERO, Some("agi_limit"));
    }
    if investment_income.cents() > ca.caleitc.investment_income_limit_cents {
        return done(Money::ZERO, Some("investment_income_limit"));
    }
    if children == 0 {
        match ca_childless_age_outcome(input.identity.as_ref(), input.status, ca) {
            Some(true) => {}
            Some(false) => return done(Money::ZERO, Some("childless_age_not_met")),
            None => return done(Money::ZERO, Some("childless_age_unknown")),
        }
    }
    if earned_income.cents() <= 0 {
        return done(Money::ZERO, Some("no_earned_income"));
    }
    let table = CaEitcTable::load_2025();
    let on_earned = table.credit(children, earned_income);
    // Step 6 worksheet, Part II: when AGI equals earned income, or
    // AGI is below the threshold for the child count, the earned
    // lookup IS the credit; otherwise take the smaller of the two.
    let credit = if federal_agi == earned_income
        || federal_agi < ca.caleitc_agi_lookup_threshold(children)
    {
        on_earned
    } else {
        let on_agi = table.credit(children, federal_agi);
        Money::from_cents(on_earned.cents().min(on_agi.cents()))
    };
    done(credit, None)
}

/// The Young Child Tax Credit amount (FTB 3514 Part VII, lines
/// 24–28) for a taxpayer who was allowed the CalEITC with earned
/// income over zero: $1,189, reduced by $21.71 for every $100 (to two
/// decimal places, not rounded) of California earned income over
/// $27,425; a result between $0 and $1 is entered as $1, and a result
/// over $1 is rounded to the nearest whole dollar (line 28).
fn yctc_amount(earned_income: Money, ca: &CaParams) -> Money {
    let p = &ca.yctc;
    if earned_income.cents() <= p.phaseout_threshold_cents {
        return Money::from_cents(p.max_cents);
    }
    let excess_cents = earned_income.cents() - p.phaseout_threshold_cents;
    // Line 26: the excess divided by 100, two decimal places, not
    // rounded — in hundredths of a unit that is floor(excess / $1.00).
    let units_hundredths = excess_cents.div_euclid(100);
    // Hundredths of a cent, to keep the $21.71 multiplication exact.
    let credit_hundredths =
        p.max_cents * 100 - units_hundredths * p.reduction_per_100_dollars_cents;
    if credit_hundredths <= 0 {
        return Money::ZERO;
    }
    if credit_hundredths < 10_000 {
        // Between $0 and $1: the form enters $1 (line 28).
        return Money::from_cents(100);
    }
    let dollars = (credit_hundredths + 5_000) / 10_000;
    Money::from_cents(dollars * 100)
}

/// Compute the California Form 540 section for a return whose
/// federal side has been computed by [`crate::compute_1040`].
///
/// The caller is responsible for the state selection: this function
/// assumes `input.state` selected California full-year residency
/// (validated by [`crate::StateBlock::validated`] at set time and by
/// the CLI before calling).
///
/// Completeness: the California section inherits the federal facts'
/// completeness (a federal line 11 that is `Incomplete` makes every
/// AGI-derived California line `Incomplete`), and adds its own: a
/// W-2 or 1099-NEC whose state withholding box was not transcribed
/// makes line 71 (and everything downstream of total payments)
/// `Incomplete`, with a `ca_state_withholding_missing` finding per
/// entry; a W-2 whose box 16 was not transcribed makes the CalEITC
/// `Incomplete` when the credit is not already decided by a gate
/// that needs no wage figure.
pub fn compute_ca540(
    input: &Form1040Input,
    fed: &ComputedReturn,
    params: &TaxParams,
    ca: &CaParams,
) -> CaComputed {
    let status = input.status;
    let mut lines: BTreeMap<String, FactResult<Money>> = BTreeMap::new();
    let mut findings: Vec<CaFinding> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    let fed_agi = fact(&fed.lines, "11");
    let fed_line10 = fact(&fed.lines, "10");
    let fed_investment = fact(&fed.lines, "eic_investment_income");
    let schc_net = fact(&fed.lines, "schc_net_profit");

    // ---- Exemptions (lines 7-11): computable from the stored facts
    // alone; the AGI limitation applies later, at line 32.
    let counts = exemption_counts(input, params);
    let unit = Money::from_cents(ca.exemption_credits.personal_cents);
    let line7 = Money::from_cents(unit.cents() * i64::from(counts.0));
    let line8 = Money::from_cents(unit.cents() * i64::from(counts.1));
    let line9 = Money::from_cents(unit.cents() * i64::from(counts.2));
    let line10 = Money::from_cents(ca.exemption_credits.dependent_cents * i64::from(counts.3));
    lines.insert("7".into(), FactResult::Complete(line7));
    lines.insert("8".into(), FactResult::Complete(line8));
    lines.insert("9".into(), FactResult::Complete(line9));
    lines.insert("10".into(), FactResult::Complete(line10));
    lines.insert(
        "11".into(),
        FactResult::Complete(line7 + line8 + line9 + line10),
    );

    // ---- Income (lines 13-19). Schedule CA totals (lines 14/16) are
    // a stated zero for in-scope facts (module docs).
    lines.insert("13".into(), fed_agi.clone());
    lines.insert("14".into(), FactResult::Complete(Money::ZERO));
    lines.insert("15".into(), fed_agi.clone());
    lines.insert("16".into(), FactResult::Complete(Money::ZERO));
    lines.insert("17".into(), fed_agi.clone());
    let std_deduction = ca.standard_deduction(status);
    lines.insert("18".into(), FactResult::Complete(std_deduction));
    let line19 = map_fact(&fed_agi, |agi| {
        Money::from_cents((agi.cents() - std_deduction.cents()).max(0))
    });
    lines.insert("19".into(), line19.clone());

    // ---- Tax and nonrefundable credits (lines 31-48, 62, 64).
    let (line31, line31_method) = match line19.value() {
        Some(taxable) => {
            let (tax, method) = ca_tax(status, *taxable, ca);
            (map_fact(&line19, |_| tax), Some(method.to_string()))
        }
        None => (FactResult::Incomplete, None),
    };
    lines.insert("31".into(), line31.clone());
    let line32 = match fed_agi.value() {
        Some(agi) => FactResult::Complete(exemption_credits_after_limitation(
            *agi,
            status,
            counts,
            (line7, line8, line9, line10),
            ca,
        )),
        None => FactResult::Incomplete,
    };
    // Preserve Placeholder flavor if the federal AGI is a placeholder.
    let line32 = match (&fed_agi, &line32) {
        (FactResult::Placeholder(_), FactResult::Complete(v)) => FactResult::Placeholder(*v),
        _ => line32,
    };
    lines.insert("32".into(), line32.clone());
    let line33 = combine(&line31, &line32, |tax, credits| {
        Money::from_cents((tax.cents() - credits.cents()).max(0))
    });
    lines.insert("33".into(), line33.clone());
    lines.insert("34".into(), FactResult::Complete(Money::ZERO));
    lines.insert("35".into(), line33.clone());
    lines.insert("48".into(), line33.clone());
    let bhst = map_fact(&line19, |taxable| {
        let excess_dollars = (taxable.cents().div_euclid(100)
            - ca.behavioral_health_services_tax.threshold_cents / 100)
            .max(0);
        // 1% of a whole-dollar excess is exactly `excess` cents.
        Money::from_cents(excess_dollars * ca.behavioral_health_services_tax.rate_bp as i64 / 100)
    });
    lines.insert("62".into(), bhst.clone());
    let line64 = combine(&line33, &bhst, |a, b| a + b);
    lines.insert("64".into(), line64.clone());

    // ---- Withholding (line 71).
    let mut withholding = Money::ZERO;
    let mut withholding_incomplete = false;
    for (i, w2) in input.w2s.iter().enumerate() {
        match w2.state_withholding {
            Some(amount) => withholding = withholding + amount,
            None => {
                withholding_incomplete = true;
                findings.push(CaFinding {
                    code: "ca_state_withholding_missing",
                    severity: "warning",
                    message: format!(
                        "W-2 from {}: box 17 (state income tax withheld) was not \
                         transcribed, so California withholding (Form 540 line 71) is \
                         unknown — not zero. Record \"0\" if the W-2 reports no state \
                         withholding",
                        w2.employer_ein
                    ),
                    entry: Some(format!("w2_{:02}", i + 1)),
                });
            }
        }
    }
    for (i, nec) in input.f1099_necs.iter().enumerate() {
        match nec.state_withholding {
            Some(amount) => withholding = withholding + amount,
            None => {
                withholding_incomplete = true;
                findings.push(CaFinding {
                    code: "ca_state_withholding_missing",
                    severity: "warning",
                    message: format!(
                        "1099-NEC from {}: box 5 (state tax withheld) was not \
                         transcribed, so California withholding (Form 540 line 71) is \
                         unknown — not zero. Record \"0\" if the payer withheld no \
                         state tax",
                        nec.payer_name
                    ),
                    entry: Some(format!("1099nec_{:02}", i + 1)),
                });
            }
        }
    }
    let line71 = if withholding_incomplete {
        FactResult::Incomplete
    } else {
        FactResult::Complete(withholding)
    };
    lines.insert("71".into(), line71.clone());

    if !input.f1099_necs.is_empty() {
        findings.push(CaFinding {
            code: "ca_nec_classification_not_modeled",
            severity: "info",
            message: "1099-NEC engagements are treated as self-employment for \
                      California as they are federally: California conforms to the \
                      federal deductible part of self-employment tax for a taxpayer \
                      who is self-employed for both (2025 Instructions for Schedule \
                      CA (540), Part I, Section C, line 15). If an engagement is \
                      employment for California purposes but contracting federally \
                      (e.g. AB 5), the Schedule CA line 15/17 adjustments and the \
                      FTB 3514 classification rules would change the California \
                      amounts — not modeled in v1"
                .to_string(),
            entry: None,
        });
    }

    // ---- CalEITC (line 75) and YCTC (line 76).
    // Business income for FTB 3514 Worksheet 3: Schedule C net minus
    // the deductible part of SE tax (the federal line 10 amount).
    let business_income = combine(&schc_net, &fed_line10, |net, ded| net - ded);
    let (eitc_detail, eitc_line) = match (
        fed_agi.value(),
        fed_investment.value(),
        business_income.value(),
    ) {
        (Some(agi), Some(inv), Some(bus)) => {
            let (eitc, wages_incomplete) = cal_eitc(input, *agi, *inv, *bus, ca);
            // The credit is Incomplete when box 16 is missing AND no
            // wage-free gate already decided it: the MFS bar, the AGI
            // limit, the investment-income limit, and the childless
            // age test all resolve without a wage figure, but
            // "no earned income" cannot be concluded from an unknown
            // wage amount (and neither can a table amount).
            let wage_free_decided = matches!(
                eitc.disqualifier,
                Some(
                    "mfs"
                        | "agi_limit"
                        | "investment_income_limit"
                        | "childless_age_not_met"
                        | "childless_age_unknown"
                )
            );
            let eitc_incomplete = wages_incomplete && !wage_free_decided;
            lines.insert(
                "caleitc_earned_income".into(),
                if wages_incomplete {
                    FactResult::Incomplete
                } else {
                    FactResult::Complete(eitc.earned_income)
                },
            );
            lines.insert(
                "caleitc_qualifying_children".into(),
                FactResult::Complete(Money::from_cents(i64::from(eitc.qualifying_children) * 100)),
            );
            let line = if eitc_incomplete {
                for (i, w2) in input.w2s.iter().enumerate() {
                    if w2.state_wages.is_none() {
                        findings.push(CaFinding {
                            code: "ca_wages_box16_missing",
                            severity: "warning",
                            message: format!(
                                "W-2 from {}: box 16 (state wages) was not transcribed; \
                                 California earned income for the California EITC is \
                                 read from box 16 (FTB 3514 line 13), not box 1, so \
                                 the credit cannot be figured — not treated as zero",
                                w2.employer_ein
                            ),
                            entry: Some(format!("w2_{:02}", i + 1)),
                        });
                    }
                }
                FactResult::Incomplete
            } else {
                match eitc.disqualifier {
                    Some("mfs") => findings.push(CaFinding {
                        code: "caleitc_mfs",
                        severity: "warning",
                        message: "filing status is married filing separately: v1 allows \
                                  no California EITC for MFS (the separated-spouse \
                                  exception of FTB 3514 Steps 1 and 3 is not modeled) \
                                  — Form 540 line 75 is 0"
                            .to_string(),
                        entry: None,
                    }),
                    Some("investment_income_limit") => findings.push(CaFinding {
                        code: "caleitc_investment_income_limit",
                        severity: "info",
                        message: format!(
                            "investment income (FTB 3514 Worksheet 1, v1 scope) exceeds \
                             the TY2025 limit of $4,814: no California EITC is allowed \
                             — Form 540 line 75 is 0 (figured investment income: {})",
                            eitc.investment_income
                        ),
                        entry: None,
                    }),
                    Some("childless_age_unknown") => findings.push(CaFinding {
                        code: "caleitc_childless_age_unknown",
                        severity: "info",
                        message: "no qualifying children and no taxpayer date of birth \
                                  is set: the California EITC age-18 test (FTB 3514 \
                                  Step 4, question b) cannot be determined, so line 75 \
                                  is 0 — set the taxpayer date of birth with `tax \
                                  return set-identity` if the childless credit may apply"
                            .to_string(),
                        entry: None,
                    }),
                    _ => {}
                }
                if eitc.credit.cents() > 0 {
                    let ssn_missing = match &input.identity {
                        None => true,
                        Some(id) => {
                            id.person.ssn.is_none()
                                || (status == FilingStatus::Mfj
                                    && id.spouse.as_ref().map(|s| s.ssn.is_none()) != Some(false))
                        }
                    };
                    if ssn_missing {
                        findings.push(CaFinding {
                            code: "caleitc_ssn_unverified",
                            severity: "warning",
                            message: "the California EITC is computed, but FTB 3514 \
                                      Step 1, question b requires a valid SSN or ITIN \
                                      for the filer (and the spouse on a joint return) \
                                      and no SSN is stored for every required person \
                                      — v1 computes the credit; a real filing without \
                                      one would be denied"
                                .to_string(),
                            entry: None,
                        });
                    }
                }
                FactResult::Complete(eitc.credit)
            };
            (Some(eitc), line)
        }
        _ => {
            lines.insert("caleitc_earned_income".into(), FactResult::Incomplete);
            lines.insert(
                "caleitc_qualifying_children".into(),
                FactResult::Complete(Money::from_cents(
                    i64::from(
                        input
                            .dependents
                            .iter()
                            .filter(|d| classify_eic_child(d))
                            .count() as u32,
                    ) * 100,
                )),
            );
            (None, FactResult::Incomplete)
        }
    };
    lines.insert("75".into(), eitc_line.clone());

    // ---- YCTC (line 76).
    let under_six = input
        .dependents
        .iter()
        .filter(|d| classify_eic_child(d) && d.age < 6)
        .count();
    let line76 = if under_six == 0 {
        FactResult::Complete(Money::ZERO)
    } else if matches!(eitc_line, FactResult::Incomplete) {
        // The YCTC rides on the CalEITC result; an unknown credit
        // (unknown wages, unknown federal facts) makes it unknown.
        FactResult::Incomplete
    } else {
        match &eitc_detail {
            None => FactResult::Incomplete,
            Some(eitc) => match eitc.disqualifier {
                None => FactResult::Complete(yctc_amount(eitc.earned_income, ca)),
                Some("no_earned_income") => {
                    // The earned-income-of-zero-or-less expansion
                    // (FTB 3514 Step 8): eligibility turns on the
                    // line 23a/23b gates (wages and net losses each
                    // ≤ $35,640, federal AGI ≤ $32,900). v1 does not
                    // compute this path; when the AGI gate passes the
                    // credit is genuinely unknown → Incomplete.
                    match fed_agi.value() {
                        Some(agi) if agi.cents() <= ca.yctc.federal_agi_cap_cents => {
                            findings.push(CaFinding {
                                code: "yctc_zero_earned_income_path",
                                severity: "warning",
                                message: "a qualifying child under six is claimed and \
                                          California earned income is zero or less: the \
                                          Young Child Tax Credit may still be \
                                          available under the FTB 3514 Step 8 \
                                          expansion (subject to the $35,640 wage and \
                                          net-loss gates of lines 23a/23b), which v1 \
                                          does not compute — Form 540 line 76 is \
                                          incomplete, not zero"
                                    .to_string(),
                                entry: None,
                            });
                            FactResult::Incomplete
                        }
                        Some(_) => FactResult::Complete(Money::ZERO),
                        None => FactResult::Incomplete,
                    }
                }
                Some(_) => FactResult::Complete(Money::ZERO),
            },
        }
    };
    lines.insert("76".into(), line76);

    // ---- Total payments, refund / owed (lines 78, 97, 100).
    let line78 = combine(
        &combine(&line71, &eitc_line, |a, b| a + b),
        &lines["76"],
        |a, b| a + b,
    );
    lines.insert("78".into(), line78.clone());
    let line97 = combine(&line78, &line64, |payments, tax| {
        Money::from_cents((payments.cents() - tax.cents()).max(0))
    });
    let line100 = combine(&line78, &line64, |payments, tax| {
        Money::from_cents((tax.cents() - payments.cents()).max(0))
    });
    lines.insert("97".into(), line97);
    lines.insert("100".into(), line100);

    notes.push(
        "California section: full-year-resident Form 540 only. Schedule CA (540) \
         adjustments are scoped to engine facts (all conforming for the v1 persona — \
         lines 14/16 are zero); use tax and the Individual Shared Responsibility \
         penalty are not modeled, so lines 78/97/100 treat them as zero, as they do \
         the unmodeled payment lines 72-74/77 and credit lines 40-47 (including the \
         Nonrefundable Renter's Credit). See docs/KNOWN_LIMITS.md."
            .to_string(),
    );

    let incomplete: Vec<String> = lines
        .iter()
        .filter(|(_, f)| matches!(f, FactResult::Incomplete))
        .map(|(k, _)| k.clone())
        .collect();
    CaComputed {
        complete: incomplete.is_empty()
            && lines.values().all(|f| matches!(f, FactResult::Complete(_))),
        incomplete,
        lines,
        notes,
        findings,
        line31_method,
        caleitc: eitc_detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compute_1040, DependentInput, DependentRelationship, I1099NecInput, PersonIdentity,
        TaxpayerIdentity, W2Input,
    };
    use tax_core::TaxParams;

    fn fed_params() -> TaxParams {
        TaxParams::load_2025().unwrap()
    }
    fn ca_params() -> CaParams {
        CaParams::load_2025().unwrap()
    }
    fn m(dollars: i64) -> Money {
        Money::from_cents(dollars * 100)
    }

    fn w2(wages: i64, fed_wh: i64, state_wages: Option<i64>, state_wh: Option<i64>) -> W2Input {
        W2Input {
            employer_ein: "12-3456789".into(),
            wages: m(wages),
            federal_withholding: m(fed_wh),
            social_security_wages: None,
            social_security_tax: None,
            medicare_wages: None,
            medicare_tax: None,
            state_wages: state_wages.map(m),
            state_withholding: state_wh.map(m),
        }
    }

    fn base(status: FilingStatus) -> Form1040Input {
        Form1040Input {
            f1042ss: Vec::new(),
            nra: None,
            status,
            w2s: vec![],
            interest_income: Money::ZERO,
            ordinary_dividends: Money::ZERO,
            f1099_ints: vec![],
            f1099_divs: vec![],
            f1099_bs: vec![],
            f1099_necs: vec![],
            dependents: vec![],
            identity: None,
            state: Some(crate::StateBlock {
                state: "CA".into(),
                residency: "full_year".into(),
            }),
        }
    }

    fn compute(input: &Form1040Input) -> CaComputed {
        let fed = compute_1040(input, &fed_params());
        compute_ca540(input, &fed, &fed_params(), &ca_params())
    }

    fn line(ca: &CaComputed, key: &str) -> Money {
        ca.lines[key]
            .value()
            .copied()
            .unwrap_or_else(|| panic!("line {key} should be computed"))
    }

    fn person(dob: &str) -> PersonIdentity {
        PersonIdentity {
            first_name: None,
            last_name: None,
            ssn: Some("123456789".into()),
            date_of_birth: Some(dob.into()),
            occupation: None,
            blind: false,
        }
    }

    fn kid(name: &str, age: u32) -> DependentInput {
        DependentInput {
            name: name.into(),
            ssn: Some("123456789".into()),
            relationship: DependentRelationship::Child,
            age,
            months_lived_with_taxpayer: 12,
            full_time_student: false,
            permanently_disabled: false,
        }
    }

    #[test]
    fn single_w2_full_chain() {
        // Golden case CA-001 shape.
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(50_000, 4_000, Some(50_000), Some(1_500))];
        let ca = compute(&input);
        assert!(ca.complete, "incomplete: {:?}", ca.incomplete);
        assert_eq!(line(&ca, "13"), m(50_000));
        assert_eq!(line(&ca, "14"), Money::ZERO);
        assert_eq!(line(&ca, "16"), Money::ZERO);
        assert_eq!(line(&ca, "17"), m(50_000));
        assert_eq!(line(&ca, "18"), m(5_706));
        assert_eq!(line(&ca, "19"), m(44_294));
        assert_eq!(line(&ca, "31"), m(1_193));
        assert_eq!(ca.line31_method.as_deref(), Some("ca_tax_table"));
        assert_eq!(line(&ca, "7"), m(153));
        assert_eq!(line(&ca, "11"), m(153));
        assert_eq!(line(&ca, "32"), m(153));
        assert_eq!(line(&ca, "33"), m(1_040));
        assert_eq!(line(&ca, "64"), m(1_040));
        assert_eq!(line(&ca, "71"), m(1_500));
        assert_eq!(line(&ca, "75"), Money::ZERO);
        assert_eq!(line(&ca, "78"), m(1_500));
        assert_eq!(line(&ca, "97"), m(460));
        assert_eq!(line(&ca, "100"), Money::ZERO);
    }

    #[test]
    fn nec_income_no_schedule_ca_addback() {
        // Golden case CA-003 shape (federal case 005 + CA fields).
        // Per the 2025 Schedule CA (540) instructions, Section C,
        // California conforms to the deductible part of SE tax for a
        // taxpayer self-employed for both purposes: lines 14/16 are
        // zero and CA AGI equals federal AGI even with NEC income.
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(50_000, 4_000, Some(50_000), Some(1_500))];
        input.f1099_necs = vec![I1099NecInput {
            payer_name: "Example Client LLC".into(),
            payer_ein: None,
            nonemployee_compensation: m(20_000),
            federal_withholding: Money::ZERO,
            expenses: m(2_000),
            state_withholding: Some(Money::ZERO),
        }];
        let fed = compute_1040(&input, &fed_params());
        let ca = compute_ca540(&input, &fed, &fed_params(), &ca_params());
        assert!(ca.complete, "incomplete: {:?}", ca.incomplete);
        assert_eq!(line(&ca, "14"), Money::ZERO);
        assert_eq!(line(&ca, "16"), Money::ZERO);
        assert_eq!(line(&ca, "17"), line(&ca, "13"));
        assert_eq!(line(&ca, "13").cents(), 6_672_834); // federal AGI $66,728.34
        assert_eq!(line(&ca, "19").cents(), 6_102_234);
        assert_eq!(line(&ca, "31"), m(2_264));
        assert_eq!(line(&ca, "33"), m(2_111));
        assert_eq!(line(&ca, "71"), m(1_500));
        assert_eq!(line(&ca, "97"), Money::ZERO);
        assert_eq!(line(&ca, "100"), m(611));
        assert!(ca
            .findings
            .iter()
            .any(|f| f.code == "ca_nec_classification_not_modeled"));
    }

    #[test]
    fn exemption_credit_phaseout_high_agi() {
        // Single, federal AGI $260,000: excess $7,797 over $252,203 →
        // ceil(7797 / 2500) = 4 steps → $24 per exemption → the $153
        // personal credit is limited to $129 (AGI Limitation
        // Worksheet, worked from the booklet instructions).
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(260_000, 40_000, Some(260_000), Some(20_000))];
        let ca = compute(&input);
        assert_eq!(line(&ca, "11"), m(153));
        assert_eq!(line(&ca, "32"), m(129));
        // MFJ, AGI $510,000 with two dependents: excess $5,589 →
        // 3 steps → $18 per exemption; personal group (2 × $153)
        // less 2 × $18 = $270; dependents (2 × $475) less 2 × $18 =
        // $914; line 32 = $1,184.
        let mut input = base(FilingStatus::Mfj);
        input.w2s = vec![w2(510_000, 90_000, Some(510_000), Some(40_000))];
        input.dependents = vec![kid("Kid One", 9), kid("Kid Two", 7)];
        let ca = compute(&input);
        assert_eq!(line(&ca, "11"), m(306 + 950));
        assert_eq!(line(&ca, "32"), m(1_184));
        // MFS divisor is $1,250: AGI $260,000 → ceil(7797/1250) = 7
        // steps → $42 per exemption → $153 − $42 = $111.
        let mut input = base(FilingStatus::Mfs);
        input.w2s = vec![w2(260_000, 40_000, Some(260_000), Some(20_000))];
        let ca = compute(&input);
        assert_eq!(line(&ca, "32"), m(111));
    }

    #[test]
    fn family_caleitc_and_yctc() {
        // Golden case CA-002 shape.
        let mut input = base(FilingStatus::Mfj);
        input.w2s = vec![w2(28_000, 1_000, Some(28_000), Some(200))];
        input.dependents = vec![kid("Kid One", 5), kid("Kid Two", 3)];
        input.identity = Some(TaxpayerIdentity {
            person: person("1993-04-02"),
            spouse: Some(person("1995-08-20")),
            ..Default::default()
        });
        let ca = compute(&input);
        assert!(ca.complete, "incomplete: {:?}", ca.incomplete);
        assert_eq!(line(&ca, "19"), m(16_588));
        assert_eq!(line(&ca, "31"), m(166));
        assert_eq!(line(&ca, "11"), m(1_256));
        assert_eq!(line(&ca, "33"), Money::ZERO);
        // CalEITC Table, row 27,951-28,000, two children: $207.
        assert_eq!(line(&ca, "75"), m(207));
        // YCTC: excess $575 → 5.75 × $21.71 = $124.8325 → $1,064.1675
        // → $1,064 (FTB 3514 Part VII, lines 25-28).
        assert_eq!(line(&ca, "76"), m(1_064));
        assert_eq!(line(&ca, "78"), m(200 + 207 + 1_064));
        assert_eq!(line(&ca, "97"), m(1_471));
        let eitc = ca.caleitc.expect("caleitc detail");
        assert_eq!(eitc.qualifying_children, 2);
        assert_eq!(eitc.earned_income, m(28_000));
        assert_eq!(eitc.disqualifier, None);
    }

    #[test]
    fn caleitc_investment_income_disqualifies() {
        let mut input = base(FilingStatus::Mfj);
        input.w2s = vec![w2(27_000, 1_000, Some(27_000), Some(200))];
        input.interest_income = m(4_900);
        input.dependents = vec![kid("Kid One", 5), kid("Kid Two", 3)];
        input.identity = Some(TaxpayerIdentity {
            person: person("1993-04-02"),
            spouse: Some(person("1995-08-20")),
            ..Default::default()
        });
        let ca = compute(&input);
        // AGI $31,900 < $32,901 passes Step 1, but investment income
        // $4,900 > $4,814 fails Step 2 → no credit, and no YCTC.
        assert_eq!(line(&ca, "75"), Money::ZERO);
        assert_eq!(line(&ca, "76"), Money::ZERO);
        assert_eq!(
            ca.caleitc.unwrap().disqualifier,
            Some("investment_income_limit")
        );
        assert!(ca
            .findings
            .iter()
            .any(|f| f.code == "caleitc_investment_income_limit"));
    }

    #[test]
    fn caleitc_childless_age_paths() {
        // Age 30 with DOB stored: table row 9,951-10,000, 0 children.
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(10_000, 500, Some(10_000), Some(300))];
        input.identity = Some(TaxpayerIdentity {
            person: person("1995-06-01"),
            ..Default::default()
        });
        let ca = compute(&input);
        assert_eq!(line(&ca, "75"), m(211));
        // No DOB: the age test is undeterminable → stated zero + info.
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(10_000, 500, Some(10_000), Some(300))];
        let ca = compute(&input);
        assert_eq!(line(&ca, "75"), Money::ZERO);
        assert_eq!(
            ca.caleitc.unwrap().disqualifier,
            Some("childless_age_unknown")
        );
        assert!(ca
            .findings
            .iter()
            .any(|f| f.code == "caleitc_childless_age_unknown"));
        // Born 2010: under 18 at the end of 2025 → not met.
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(10_000, 500, Some(10_000), Some(300))];
        input.identity = Some(TaxpayerIdentity {
            person: person("2010-03-01"),
            ..Default::default()
        });
        let ca = compute(&input);
        assert_eq!(line(&ca, "75"), Money::ZERO);
        assert_eq!(
            ca.caleitc.unwrap().disqualifier,
            Some("childless_age_not_met")
        );
        // Born exactly January 1, 2008: the form counts the filer as
        // 18 ("born on or before January 1, 2008").
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(10_000, 500, Some(10_000), Some(300))];
        input.identity = Some(TaxpayerIdentity {
            person: person("2008-01-01"),
            ..Default::default()
        });
        let ca = compute(&input);
        assert_eq!(line(&ca, "75"), m(211));
    }

    #[test]
    fn yctc_age_boundary() {
        // A child who is 6 on December 31 is NOT "younger than six
        // years old as of the last day of the taxable year": YCTC is
        // zero even though the CalEITC still applies.
        let mut input = base(FilingStatus::Mfj);
        input.w2s = vec![w2(28_000, 1_000, Some(28_000), Some(200))];
        input.dependents = vec![kid("Kid One", 6), kid("Kid Two", 3)];
        input.identity = Some(TaxpayerIdentity {
            person: person("1993-04-02"),
            spouse: Some(person("1995-08-20")),
            ..Default::default()
        });
        let ca = compute(&input);
        assert_eq!(line(&ca, "75"), m(207));
        assert_eq!(line(&ca, "76"), m(1_064)); // the 3-year-old carries it
                                               // Both children 6+: no YCTC at all.
        let mut input = base(FilingStatus::Mfj);
        input.w2s = vec![w2(28_000, 1_000, Some(28_000), Some(200))];
        input.dependents = vec![kid("Kid One", 6), kid("Kid Two", 7)];
        input.identity = Some(TaxpayerIdentity {
            person: person("1993-04-02"),
            spouse: Some(person("1995-08-20")),
            ..Default::default()
        });
        let ca = compute(&input);
        assert_eq!(line(&ca, "76"), Money::ZERO);
        // YCTC is per return, not per child: two under-6 children
        // still take one $1,189 maximum.
        let mut input = base(FilingStatus::Mfj);
        input.w2s = vec![w2(20_000, 500, Some(20_000), Some(100))];
        input.dependents = vec![kid("Kid One", 5), kid("Kid Two", 3)];
        input.identity = Some(TaxpayerIdentity {
            person: person("1993-04-02"),
            spouse: Some(person("1995-08-20")),
            ..Default::default()
        });
        let ca = compute(&input);
        assert_eq!(line(&ca, "76"), m(1_189));
    }

    #[test]
    fn withholding_missing_is_incomplete_never_zero() {
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(50_000, 4_000, Some(50_000), None)];
        let ca = compute(&input);
        assert!(!ca.complete);
        assert!(matches!(ca.lines["71"], FactResult::Incomplete));
        assert!(matches!(ca.lines["78"], FactResult::Incomplete));
        assert!(matches!(ca.lines["97"], FactResult::Incomplete));
        // The tax side is unaffected by the payments gap.
        assert_eq!(line(&ca, "64"), m(1_040));
        let f = ca
            .findings
            .iter()
            .find(|f| f.code == "ca_state_withholding_missing")
            .expect("withholding finding");
        assert_eq!(f.entry.as_deref(), Some("w2_01"));
        // NEC with the field absent behaves the same; recorded "0"
        // is a stated fact and completes the line.
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(50_000, 4_000, Some(50_000), Some(1_500))];
        input.f1099_necs = vec![I1099NecInput {
            payer_name: "Example Client LLC".into(),
            payer_ein: None,
            nonemployee_compensation: m(20_000),
            federal_withholding: Money::ZERO,
            expenses: m(2_000),
            state_withholding: None,
        }];
        let ca = compute(&input);
        assert!(matches!(ca.lines["71"], FactResult::Incomplete));
        assert!(ca
            .findings
            .iter()
            .any(|f| f.code == "ca_state_withholding_missing"
                && f.entry.as_deref() == Some("1099nec_01")));
    }

    #[test]
    fn box16_missing_makes_caleitc_incomplete() {
        let mut input = base(FilingStatus::Mfj);
        input.w2s = vec![w2(28_000, 1_000, None, Some(200))];
        input.dependents = vec![kid("Kid One", 5), kid("Kid Two", 3)];
        input.identity = Some(TaxpayerIdentity {
            person: person("1993-04-02"),
            spouse: Some(person("1995-08-20")),
            ..Default::default()
        });
        let ca = compute(&input);
        assert!(matches!(ca.lines["75"], FactResult::Incomplete));
        assert!(matches!(ca.lines["76"], FactResult::Incomplete));
        assert!(ca
            .findings
            .iter()
            .any(|f| f.code == "ca_wages_box16_missing"));
        // But a credit already decided by the AGI gate stays a
        // stated zero without box 16.
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(50_000, 4_000, None, Some(1_500))];
        let ca = compute(&input);
        assert_eq!(line(&ca, "75"), Money::ZERO);
    }

    #[test]
    fn rate_schedule_above_100k_and_bhst() {
        // Taxable income over $100,000 → Schedule X, not the table.
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(150_000, 20_000, Some(150_000), Some(9_000))];
        let ca = compute(&input);
        assert_eq!(ca.line31_method.as_deref(), Some("ca_rate_schedule"));
        // Schedule X at $144,294: 3,201.97 + (144,294 − 72,724) × 9.3%
        // = 3,201.97 + 6,656.01 = $9,857.98.
        assert_eq!(line(&ca, "19"), m(144_294));
        assert_eq!(line(&ca, "31").cents(), 985_798);
        assert_eq!(line(&ca, "62"), Money::ZERO);
        // Behavioral Health Services Tax: 1% of taxable income over
        // $1,000,000.
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(1_200_000, 300_000, Some(1_200_000), Some(80_000))];
        let ca = compute(&input);
        assert_eq!(line(&ca, "19"), m(1_194_294));
        // 1% of the $194,294 excess = $1,942.94 (engine cents; the
        // filed form rounds to whole dollars).
        assert_eq!(line(&ca, "62").cents(), 194_294);
    }

    #[test]
    fn senior_and_blind_exemption_counts() {
        // MFJ, both spouses 65+, one blind: line 7 = 2, line 8 = 1,
        // line 9 = 2 → 5 × $153 = $765 before dependents.
        let mut input = base(FilingStatus::Mfj);
        input.w2s = vec![w2(60_000, 5_000, Some(60_000), Some(2_000))];
        let mut spouse = person("1959-01-15");
        spouse.blind = true;
        input.identity = Some(TaxpayerIdentity {
            person: person("1958-05-01"),
            spouse: Some(spouse),
            ..Default::default()
        });
        let ca = compute(&input);
        assert_eq!(line(&ca, "7"), m(306));
        assert_eq!(line(&ca, "8"), m(153));
        assert_eq!(line(&ca, "9"), m(306));
        assert_eq!(line(&ca, "11"), m(765));
        // A taxpayer who can be claimed as a dependent takes no
        // personal (or blind/senior) exemption credit (line 7
        // instructions).
        let mut input = base(FilingStatus::Single);
        input.w2s = vec![w2(20_000, 1_000, Some(20_000), Some(500))];
        let mut id = TaxpayerIdentity {
            person: person("1958-05-01"),
            ..Default::default()
        };
        id.can_be_claimed_as_dependent = true;
        input.identity = Some(id);
        let ca = compute(&input);
        assert_eq!(line(&ca, "7"), Money::ZERO);
        assert_eq!(line(&ca, "9"), Money::ZERO);
        assert_eq!(line(&ca, "11"), Money::ZERO);
    }

    #[test]
    fn caleitc_mfs_barred() {
        let mut input = base(FilingStatus::Mfs);
        input.w2s = vec![w2(20_000, 1_000, Some(20_000), Some(500))];
        input.dependents = vec![kid("Kid One", 4)];
        let ca = compute(&input);
        assert_eq!(line(&ca, "75"), Money::ZERO);
        assert_eq!(line(&ca, "76"), Money::ZERO);
        assert_eq!(ca.caleitc.unwrap().disqualifier, Some("mfs"));
        assert!(ca.findings.iter().any(|f| f.code == "caleitc_mfs"));
    }
}
