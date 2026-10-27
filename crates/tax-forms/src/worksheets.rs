//! Procedural worksheets (repo rule: worksheets are code, parameters are
//! data). Every step below transcribes the official TY2025 worksheet text:
//!
//! - Schedule D combination + line 21 loss limit: Instructions for
//!   Schedule D (Form 1040), TY2025 (`i1040sd.pdf`).
//! - Capital Loss Carryover Worksheet: same document ("Capital Loss
//!   Carryover Worksheet—Lines 6 and 14").
//! - Qualified Dividends and Capital Gain Tax Worksheet: Instructions
//!   for Form 1040, TY2025 ("Qualified Dividends and Capital Gain Tax
//!   Worksheet—Line 16", `i1040gi.pdf`).
//! - Schedule SE (simplified): Form 1040 Schedule SE, TY2025, Section B
//!   lines 4a, 10–13 as reduced by the brief (no church-employee or
//!   optional-method lines; the W-2 Social Security wage input is the
//!   caller's Box 1 proxy — see docs/KNOWN_LIMITS.md).
//! - Schedule 8812 (simplified): Schedule 8812 (Form 1040), TY2025,
//!   Part I (child tax credit / credit for other dependents, with the
//!   phaseout of lines 9–12 and the Credit Limit Worksheet A of the
//!   Schedule 8812 instructions) and Part II-A (additional child tax
//!   credit); Part II-B (three or more qualifying children) is not
//!   implemented — see docs/KNOWN_LIMITS.md for every simplification.
//! - Schedule 1-A, Part V ONLY (enhanced deduction for seniors):
//!   Schedule 1-A (Form 1040), TY2025, lines 31–38. Parts II (tips),
//!   III (overtime pay), and IV (car loan interest) are NOT implemented
//!   — their inputs do not exist in v1. Part I (modified AGI) is the
//!   caller's AGI: the add-backs for excluded foreign income (§§911 /
//!   931 / 933) do not exist among v1 inputs, so MAGI = AGI.
//!
//! Money is integer cents throughout; the only multiplications are the
//! worksheet's 15% / 20% rates, done with [`Money::mul_bp`] (integer
//! basis-point arithmetic, rounded to the nearest cent). The paper
//! worksheets are figured in whole dollars; the engine keeps cents and
//! selects Tax Table rows by the floored dollar amount (see
//! docs/KNOWN_LIMITS.md).

use tax_core::{bracket_tax, EicTable, FilingStatus, Money, TaxParams, TaxTable};

use crate::{DependentInput, DependentRelationship, PersonIdentity, TaxpayerIdentity};

/// The Schedule D combination of the two 8949 nets, in Schedule D's own
/// line numbering (our simplified Schedule D: line 7 = net short-term
/// from Form 8949 Part I, line 15 = net long-term from Part II; the
/// intervening Schedule D components — K-1 amounts, Forms 6252/4684,
/// capital gain distributions, prior-year carryovers — do not exist in
/// the v1 input set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleDCombination {
    /// Schedule D, line 7: net short-term capital gain or (loss).
    pub net_short: Money,
    /// Schedule D, line 15: net long-term capital gain or (loss).
    pub net_long: Money,
    /// Schedule D, line 16: lines 7 + 15 combined.
    pub combined: Money,
    /// Form 1040, line 7: line 16 when it is a gain (or zero); when
    /// line 16 is a loss, the Schedule D line 21 amount — the smaller of
    /// the loss or the IRC §1211(b) limit, as a negative amount.
    pub line7: Money,
    /// The capital-gain amount the QDCG worksheet consumes on its
    /// line 3: "Enter the smaller of line 15 or line 16 of Schedule D.
    /// If either line 15 or line 16 is blank or a loss, enter -0-."
    pub net_capital_gain: Money,
}

/// Combine the short-term and long-term nets the way Schedule D does,
/// applying the §1211(b) capital-loss limit to Form 1040 line 7.
pub fn combine_schedule_d(
    net_short: Money,
    net_long: Money,
    status: FilingStatus,
    params: &TaxParams,
) -> ScheduleDCombination {
    let combined = net_short + net_long;
    let line7 = if combined.cents() >= 0 {
        combined
    } else {
        // Schedule D, line 21: the smaller of the line 16 loss or the
        // ($3,000 / $1,500 MFS) limit, entered as a loss.
        let limit = params.capital_loss_limit(status);
        Money::from_cents(-((-combined.cents()).min(limit.cents())))
    };
    let net_capital_gain = if net_long.cents() > 0 && combined.cents() > 0 {
        Money::from_cents(net_long.cents().min(combined.cents()))
    } else {
        Money::ZERO
    };
    ScheduleDCombination {
        net_short,
        net_long,
        combined,
        line7,
        net_capital_gain,
    }
}

/// The Capital Loss Carryover Worksheet (Instructions for Schedule D),
/// figured for the current year: returns the short-term and long-term
/// capital loss carryovers to the next year, as positive amounts.
///
/// `taxable_income_unfloored` is Form 1040 line 15 *as the worksheet
/// takes it*: the worksheet's line 1 is the amount from line 15, except
/// that "if the amount would have been a loss if you could enter a
/// negative number on that line", the negative amount is used — so the
/// caller passes AGI minus deductions *before* the line-15 zero floor.
///
/// When Schedule D line 16 is not a loss there is nothing to carry and
/// both carryovers are zero. When it is a loss, the worksheet lines are:
/// 1 = line 15 (above); 2 = the Schedule D line 21 loss, as a positive
/// amount; 3 = lines 1 + 2 combined, floored at 0; 4 = smaller of
/// lines 2 or 3 (the part of the allowed loss actually absorbed).
pub fn capital_loss_carryover(
    comb: &ScheduleDCombination,
    taxable_income_unfloored: Money,
) -> (Money, Money) {
    if comb.combined.cents() >= 0 {
        return (Money::ZERO, Money::ZERO);
    }
    let w1 = taxable_income_unfloored;
    let w2 = -comb.line7; // line 21 loss as a positive amount (>= 0 here)
    let w3 = Money::from_cents((w1 + w2).cents().max(0));
    let w4 = Money::from_cents(w2.cents().min(w3.cents()));

    // Short-term section (lines 5-8), entered when Schedule D line 7 is
    // a loss; otherwise line 5 is -0- and the section yields nothing.
    let w5 = if comb.net_short.cents() < 0 {
        -comb.net_short
    } else {
        Money::ZERO
    };
    let carry_short = if comb.net_short.cents() < 0 {
        let w6 = if comb.net_long.cents() > 0 {
            comb.net_long // any gain from Schedule D, line 15
        } else {
            Money::ZERO
        };
        let w7 = w4 + w6;
        Money::from_cents((w5 - w7).cents().max(0))
    } else {
        Money::ZERO
    };

    // Long-term section (lines 9-13), entered when Schedule D line 15
    // is a loss.
    let carry_long = if comb.net_long.cents() < 0 {
        let w9 = -comb.net_long;
        let w10 = if comb.net_short.cents() > 0 {
            comb.net_short // any gain from Schedule D, line 7
        } else {
            Money::ZERO
        };
        let w11 = Money::from_cents((w4 - w5).cents().max(0));
        let w12 = w10 + w11;
        Money::from_cents((w9 - w12).cents().max(0))
    } else {
        Money::ZERO
    };

    (carry_short, carry_long)
}

/// Tax on an amount by the ordinary methods: the Tax Table when the
/// amount is under $100,000, the Tax Computation Worksheet (marginal
/// brackets) at or above $100,000. This is the choice the 1040
/// instructions prescribe for line 16 and for QDCG worksheet lines 22
/// and 24.
pub fn tax_on_amount(amount: Money, status: FilingStatus, params: &TaxParams) -> Money {
    match TaxTable::load_2025().tax(status, amount) {
        Some(t) => t,
        None => bracket_tax(amount, &params.bracket_table(status)),
    }
}

/// The Qualified Dividends and Capital Gain Tax Worksheet (Instructions
/// for Form 1040, line 16), transcribed line by line. `capital_gain` is
/// the worksheet's line 3 input (see [`ScheduleDCombination::
/// net_capital_gain`]); `qualified_dividends` is Form 1040 line 3a.
pub fn qdcg_tax(
    taxable_income: Money,
    qualified_dividends: Money,
    capital_gain: Money,
    status: FilingStatus,
    params: &TaxParams,
) -> Money {
    let thresholds = params
        .capital_gain_thresholds(status)
        .expect("params must carry capital-gain thresholds for QDCG");
    let zero_rate_up_to = Money::from_cents(thresholds.zero_rate_up_to_cents);
    let twenty_rate_from = Money::from_cents(thresholds.twenty_rate_from_cents);
    let min = |a: Money, b: Money| Money::from_cents(a.cents().min(b.cents()));
    let floor0 = |a: Money| Money::from_cents(a.cents().max(0));

    let l1 = taxable_income;
    let l2 = qualified_dividends;
    let l3 = capital_gain;
    let l4 = l2 + l3;
    let l5 = floor0(l1 - l4);
    let l6 = zero_rate_up_to;
    let l7 = min(l1, l6);
    let l8 = min(l5, l7);
    let l9 = l7 - l8; // amount taxed at 0%
    let l10 = min(l1, l4);
    let l11 = l9;
    let l12 = l10 - l11;
    let l13 = twenty_rate_from;
    let l14 = min(l1, l13);
    let l15 = l5 + l9;
    let l16 = floor0(l14 - l15);
    let l17 = min(l12, l16);
    let l18 = l17.mul_bp(1500); // 15%
    let l19 = l9 + l17;
    let l20 = l10 - l19;
    let l21 = l20.mul_bp(2000); // 20%
    let l22 = tax_on_amount(l5, status, params);
    let l23 = l18 + l21 + l22;
    let l24 = tax_on_amount(l1, status, params);
    min(l23, l24) // line 25: the smaller of lines 23 or 24
}

/// Figure Form 1040 line 16 and report which method produced it:
/// `"qdcg"`, `"tax_table"`, or `"tax_computation"`.
///
/// Selection (Instructions for Form 1040, line 16): use the QDCG
/// worksheet when qualified dividends were reported on line 3a or when
/// Schedule D lines 15 and 16 are both gains (i.e. the QDCG line 3
/// amount is positive) — the Schedule D Tax Worksheet triggers (28%
/// rate / unrecaptured §1250 amounts, Form 4952) cannot arise from the
/// v1 input set. Otherwise, the Tax Table below $100,000 of taxable
/// income and the Tax Computation Worksheet at or above it.
pub fn line16_tax(
    taxable_income: Money,
    qualified_dividends: Money,
    schd_capital_gain: Money,
    status: FilingStatus,
    params: &TaxParams,
) -> (Money, &'static str) {
    if (qualified_dividends.cents() > 0 || schd_capital_gain.cents() > 0)
        && params.capital_gain_thresholds(status).is_some()
    {
        (
            qdcg_tax(
                taxable_income,
                qualified_dividends,
                schd_capital_gain,
                status,
                params,
            ),
            "qdcg",
        )
    } else if taxable_income.cents() < 10_000_000 {
        (tax_on_amount(taxable_income, status, params), "tax_table")
    } else {
        (
            bracket_tax(taxable_income, &params.bracket_table(status)),
            "tax_computation",
        )
    }
}

/// The simplified Schedule SE computation (Short Schedule SE / Section B
/// reduced to the lines the v1 inputs can feed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduleSe {
    /// Schedule SE line 4a: net earnings from self-employment — the
    /// Schedule C net profit × 92.35% (IRC §1402(a)). Zero when the net
    /// profit is zero or a loss: a loss produces no SE tax and no
    /// negative earnings.
    pub net_earnings: Money,
    /// Schedule SE line 10: Social Security part — 12.4% (IRC §1401(a))
    /// of the smaller of net earnings or the wage-base room left after
    /// W-2 Social Security wages.
    pub ss_part: Money,
    /// Schedule SE line 11: Medicare part — 2.9% (IRC §1401(b)) of net
    /// earnings (no wage-base cap).
    pub medicare_part: Money,
    /// Schedule SE line 12: total self-employment tax (lines 10 + 11),
    /// which flows to Schedule 2 line 4 and Form 1040 line 23.
    pub tax: Money,
}

/// Figure Schedule SE tax from the Schedule C net profit.
///
/// `w2_ss_wages` is the total of W-2 Social Security wages (Schedule SE
/// line 8a takes Box 3); the v1 engine passes Σ W-2 Box 1 wages as a
/// documented proxy (docs/KNOWN_LIMITS.md). Components are figured in
/// cents with [`Money::mul_bp`]; the paper Schedule SE rounds each line
/// to whole dollars, so a paper filing can differ by a dollar.
pub fn schedule_se_tax(
    schc_net_profit: Money,
    w2_ss_wages: Money,
    params: &TaxParams,
) -> ScheduleSe {
    if schc_net_profit.cents() <= 0 {
        return ScheduleSe {
            net_earnings: Money::ZERO,
            ss_part: Money::ZERO,
            medicare_part: Money::ZERO,
            tax: Money::ZERO,
        };
    }
    let net_earnings = schc_net_profit.mul_bp(params.se_net_earnings_factor_bp);
    let wage_base = Money::from_cents(params.social_security_wage_base_cents);
    let ss_room = Money::from_cents((wage_base - w2_ss_wages).cents().max(0));
    let ss_taxable = Money::from_cents(net_earnings.cents().min(ss_room.cents()));
    let ss_part = ss_taxable.mul_bp(params.se_ss_rate_bp);
    let medicare_part = net_earnings.mul_bp(params.se_medicare_rate_bp);
    ScheduleSe {
        net_earnings,
        ss_part,
        medicare_part,
        tax: ss_part + medicare_part,
    }
}

/// The deductible part of the SE tax (IRC §164(f); Schedule SE line 13 →
/// Schedule 1 line 15 → Form 1040 line 10): one-half of the SE tax,
/// rounded half up to the cent. SE tax is never negative, so the
/// `(c + 1) / 2` form is exact half-up rounding here.
pub fn deductible_part_of_se_tax(se_tax: Money) -> Money {
    debug_assert!(se_tax.cents() >= 0);
    Money::from_cents((se_tax.cents() + 1) / 2)
}

/// Which Schedule 8812 credit (if any) one stored dependent qualifies
/// for under the tests v1 models: relationship, age under 17 at
/// year-end, and the more-than-half-the-year residency test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependentCreditClass {
    /// Qualifying child for the child tax credit (Schedule 8812 line 4).
    ChildTaxCredit,
    /// Other dependent (Schedule 8812 line 6).
    OtherDependent,
    /// No credit: fails the residency test as modeled in v1.
    None,
}

/// Classify one dependent (Schedule 8812, Part I):
/// - credit for the child tax credit requires a child-type relationship
///   (child, stepchild, foster child), age under 17 on December 31, and
///   having lived with the taxpayer more than half the year;
/// - any other stored dependent who meets the residency test takes the
///   credit-for-other-dependents path (v1 simplification: the support
///   and gross-income tests for qualifying relatives are not modeled —
///   see docs/KNOWN_LIMITS.md);
/// - a dependent who lived with the taxpayer 6 months or fewer gets no
///   credit in v1.
pub fn classify_dependent(dep: &DependentInput) -> DependentCreditClass {
    if dep.months_lived_with_taxpayer <= 6 {
        return DependentCreditClass::None;
    }
    let child_type = matches!(
        dep.relationship,
        DependentRelationship::Child
            | DependentRelationship::Stepchild
            | DependentRelationship::FosterChild
    );
    if child_type && dep.age < 17 {
        DependentCreditClass::ChildTaxCredit
    } else {
        DependentCreditClass::OtherDependent
    }
}

/// The simplified Schedule 8812 computation (TY2025 form, transcribed
/// line by line at the simplified level; money = cents throughout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule8812 {
    /// Line 4: number of qualifying children under age 17.
    pub ctc_children: u32,
    /// Line 6: number of other dependents.
    pub odc_dependents: u32,
    /// Line 8: tentative credit — line 5 ($2,200 × line 4) + line 7
    /// ($500 × line 6), before the phaseout.
    pub tentative_credit: Money,
    /// Line 11: phaseout reduction — 5% of the line 10 amount (MAGI over
    /// the line 9 threshold, rounded up to the next multiple of $1,000).
    pub phaseout_reduction: Money,
    /// Line 12: credit after the phaseout (line 8 − line 11; zero when
    /// the reduction is at least the tentative credit — the form stops
    /// there and allows no credit at all).
    pub credit_after_phaseout: Money,
    /// Line 13: the Credit Limit Worksheet A amount. In v1 this is Form
    /// 1040 line 16: Worksheet A line 1 takes Form 1040 line 18
    /// (= line 16 + line 17), and every other input to the worksheet
    /// (line 17 AMT, the Schedule 3 nonrefundable credits of Worksheet
    /// A line 2, and Credit Limit Worksheet B) is zero or inapplicable
    /// for the v1 input set.
    pub credit_limit: Money,
    /// Line 14 → Form 1040 line 19: the nonrefundable child tax credit
    /// and credit for other dependents — the smaller of lines 12 or 13.
    pub nonrefundable_credit: Money,
    /// Part II-A, line 18a: earned income, as defined by the caller
    /// (v1: total W-2 wages plus a positive Schedule C net profit).
    pub earned_income: Money,
    /// Part II-A, line 27 → Form 1040 line 28: the additional child tax
    /// credit — the smallest of (line 16a: line 12 − line 14), (line
    /// 16b: $1,700 × line 4) and (line 20: 15% of earned income over
    /// $2,500). Part II-B (three or more qualifying children) is not
    /// implemented, so for those filers this can understate the ACTC
    /// (docs/KNOWN_LIMITS.md).
    pub additional_child_tax_credit: Money,
}

/// Figure Schedule 8812 for a return.
///
/// `magi` is the modified AGI of line 3 — v1 takes it to be the AGI
/// itself (the line 2 exclusions, Puerto Rico / Form 2555 / Form 4563
/// income, do not exist in the v1 input set). `income_tax` is Form 1040
/// line 16, the base of Credit Limit Worksheet A (see
/// [`Schedule8812::credit_limit`]). `earned_income` is the Part II-A
/// line 18a amount (v1 definition on [`Schedule8812::earned_income`]).
pub fn schedule_8812(
    dependents: &[DependentInput],
    status: FilingStatus,
    magi: Money,
    income_tax: Money,
    earned_income: Money,
    params: &TaxParams,
) -> Schedule8812 {
    let mut ctc_children: u32 = 0;
    let mut odc_dependents: u32 = 0;
    for dep in dependents {
        match classify_dependent(dep) {
            DependentCreditClass::ChildTaxCredit => ctc_children += 1,
            DependentCreditClass::OtherDependent => odc_dependents += 1,
            DependentCreditClass::None => {}
        }
    }

    // Part I. Line 5 = line 4 × $2,200; line 7 = line 6 × $500;
    // line 8 = lines 5 + 7.
    let tentative_credit = Money::from_cents(
        ctc_children as i64 * params.ctc_per_child_cents
            + odc_dependents as i64 * params.odc_per_dependent_cents,
    );
    // Lines 9–11: the phaseout. Line 10 is the MAGI excess over the
    // line 9 threshold, rounded UP to the next multiple of $1,000
    // (zero when MAGI is at or under the threshold); line 11 is 5% of
    // line 10 ($50 per $1,000 or fraction thereof).
    let threshold = params.ctc_phaseout_threshold(status);
    let excess_cents = (magi - threshold).cents().max(0);
    let line10_cents = if excess_cents == 0 {
        0
    } else {
        // $1,000 = 100_000 cents.
        (excess_cents + 100_000 - 1) / 100_000 * 100_000
    };
    let phaseout_reduction = Money::from_cents(line10_cents).mul_bp(params.ctc_phaseout_rate_bp);
    // Line 12: line 8 − line 11 (the form stops when line 8 is not more
    // than line 11 — the same zero floor).
    let credit_after_phaseout =
        Money::from_cents((tentative_credit - phaseout_reduction).cents().max(0));
    // Line 13 (Credit Limit Worksheet A) and line 14.
    let credit_limit = Money::from_cents(income_tax.cents().max(0));
    let nonrefundable_credit =
        Money::from_cents(credit_after_phaseout.cents().min(credit_limit.cents()));

    // Part II-A. Line 16a = line 12 − line 14 (the unused portion);
    // line 16b = line 4 × $1,700; line 17 = the smaller of the two.
    let unused = credit_after_phaseout - nonrefundable_credit;
    let actc_cap = Money::from_cents(ctc_children as i64 * params.actc_cap_per_child_cents);
    let line17 = Money::from_cents(unused.cents().min(actc_cap.cents()));
    // Lines 19–20: 15% of earned income over $2,500 (floored at zero).
    let ei_excess = Money::from_cents(
        (earned_income - Money::from_cents(params.actc_earned_income_threshold_cents))
            .cents()
            .max(0),
    );
    let line20 = ei_excess.mul_bp(params.actc_rate_bp);
    // Line 27 (for filers not in Part II-B) = the smaller of lines 17
    // or 20.
    let additional_child_tax_credit = Money::from_cents(line17.cents().min(line20.cents()));

    Schedule8812 {
        ctc_children,
        odc_dependents,
        tentative_credit,
        phaseout_reduction,
        credit_after_phaseout,
        credit_limit,
        nonrefundable_credit,
        earned_income,
        additional_child_tax_credit,
    }
}

/// The EIC age test for one dependent (Instructions for Form 1040
/// (2025), line 27, Step 3): under 19 at the end of 2025, or under 24
/// and a full-time student, or permanently and totally disabled at any
/// age.
pub fn eic_age_test(dep: &DependentInput) -> bool {
    dep.age < 19 || (dep.full_time_student && dep.age < 24) || dep.permanently_disabled
}

/// Whether one stored dependent is a qualifying child for the earned
/// income credit under the tests v1 models (Instructions for Form
/// 1040 (2025), line 27, Steps 1 and 3):
/// - relationship: child, stepchild, or foster child. The printed test
///   also covers siblings, half/step siblings, and descendants of any
///   of them; the v1 dependent entry does not distinguish those from
///   other relatives, so `"other"` never counts in v1
///   (docs/KNOWN_LIMITS.md);
/// - age: [`eic_age_test`];
/// - residency: lived with the taxpayer more than half the year
///   (months > 6, the same modeling as Schedule 8812);
/// - the child has an SSN stored (the EIC requires a valid SSN; the
///   SSN-issued-by-the-due-date timing rule is not modeled);
/// - the joint-return test and the "younger than the taxpayer" test
///   are not modeled (v1 stores neither fact).
pub fn classify_eic_child(dep: &DependentInput) -> bool {
    let child_type = matches!(
        dep.relationship,
        DependentRelationship::Child
            | DependentRelationship::Stepchild
            | DependentRelationship::FosterChild
    );
    child_type
        && dep.months_lived_with_taxpayer > 6
        && dep.ssn.as_deref().and_then(crate::normalize_ssn).is_some()
        && eic_age_test(dep)
}

/// The number of stored dependents who are EIC qualifying children.
pub fn eic_qualifying_children(dependents: &[DependentInput]) -> u32 {
    dependents.iter().filter(|d| classify_eic_child(d)).count() as u32
}

/// The no-children EIC age test outcome (Instructions for Form 1040
/// (2025), line 27, Step 4, question 2): `Some(true)` when a date of
/// birth on file falls in the eligible range (at least 25 but under
/// 65 at the end of 2025 — born after 1960-12-31 and before
/// 2001-01-02), `Some(false)` when every date of birth on file is
/// outside it, and `None` when no usable date of birth is stored. On a
/// joint return either spouse meeting the test suffices.
pub fn eic_childless_age_outcome(
    identity: Option<&TaxpayerIdentity>,
    status: FilingStatus,
    params: &TaxParams,
) -> Option<bool> {
    let in_range = |dob: &str| {
        crate::valid_iso_date(dob)
            && dob > params.eic_childless_born_after.as_str()
            && dob < params.eic_childless_born_before.as_str()
    };
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

/// The earned income credit computation (Form 1040 line 27), following
/// EIC Worksheets A and B of the Instructions for Form 1040 (2025).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EarnedIncomeCredit {
    /// Qualifying children counted (EIC tests, see
    /// [`classify_eic_child`]).
    pub qualifying_children: u32,
    /// Earned income as figured by the caller (Worksheet A: the Step 5
    /// amount = Form 1040 line 1z; Worksheet B: wages plus Schedule SE
    /// line 3 net earnings minus the Schedule SE line 13 deductible
    /// part of SE tax).
    pub earned_income: Money,
    /// Investment income as figured by the caller (v1 scope: lines
    /// 2a + 2b + 3b plus the positive part of line 7).
    pub investment_income: Money,
    /// The credit for Form 1040 line 27 (zero when disqualified).
    pub credit: Money,
    /// Why the credit is zero, when a disqualifier decided it:
    /// `"mfs"`, `"investment_income_limit"`, `"no_earned_income"`,
    /// `"over_completed_phaseout"`, `"childless_age_not_met"`, or
    /// `"childless_age_unknown"` (the no-children age test cannot be
    /// determined because no date of birth is stored — v1 takes no
    /// credit rather than guessing; docs/KNOWN_LIMITS.md). `None` when
    /// the table lookup produced the amount.
    pub disqualifier: Option<&'static str>,
}

/// Figure the earned income credit.
///
/// `earned_income` and `agi` feed the worksheets' two table lookups
/// (Worksheets A and B, Parts 2/3 and 5/6, transcribed literally):
/// look the credit up on earned income; when AGI differs from earned
/// income, also look it up on AGI — but only when AGI is at or above
/// the filing-status phaseout threshold for the child count — and
/// take the smaller amount. Before any lookup, the worksheet gates
/// apply (Steps 1, 2, 4, 5 and Worksheet B line 5): married filing
/// separately takes no credit in v1 (the separated-spouse exception
/// is not modeled); investment income over the IRC §32(i) limit
/// disqualifies; AGI or earned income at or above the Rev. Proc.
/// 2024-40 completed phaseout amount for the child count yields zero;
/// and with no qualifying children the age-25-to-64 test must be met
/// by a stored date of birth.
pub fn earned_income_credit(
    dependents: &[DependentInput],
    status: FilingStatus,
    identity: Option<&TaxpayerIdentity>,
    earned_income: Money,
    agi: Money,
    investment_income: Money,
    params: &TaxParams,
) -> EarnedIncomeCredit {
    let children = eic_qualifying_children(dependents);
    let done = |credit: Money, disqualifier: Option<&'static str>| EarnedIncomeCredit {
        qualifying_children: children,
        earned_income,
        investment_income,
        credit,
        disqualifier,
    };
    if status == FilingStatus::Mfs {
        return done(Money::ZERO, Some("mfs"));
    }
    if investment_income.cents() > params.eic_investment_income_limit_cents {
        return done(Money::ZERO, Some("investment_income_limit"));
    }
    if earned_income.cents() <= 0 {
        return done(Money::ZERO, Some("no_earned_income"));
    }
    let completed = params.eic_completed_phaseout(status, children);
    if agi.cents() >= completed.cents() || earned_income.cents() >= completed.cents() {
        return done(Money::ZERO, Some("over_completed_phaseout"));
    }
    if children == 0 {
        match eic_childless_age_outcome(identity, status, params) {
            Some(true) => {}
            Some(false) => return done(Money::ZERO, Some("childless_age_not_met")),
            None => return done(Money::ZERO, Some("childless_age_unknown")),
        }
    }
    let table = EicTable::load_2025();
    let mfj = status == FilingStatus::Mfj;
    let on_earned = table.credit(mfj, children, earned_income);
    // Worksheet lines 5-6 / 10-11: when AGI equals earned income, or
    // AGI is below the phaseout threshold, the earned-income lookup IS
    // the credit (no second lookup); otherwise take the smaller of
    // the two lookups.
    let credit = if agi == earned_income || agi < params.eic_phaseout_threshold(status, children) {
        on_earned
    } else {
        let on_agi = table.credit(mfj, children, agi);
        Money::from_cents(on_earned.cents().min(on_agi.cents()))
    };
    done(credit, None)
}

/// How many people on this return can take the Schedule 1-A, Part V
/// enhanced deduction for seniors (lines 36a/36b): 0, 1, or 2.
///
/// A person counts when BOTH conditions on the form are met: they
/// "have a valid social security number" (v1 proxy: a normalized SSN
/// is stored — the same standing proxy as the EIC; SSN validity and
/// issuance timing are not modeled) AND they "were born before
/// January 2, 1961" ([`PersonIdentity::is_age_65`] against the shared
/// TY2025 cutoff — Schedule 1-A prints the same date as the
/// standard-deduction addition). The spouse counts only on a joint
/// return: Part V's caution states "If married, you must file jointly
/// to claim this deduction", so married filing separately takes NO
/// deduction for either spouse (the spouse block is not even consulted)
/// and a spouse stored on a single / head-of-household return is data
/// noise, consistent with the standard-deduction additions. With no
/// identity (or no DOB) stored, the answer is 0 — the deduction is a
/// stated zero, surfaced by a `tax return validate` info finding.
pub fn senior_bonus_eligible_persons(
    identity: Option<&TaxpayerIdentity>,
    status: FilingStatus,
    params: &TaxParams,
) -> u32 {
    let Some(id) = identity else { return 0 };
    if status == FilingStatus::Mfs {
        return 0;
    }
    let eligible = |p: &PersonIdentity| p.is_age_65(params) && p.ssn.is_some();
    let mut n = u32::from(eligible(&id.person));
    if status == FilingStatus::Mfj {
        if let Some(spouse) = &id.spouse {
            n += u32::from(eligible(spouse));
        }
    }
    n
}

/// Schedule 1-A (Form 1040) TY2025, Part V — Enhanced Deduction for
/// Seniors, lines 31–37, and the resulting deduction.
///
/// The phaseout is PER PERSON, which the form's line structure makes
/// explicit: line 33 is the modified AGI over the line 32 threshold,
/// line 34 is 6% of that excess, and line 35 is "$6,000 minus line
/// 34" — a single per-person amount that lines 36a (you) and 36b
/// (spouse, MFJ only) each take in full. A joint return with both
/// spouses eligible therefore gets 2 × line 35, NOT (2 × $6,000)
/// minus one phaseout. Line 37 is the sum of 36a and 36b; the
/// Schedule 1-A line 38 total that flows to Form 1040 line 13b equals
/// line 37 in v1 (Parts II–IV are not implemented).
///
/// `magi` is the caller's modified AGI — AGI in v1 (Part I add-backs
/// for excluded foreign income do not exist among v1 inputs). The
/// form is figured in whole dollars; the engine keeps cents and the
/// 6% multiplication rounds to the nearest cent ([`Money::mul_bp`]),
/// the same treatment as every other worksheet here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeniorBonus {
    /// Schedule 1-A line 31: modified AGI (AGI in v1).
    pub magi: Money,
    /// Schedule 1-A line 35: the per-person amount after the phaseout
    /// ($6,000 at or under the threshold; $0 once the excess reaches
    /// $100,000).
    pub per_person: Money,
    /// Lines 36a/36b: how many people take the line 35 amount (0–2).
    pub eligible_persons: u32,
    /// Schedule 1-A line 37 (= line 38 total in v1): the deduction
    /// that flows to Form 1040 line 13b.
    pub deduction: Money,
    /// Why the deduction is structurally zero regardless of MAGI:
    /// `"mfs"` (Part V caution — married taxpayers must file jointly)
    /// or `"no_eligible_person"` (no stored person is 65+ with an
    /// SSN). A deduction phased out to zero by MAGI is arithmetic,
    /// not a disqualifier (`None`).
    pub disqualifier: Option<&'static str>,
}

pub fn schedule_1a_senior_deduction(
    identity: Option<&TaxpayerIdentity>,
    status: FilingStatus,
    magi: Money,
    params: &TaxParams,
) -> SeniorBonus {
    let eligible_persons = senior_bonus_eligible_persons(identity, status, params);
    let disqualifier = if status == FilingStatus::Mfs {
        Some("mfs")
    } else if eligible_persons == 0 {
        Some("no_eligible_person")
    } else {
        None
    };
    // Lines 31–35: excess over the threshold, 6% of it, subtracted
    // from the $6,000 per-person amount, floored at zero.
    let threshold = params.senior_bonus_phaseout_threshold(status);
    let excess = Money::from_cents((magi.cents() - threshold.cents()).max(0));
    let reduction = excess.mul_bp(params.senior_bonus_phaseout_rate_bp);
    let per_person =
        Money::from_cents((params.senior_bonus_per_person_cents - reduction.cents()).max(0));
    SeniorBonus {
        magi,
        per_person,
        eligible_persons,
        deduction: Money::from_cents(per_person.cents() * i64::from(eligible_persons)),
        disqualifier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> TaxParams {
        TaxParams::load_2025().unwrap()
    }
    fn m(dollars: i64) -> Money {
        Money::from_cents(dollars * 100)
    }

    // -- Schedule D combination --------------------------------------

    #[test]
    fn schd_gain_flows_through_in_full() {
        let p = params();
        let c = combine_schedule_d(m(2_000), m(-500), FilingStatus::Single, &p);
        assert_eq!(c.combined, m(1_500));
        assert_eq!(c.line7, m(1_500));
        // Sch D line 15 is a loss -> QDCG line 3 is -0-.
        assert_eq!(c.net_capital_gain, Money::ZERO);
    }

    #[test]
    fn schd_qdcg_gain_is_smaller_of_lines_15_and_16() {
        let p = params();
        // ST loss eats into the LT gain: QDCG sees min(5000, 4000).
        let c = combine_schedule_d(m(-1_000), m(5_000), FilingStatus::Single, &p);
        assert_eq!(c.line7, m(4_000));
        assert_eq!(c.net_capital_gain, m(4_000));
        // Pure LT gain: both lines are the gain itself.
        let c = combine_schedule_d(Money::ZERO, m(10_000), FilingStatus::Single, &p);
        assert_eq!(c.net_capital_gain, m(10_000));
    }

    #[test]
    fn schd_loss_is_limited_on_line_7() {
        let p = params();
        let c = combine_schedule_d(m(-6_000), m(-4_000), FilingStatus::Single, &p);
        assert_eq!(c.combined, m(-10_000));
        assert_eq!(c.line7, m(-3_000), "IRC 1211(b) limit, single");
        let c = combine_schedule_d(m(-6_000), m(-4_000), FilingStatus::Mfs, &p);
        assert_eq!(c.line7, m(-1_500), "IRC 1211(b) limit, MFS");
        // A loss under the limit passes through whole.
        let c = combine_schedule_d(m(-1_000), Money::ZERO, FilingStatus::Single, &p);
        assert_eq!(c.line7, m(-1_000));
    }

    // -- Capital Loss Carryover Worksheet ----------------------------

    #[test]
    fn carryover_gain_case_is_zero() {
        let p = params();
        let c = combine_schedule_d(m(2_000), m(-500), FilingStatus::Single, &p);
        assert_eq!(
            capital_loss_carryover(&c, m(37_250)),
            (Money::ZERO, Money::ZERO)
        );
    }

    #[test]
    fn carryover_small_loss_fully_absorbed() {
        let p = params();
        // $1,000 ST loss, plenty of other income: deducted in full this
        // year (line 21 = line 16), nothing left to carry.
        let c = combine_schedule_d(m(-1_000), Money::ZERO, FilingStatus::Single, &p);
        assert_eq!(
            capital_loss_carryover(&c, m(20_000)),
            (Money::ZERO, Money::ZERO)
        );
    }

    #[test]
    fn carryover_big_short_term_loss() {
        let p = params();
        // $6,000 ST loss: $3,000 allowed this year, $3,000 carries.
        let c = combine_schedule_d(m(-6_000), Money::ZERO, FilingStatus::Single, &p);
        assert_eq!(
            capital_loss_carryover(&c, m(30_000)),
            (m(3_000), Money::ZERO)
        );
    }

    #[test]
    fn carryover_mixed_losses_short_absorbed_first() {
        let p = params();
        // Golden case 003's Schedule D: ST -$6,000, LT -$4,000, taxable
        // income $41,250. The $3,000 allowance is consumed against the
        // short-term loss first: carryovers ST $3,000 / LT $4,000.
        let c = combine_schedule_d(m(-6_000), m(-4_000), FilingStatus::Single, &p);
        assert_eq!(capital_loss_carryover(&c, m(41_250)), (m(3_000), m(4_000)));
    }

    #[test]
    fn carryover_lt_loss_offset_by_st_gain() {
        let p = params();
        // ST gain $2,000 + LT loss $5,000: combined loss $3,000, all of
        // it allowed; the worksheet credits the ST gain (line 10) and
        // the absorbed allowance (line 11) against the LT loss, so
        // nothing carries.
        let c = combine_schedule_d(m(2_000), m(-5_000), FilingStatus::Single, &p);
        assert_eq!(c.line7, m(-3_000));
        assert_eq!(
            capital_loss_carryover(&c, m(20_000)),
            (Money::ZERO, Money::ZERO)
        );
    }

    #[test]
    fn carryover_unused_allowance_when_taxable_would_be_negative() {
        let p = params();
        // ST loss $8,000; taxable income would be -$5,750 before the
        // line-15 zero floor. The allowed $3,000 absorbed nothing
        // (worksheet lines 1-4 give line 4 = 0), so the entire $8,000
        // short-term loss carries.
        let c = combine_schedule_d(m(-8_000), Money::ZERO, FilingStatus::Single, &p);
        assert_eq!(
            capital_loss_carryover(&c, m(-5_750)),
            (m(8_000), Money::ZERO)
        );
    }

    // -- Tax on amount / line 16 selection ---------------------------

    #[test]
    fn tax_on_amount_uses_table_below_100k_and_brackets_above() {
        let p = params();
        // Tax Table row 34,250-34,300, single = $3,875.
        assert_eq!(tax_on_amount(m(34_250), FilingStatus::Single, &p), m(3_875));
        // At $100,000 the Tax Computation Worksheet takes over; it
        // equals the bracket tax by construction.
        let brackets = p.bracket_table(FilingStatus::Single);
        assert_eq!(
            tax_on_amount(m(100_000), FilingStatus::Single, &p),
            bracket_tax(m(100_000), &brackets)
        );
        assert_eq!(
            line16_tax(
                m(150_000),
                Money::ZERO,
                Money::ZERO,
                FilingStatus::Single,
                &p
            ),
            (bracket_tax(m(150_000), &brackets), "tax_computation")
        );
        assert_eq!(
            line16_tax(
                m(34_250),
                Money::ZERO,
                Money::ZERO,
                FilingStatus::Single,
                &p
            )
            .1,
            "tax_table"
        );
    }

    // -- QDCG worksheet ----------------------------------------------

    #[test]
    fn qdcg_without_preferential_income_reduces_to_ordinary_tax() {
        let p = params();
        // No qualified dividends and no capital gain: the worksheet's
        // line 25 (smaller of 23 or 24) is the ordinary tax on line 1.
        let tax = qdcg_tax(
            m(37_250),
            Money::ZERO,
            Money::ZERO,
            FilingStatus::Single,
            &p,
        );
        assert_eq!(tax, tax_on_amount(m(37_250), FilingStatus::Single, &p));
        assert_eq!(tax, m(4_235)); // Tax Table row 37,250-37,300, single
    }

    #[test]
    fn qdcg_zero_percent_band() {
        let p = params();
        // Golden case 002: taxable $37,250, qualified dividends $600,
        // QDCG capital gain $0 (Sch D line 15 is a loss). The $600 sits
        // under the $48,350 threshold, taxed at 0%; the rest is the Tax
        // Table on $36,650 = $4,163.
        let (tax, method) = line16_tax(m(37_250), m(600), Money::ZERO, FilingStatus::Single, &p);
        assert_eq!(method, "qdcg");
        assert_eq!(tax, m(4_163));
    }

    #[test]
    fn qdcg_fifteen_percent_band() {
        let p = params();
        // Golden case 004: taxable $79,250; preferential $15,000 all
        // above the $48,350 zero-rate threshold (ordinary part $64,250),
        // all at 15%: Tax Table on $64,250 ($9,055) + 15% x $15,000
        // ($2,250) = $11,305.
        let (tax, method) = line16_tax(m(79_250), m(5_000), m(10_000), FilingStatus::Single, &p);
        assert_eq!(method, "qdcg");
        assert_eq!(tax, m(11_305));
    }

    #[test]
    fn qdcg_twenty_percent_band() {
        let p = params();
        // Single, $600,000 taxable, all of it long-term gain: $48,350
        // at 0%, ($533,400 - $48,350) = $485,050 at 15% = $72,757.50,
        // remaining $66,600 at 20% = $13,320. Total $86,077.50.
        let tax = qdcg_tax(
            m(600_000),
            Money::ZERO,
            m(600_000),
            FilingStatus::Single,
            &p,
        );
        assert_eq!(tax, Money::from_cents(8_607_750));
        // And it beats the all-ordinary figure (worksheet line 25 min).
        assert!(tax < tax_on_amount(m(600_000), FilingStatus::Single, &p));
    }

    #[test]
    fn qdcg_never_exceeds_ordinary_tax() {
        let p = params();
        // Tiny preferential amount at a high income: worksheet result
        // is capped by line 24 (ordinary tax on all of line 1).
        let (tax, _) = line16_tax(m(99_000), m(100), Money::ZERO, FilingStatus::Single, &p);
        assert!(tax <= tax_on_amount(m(99_000), FilingStatus::Single, &p));
    }

    // -- Schedule SE ---------------------------------------------------

    #[test]
    fn se_tax_golden_case_005_figures() {
        let p = params();
        // Case 005: Sch C net $18,000, W-2 wages $50,000.
        // Earnings = 18000 x 0.9235 = $16,623.00; SS room = 176100 -
        // 50000 = $126,100, so all earnings are SS-taxable:
        // SS = 16623 x 12.4% = $2,061.252 -> $2,061.25;
        // Medicare = 16623 x 2.9% = $482.067 -> $482.07.
        let se = schedule_se_tax(m(18_000), m(50_000), &p);
        assert_eq!(se.net_earnings, Money::from_cents(1_662_300));
        assert_eq!(se.ss_part, Money::from_cents(206_125));
        assert_eq!(se.medicare_part, Money::from_cents(48_207));
        assert_eq!(se.tax, Money::from_cents(254_332));
        // Half of $2,543.32 is exactly $1,271.66.
        assert_eq!(
            deductible_part_of_se_tax(se.tax),
            Money::from_cents(127_166)
        );
    }

    #[test]
    fn se_tax_wage_base_cap_zeroes_ss_part_only() {
        let p = params();
        // W-2 wages $200,000 already exceed the $176,100 base: no SS
        // room remains, but Medicare has no cap.
        let se = schedule_se_tax(m(18_000), m(200_000), &p);
        assert_eq!(se.net_earnings, Money::from_cents(1_662_300));
        assert_eq!(se.ss_part, Money::ZERO);
        assert_eq!(se.medicare_part, Money::from_cents(48_207));
        assert_eq!(se.tax, Money::from_cents(48_207));
        // Partial room: wages $170,000 leave $6,100 of base; SS applies
        // to $6,100 only: 6100 x 12.4% = $756.40.
        let se = schedule_se_tax(m(18_000), m(170_000), &p);
        assert_eq!(se.ss_part, Money::from_cents(75_640));
    }

    #[test]
    fn se_tax_zero_or_loss_profit_is_zero() {
        let p = params();
        for profit in [Money::ZERO, m(-3_000)] {
            let se = schedule_se_tax(profit, m(50_000), &p);
            assert_eq!(se.net_earnings, Money::ZERO);
            assert_eq!(se.tax, Money::ZERO);
        }
    }

    // -- Schedule 8812 -------------------------------------------------

    fn dep(relationship: DependentRelationship, age: u32, months: u32) -> DependentInput {
        DependentInput {
            name: "Test Dependent".into(),
            ssn: None,
            relationship,
            age,
            months_lived_with_taxpayer: months,
            full_time_student: false,
            permanently_disabled: false,
        }
    }

    fn dep_ssn(relationship: DependentRelationship, age: u32, months: u32) -> DependentInput {
        let mut d = dep(relationship, age, months);
        d.ssn = Some("123456789".to_string());
        d
    }

    fn identity_with_dobs(taxpayer: Option<&str>, spouse: Option<&str>) -> TaxpayerIdentity {
        let person = |dob: Option<&str>| crate::PersonIdentity {
            date_of_birth: dob.map(str::to_string),
            ..Default::default()
        };
        TaxpayerIdentity {
            person: person(taxpayer),
            spouse: spouse.map(|dob| person(Some(dob))),
            ..Default::default()
        }
    }

    #[test]
    fn classify_dependent_boundaries() {
        use DependentRelationship::*;
        // Age test: under 17 on December 31 qualifies; 17 does not.
        assert_eq!(
            classify_dependent(&dep(Child, 16, 12)),
            DependentCreditClass::ChildTaxCredit
        );
        assert_eq!(
            classify_dependent(&dep(Child, 17, 12)),
            DependentCreditClass::OtherDependent
        );
        // Stepchildren and foster children are child-type relationships.
        assert_eq!(
            classify_dependent(&dep(Stepchild, 10, 12)),
            DependentCreditClass::ChildTaxCredit
        );
        assert_eq!(
            classify_dependent(&dep(FosterChild, 10, 12)),
            DependentCreditClass::ChildTaxCredit
        );
        // Residency test: more than half the year — 7 months qualifies,
        // 6 does not (and no ODC either).
        assert_eq!(
            classify_dependent(&dep(Child, 10, 7)),
            DependentCreditClass::ChildTaxCredit
        );
        assert_eq!(
            classify_dependent(&dep(Child, 10, 6)),
            DependentCreditClass::None
        );
        assert_eq!(
            classify_dependent(&dep(Other, 40, 6)),
            DependentCreditClass::None
        );
        // Relationship "other" is ODC at any age.
        assert_eq!(
            classify_dependent(&dep(Other, 10, 12)),
            DependentCreditClass::OtherDependent
        );
        assert_eq!(
            classify_dependent(&dep(Other, 40, 12)),
            DependentCreditClass::OtherDependent
        );
    }

    #[test]
    fn sched8812_golden_case_006_figures() {
        let p = params();
        // Case 006: MFJ, AGI $90,000, line 16 $6,546, earned income
        // $90,000; children 7 and 4 (CTC) + other dependent 19 (ODC).
        let deps = vec![
            dep(DependentRelationship::Child, 7, 12),
            dep(DependentRelationship::Child, 4, 12),
            dep(DependentRelationship::Other, 19, 12),
        ];
        let s = schedule_8812(&deps, FilingStatus::Mfj, m(90_000), m(6_546), m(90_000), &p);
        assert_eq!(s.ctc_children, 2);
        assert_eq!(s.odc_dependents, 1);
        assert_eq!(s.tentative_credit, m(4_900), "2 x $2,200 + $500");
        assert_eq!(s.phaseout_reduction, Money::ZERO, "MAGI under $400,000");
        assert_eq!(s.credit_after_phaseout, m(4_900));
        assert_eq!(
            s.credit_limit,
            m(6_546),
            "Credit Limit Worksheet A = line 16"
        );
        assert_eq!(s.nonrefundable_credit, m(4_900));
        assert_eq!(
            s.additional_child_tax_credit,
            Money::ZERO,
            "credit fully absorbed: line 16a is zero"
        );
    }

    #[test]
    fn sched8812_golden_case_007_figures() {
        let p = params();
        // Case 007: HoH, AGI $30,000, line 16 $638, earned income
        // $30,000; one child age 5.
        let deps = vec![dep(DependentRelationship::Child, 5, 12)];
        let s = schedule_8812(&deps, FilingStatus::Hoh, m(30_000), m(638), m(30_000), &p);
        assert_eq!(s.tentative_credit, m(2_200));
        assert_eq!(s.credit_after_phaseout, m(2_200));
        assert_eq!(s.nonrefundable_credit, m(638), "limited by line 16");
        // ACTC: unused 2200 - 638 = $1,562, under the $1,700 cap and
        // under 15% x (30000 - 2500) = $4,125.
        assert_eq!(s.additional_child_tax_credit, m(1_562));
    }

    #[test]
    fn sched8812_phaseout_mechanics() {
        let p = params();
        let deps = vec![dep(DependentRelationship::Child, 5, 12)];
        let big_tax = m(100_000);
        let ei = m(500_000);
        // MAGI exactly at the $200,000 threshold: no reduction.
        let s = schedule_8812(&deps, FilingStatus::Single, m(200_000), big_tax, ei, &p);
        assert_eq!(s.phaseout_reduction, Money::ZERO);
        assert_eq!(s.credit_after_phaseout, m(2_200));
        // $1 over: the excess rounds up to $1,000 -> $50 reduction.
        let s = schedule_8812(&deps, FilingStatus::Single, m(200_001), big_tax, ei, &p);
        assert_eq!(s.phaseout_reduction, m(50));
        // Exactly $1,000 over: still one $1,000 unit -> $50.
        let s = schedule_8812(&deps, FilingStatus::Single, m(201_000), big_tax, ei, &p);
        assert_eq!(s.phaseout_reduction, m(50));
        // $1,001 over: two units -> $100.
        let s = schedule_8812(&deps, FilingStatus::Single, m(201_001), big_tax, ei, &p);
        assert_eq!(s.phaseout_reduction, m(100));
        // MFJ threshold is $400,000.
        let s = schedule_8812(&deps, FilingStatus::Mfj, m(400_001), big_tax, ei, &p);
        assert_eq!(s.phaseout_reduction, m(50));
        // A reduction at least the tentative credit stops the credit
        // entirely (form line 12): $500,000 single -> excess $300,000
        // -> $15,000 reduction; no nonrefundable credit and no ACTC.
        let s = schedule_8812(&deps, FilingStatus::Single, m(500_000), big_tax, ei, &p);
        assert_eq!(s.credit_after_phaseout, Money::ZERO);
        assert_eq!(s.nonrefundable_credit, Money::ZERO);
        assert_eq!(s.additional_child_tax_credit, Money::ZERO);
    }

    #[test]
    fn sched8812_actc_caps() {
        let p = params();
        let deps = vec![dep(DependentRelationship::Child, 5, 12)];
        // Per-child cap binds: zero tax liability leaves the full
        // $2,200 unused, earned income is ample -> ACTC = $1,700.
        let s = schedule_8812(
            &deps,
            FilingStatus::Single,
            m(200_000),
            Money::ZERO,
            m(200_000),
            &p,
        );
        assert_eq!(s.nonrefundable_credit, Money::ZERO);
        assert_eq!(s.additional_child_tax_credit, m(1_700));
        // Earned-income formula binds: 15% x (10000 - 2500) = $1,125.
        let s = schedule_8812(
            &deps,
            FilingStatus::Single,
            m(10_000),
            Money::ZERO,
            m(10_000),
            &p,
        );
        assert_eq!(s.additional_child_tax_credit, m(1_125));
        // Earned income at or under $2,500: no ACTC.
        let s = schedule_8812(
            &deps,
            FilingStatus::Single,
            m(2_500),
            Money::ZERO,
            m(2_500),
            &p,
        );
        assert_eq!(s.additional_child_tax_credit, Money::ZERO);
        // ODC-only family: line 16b is $0 (no qualifying children), so
        // the ACTC is always zero even with unused credit.
        let deps = vec![
            dep(DependentRelationship::Other, 40, 12),
            dep(DependentRelationship::Other, 38, 12),
        ];
        let s = schedule_8812(
            &deps,
            FilingStatus::Single,
            m(30_000),
            Money::ZERO,
            m(30_000),
            &p,
        );
        assert_eq!(s.tentative_credit, m(1_000), "2 x $500 ODC");
        assert_eq!(s.nonrefundable_credit, Money::ZERO);
        assert_eq!(s.additional_child_tax_credit, Money::ZERO);
    }

    // -- Earned income credit -----------------------------------------

    #[test]
    fn eic_classify_age_and_relationship_tests() {
        use DependentRelationship::*;
        // Under 19 at year-end counts; 19 does not (unless student).
        assert!(classify_eic_child(&dep_ssn(Child, 18, 12)));
        assert!(!classify_eic_child(&dep_ssn(Child, 19, 12)));
        // A 20-year-old full-time student counts; a 24-year-old does
        // not (the test is under 24).
        let mut student = dep_ssn(Child, 20, 12);
        student.full_time_student = true;
        assert!(classify_eic_child(&student));
        let mut student24 = dep_ssn(Child, 24, 12);
        student24.full_time_student = true;
        assert!(!classify_eic_child(&student24));
        // Permanently and totally disabled: any age.
        let mut disabled = dep_ssn(Child, 30, 12);
        disabled.permanently_disabled = true;
        assert!(classify_eic_child(&disabled));
        // Stepchildren / foster children count; "other" never does in
        // v1; residency (more than half the year) and the SSN are both
        // required.
        assert!(classify_eic_child(&dep_ssn(Stepchild, 10, 12)));
        assert!(classify_eic_child(&dep_ssn(FosterChild, 10, 12)));
        assert!(!classify_eic_child(&dep_ssn(Other, 10, 12)));
        assert!(!classify_eic_child(&dep_ssn(Child, 10, 6)));
        assert!(!classify_eic_child(&dep(Child, 10, 12)), "no SSN stored");
    }

    #[test]
    fn eic_golden_case_007_shape() {
        let p = params();
        // HoH, one child age 5 with an SSN, earned income = AGI =
        // $30,000, no investment income: EIC Table row 30,000-30,050,
        // 1 child, single/head-of-household columns = $3,261.
        let deps = vec![dep_ssn(DependentRelationship::Child, 5, 12)];
        let e = earned_income_credit(
            &deps,
            FilingStatus::Hoh,
            None,
            m(30_000),
            m(30_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.qualifying_children, 1);
        assert_eq!(e.credit, m(3_261));
        assert_eq!(e.disqualifier, None);
    }

    #[test]
    fn eic_two_kids_and_mfj_column_difference() {
        let p = params();
        let deps = vec![
            dep_ssn(DependentRelationship::Child, 9, 12),
            dep_ssn(DependentRelationship::Child, 6, 12),
        ];
        // Golden case 009: single, earned = AGI = $38,000, 2 children
        // -> $4,061 (EIC Table row 38,000-38,050).
        let e = earned_income_credit(
            &deps,
            FilingStatus::Single,
            None,
            m(38_000),
            m(38_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.qualifying_children, 2);
        assert_eq!(e.credit, m(4_061));
        // Same income filing jointly: the MFJ phaseout threshold is
        // $30,470 instead of $23,350, so the credit is $5,561.
        let e = earned_income_credit(
            &deps,
            FilingStatus::Mfj,
            None,
            m(38_000),
            m(38_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.credit, m(5_561));
    }

    #[test]
    fn eic_agi_lookup_min_rule() {
        let p = params();
        let deps = vec![dep_ssn(DependentRelationship::Child, 5, 12)];
        // Earned income $30,000 (credit on EI = $3,261) but AGI is
        // $40,000 because of $10,000 of interest: AGI is at/above the
        // $23,350 threshold, so the credit is the smaller of the two
        // lookups; the $40,000 lookup (row 40,000-40,050, 1 child,
        // single columns) is $1,663.
        let e = earned_income_credit(
            &deps,
            FilingStatus::Hoh,
            None,
            m(30_000),
            m(40_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.credit, m(1_663));
        // But when AGI is below the phaseout threshold, the AGI lookup
        // is NOT taken (Worksheet A, Part 3): earned income $5,000,
        // AGI $2,000 (a capital loss) — the credit stays the earned
        // income lookup, $1,709 (row 5,000-5,050, 1 child).
        let e = earned_income_credit(
            &deps,
            FilingStatus::Single,
            None,
            m(5_000),
            m(2_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.credit, m(1_709));
    }

    #[test]
    fn eic_disqualifiers() {
        let p = params();
        let deps = vec![dep_ssn(DependentRelationship::Child, 5, 12)];
        // MFS: no credit in v1.
        let e = earned_income_credit(
            &deps,
            FilingStatus::Mfs,
            None,
            m(30_000),
            m(30_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.credit, Money::ZERO);
        assert_eq!(e.disqualifier, Some("mfs"));
        // Investment income $11,951 (one dollar over the section 32(i)
        // limit) disqualifies entirely; $11,950 does not. (The $638
        // here is because at $11,950 of interest the AGI lookup —
        // AGI $41,950, row 41,950-42,000, 1 child — controls.)
        let e = earned_income_credit(
            &deps,
            FilingStatus::Hoh,
            None,
            m(30_000),
            m(41_951),
            m(11_951),
            &p,
        );
        assert_eq!(e.credit, Money::ZERO);
        assert_eq!(e.disqualifier, Some("investment_income_limit"));
        let e = earned_income_credit(
            &deps,
            FilingStatus::Hoh,
            None,
            m(30_000),
            m(41_950),
            m(11_950),
            &p,
        );
        assert!(e.credit.cents() > 0);
        assert_eq!(e.disqualifier, None);
        // AGI at the completed phaseout amount ($50,434, 1 child,
        // non-MFJ) yields zero even with low earned income.
        let e = earned_income_credit(
            &deps,
            FilingStatus::Single,
            None,
            m(10_000),
            m(50_434),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.credit, Money::ZERO);
        assert_eq!(e.disqualifier, Some("over_completed_phaseout"));
        // No earned income: no credit.
        let e = earned_income_credit(
            &deps,
            FilingStatus::Single,
            None,
            Money::ZERO,
            m(5_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.disqualifier, Some("no_earned_income"));
        // Golden case 006 shape: MFJ AGI $90,000 is over the completed
        // phaseout amount whatever the child count.
        let e = earned_income_credit(
            &deps,
            FilingStatus::Mfj,
            None,
            m(90_000),
            m(90_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.credit, Money::ZERO);
    }

    #[test]
    fn eic_childless_age_test() {
        let p = params();
        // Single, no children, earned = AGI = $10,000: the maximum
        // no-children credit, $649 (EIC Table plateau) — when a date
        // of birth in the 25-64 range is stored (born 1990-06-15).
        let id = identity_with_dobs(Some("1990-06-15"), None);
        let e = earned_income_credit(
            &[],
            FilingStatus::Single,
            Some(&id),
            m(10_000),
            m(10_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.qualifying_children, 0);
        assert_eq!(e.credit, m(649));
        // Born 1958 (age 67 at year-end): outside the range.
        let id = identity_with_dobs(Some("1958-03-01"), None);
        let e = earned_income_credit(
            &[],
            FilingStatus::Single,
            Some(&id),
            m(10_000),
            m(10_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.credit, Money::ZERO);
        assert_eq!(e.disqualifier, Some("childless_age_not_met"));
        // No identity/DOB stored: v1 takes no credit rather than
        // guessing (documented modeling choice).
        let e = earned_income_credit(
            &[],
            FilingStatus::Single,
            None,
            m(10_000),
            m(10_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.credit, Money::ZERO);
        assert_eq!(e.disqualifier, Some("childless_age_unknown"));
        // Joint return: either spouse in range suffices (taxpayer
        // born 1950, spouse born 1995).
        let id = identity_with_dobs(Some("1950-01-01"), Some("1995-05-05"));
        let e = earned_income_credit(
            &[],
            FilingStatus::Mfj,
            Some(&id),
            m(10_000),
            m(10_000),
            Money::ZERO,
            &p,
        );
        assert_eq!(e.credit, m(649));
        // Boundaries from the instructions: born 1961-01-01 (64) is
        // in; born 1960-12-31 (65) is out; born 2001-01-01 is in
        // (treated as 25); born 2001-01-02 (24) is out.
        for (dob, ok) in [
            ("1961-01-01", true),
            ("1960-12-31", false),
            ("2001-01-01", true),
            ("2001-01-02", false),
        ] {
            let id = identity_with_dobs(Some(dob), None);
            assert_eq!(
                eic_childless_age_outcome(Some(&id), FilingStatus::Single, &p),
                Some(ok),
                "DOB {dob}"
            );
        }
    }

    #[test]
    fn se_tax_half_rounds_half_up() {
        let p = params();
        // $100 profit: earnings $92.35; SS = 92.35 x 12.4% = $11.4514
        // -> $11.45; Medicare = 92.35 x 2.9% = $2.67815 -> $2.68;
        // tax = $14.13 (odd cents) -> half rounds half up to $7.07.
        let se = schedule_se_tax(m(100), Money::ZERO, &p);
        assert_eq!(se.tax, Money::from_cents(1_413));
        assert_eq!(deductible_part_of_se_tax(se.tax), Money::from_cents(707));
    }

    fn senior(dob: &str, with_ssn: bool) -> PersonIdentity {
        PersonIdentity {
            ssn: with_ssn.then(|| "123456789".to_string()),
            date_of_birth: Some(dob.to_string()),
            ..Default::default()
        }
    }

    fn identity(person: PersonIdentity, spouse: Option<PersonIdentity>) -> TaxpayerIdentity {
        TaxpayerIdentity {
            person,
            spouse,
            ..Default::default()
        }
    }

    #[test]
    fn senior_bonus_age_boundary() {
        let p = params();
        // Born before January 2, 1961 counts (Schedule 1-A lines
        // 36a/36b); born ON the cutoff date does not.
        let yes = identity(senior("1961-01-01", true), None);
        let no = identity(senior("1961-01-02", true), None);
        assert_eq!(
            senior_bonus_eligible_persons(Some(&yes), FilingStatus::Single, &p),
            1
        );
        assert_eq!(
            senior_bonus_eligible_persons(Some(&no), FilingStatus::Single, &p),
            0
        );
        let r = schedule_1a_senior_deduction(Some(&yes), FilingStatus::Single, m(50_000), &p);
        assert_eq!(r.deduction, m(6_000), "full $6,000 under the threshold");
        assert_eq!(r.disqualifier, None);
    }

    #[test]
    fn senior_bonus_requires_ssn_and_identity() {
        let p = params();
        // 65+ but no SSN stored: lines 36a/36b require a valid SSN.
        let no_ssn = identity(senior("1958-05-01", false), None);
        assert_eq!(
            senior_bonus_eligible_persons(Some(&no_ssn), FilingStatus::Single, &p),
            0
        );
        // No identity at all: stated zero with the documented reason.
        let r = schedule_1a_senior_deduction(None, FilingStatus::Single, m(50_000), &p);
        assert_eq!(r.deduction, Money::ZERO);
        assert_eq!(r.disqualifier, Some("no_eligible_person"));
    }

    #[test]
    fn senior_bonus_mfj_one_vs_both_spouses() {
        let p = params();
        let both = identity(senior("1958-05-01", true), Some(senior("1960-11-15", true)));
        let one = identity(senior("1958-05-01", true), Some(senior("1970-06-01", true)));
        let magi = m(70_000); // under the $150,000 MFJ threshold
        let r = schedule_1a_senior_deduction(Some(&both), FilingStatus::Mfj, magi, &p);
        assert_eq!(r.eligible_persons, 2);
        assert_eq!(r.deduction, m(12_000), "golden case 008 shape");
        let r = schedule_1a_senior_deduction(Some(&one), FilingStatus::Mfj, magi, &p);
        assert_eq!(r.eligible_persons, 1);
        assert_eq!(r.deduction, m(6_000));
        // A spouse stored on a single return never counts.
        assert_eq!(
            senior_bonus_eligible_persons(Some(&both), FilingStatus::Single, &p),
            1
        );
    }

    #[test]
    fn senior_bonus_phaseout_is_per_person() {
        let p = params();
        let both = identity(senior("1958-05-01", true), Some(senior("1960-11-15", true)));
        // Golden case 010 shape: MFJ, MAGI $200,000 → excess $50,000 →
        // 6% = $3,000 off EACH person's $6,000 → 2 × $3,000 = $6,000
        // (not $12,000 − $3,000 = $9,000).
        let r = schedule_1a_senior_deduction(Some(&both), FilingStatus::Mfj, m(200_000), &p);
        assert_eq!(r.per_person, m(3_000), "Schedule 1-A line 35");
        assert_eq!(r.deduction, m(6_000), "Schedule 1-A line 37");
        assert_eq!(r.disqualifier, None);
        // Exactly at the threshold: no reduction (line 33 is zero).
        let single = identity(senior("1958-05-01", true), None);
        let r = schedule_1a_senior_deduction(Some(&single), FilingStatus::Single, m(75_000), &p);
        assert_eq!(r.deduction, m(6_000));
        let r = schedule_1a_senior_deduction(Some(&both), FilingStatus::Mfj, m(150_000), &p);
        assert_eq!(r.deduction, m(12_000));
        // Floor at zero: $100,000 of excess exhausts the $6,000
        // (single at $175,000); beyond that it stays zero, and a
        // phaseout to zero is arithmetic, not a disqualifier.
        let r = schedule_1a_senior_deduction(Some(&single), FilingStatus::Single, m(175_000), &p);
        assert_eq!(r.per_person, Money::ZERO);
        assert_eq!(r.deduction, Money::ZERO);
        assert_eq!(r.disqualifier, None);
        let r = schedule_1a_senior_deduction(Some(&both), FilingStatus::Mfj, m(900_000), &p);
        assert_eq!(r.deduction, Money::ZERO);
        // Partial phaseout, single: $25,000 excess → $1,500 off.
        let r = schedule_1a_senior_deduction(Some(&single), FilingStatus::Single, m(100_000), &p);
        assert_eq!(r.deduction, m(4_500));
    }

    #[test]
    fn senior_bonus_mfs_barred() {
        let p = params();
        let both = identity(senior("1958-05-01", true), Some(senior("1960-11-15", true)));
        // Part V caution: "If married, you must file jointly to claim
        // this deduction" — MFS takes nothing, even at low MAGI.
        assert_eq!(
            senior_bonus_eligible_persons(Some(&both), FilingStatus::Mfs, &p),
            0
        );
        let r = schedule_1a_senior_deduction(Some(&both), FilingStatus::Mfs, m(40_000), &p);
        assert_eq!(r.deduction, Money::ZERO);
        assert_eq!(r.disqualifier, Some("mfs"));
    }
}
