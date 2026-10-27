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
//!
//! Money is integer cents throughout; the only multiplications are the
//! worksheet's 15% / 20% rates, done with [`Money::mul_bp`] (integer
//! basis-point arithmetic, rounded to the nearest cent). The paper
//! worksheets are figured in whole dollars; the engine keeps cents and
//! selects Tax Table rows by the floored dollar amount (see
//! docs/KNOWN_LIMITS.md).

use tax_core::{bracket_tax, FilingStatus, Money, TaxParams, TaxTable};

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
}
