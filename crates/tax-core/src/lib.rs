//! tax-core: deterministic engine primitives.
//!
//! Design rules (see repo AGENTS.md):
//! - Money is integer cents. No floats anywhere in the filed path.
//! - A missing fact is [`FactResult::Incomplete`], never silently zero.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::ops::{Add, Neg, Sub};

// ---------------------------------------------------------------------------
// Money
// ---------------------------------------------------------------------------

/// Money as integer cents (i64).
///
/// Serialized as a *string of cents* (e.g. `387150` cents is `"387150"`) so
/// JSON round-trips are exact and float-free. Deserialization also accepts a
/// bare JSON integer (interpreted as cents) for convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Money = Money(0);

    pub fn from_cents(cents: i64) -> Money {
        Money(cents)
    }

    pub fn cents(self) -> i64 {
        self.0
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Parse a cents string like `"387150"` or a dollar string like
    /// `"3871.50"` when it contains a `.`. Used for human/CLI input.
    pub fn parse(s: &str) -> Result<Money, String> {
        let s = s.trim();
        if let Some((dollars, frac)) = s.split_once('.') {
            let d: i64 = dollars.parse().map_err(|_| format!("invalid money: {s}"))?;
            let f = format!("{:0<2}", frac);
            let f: i64 = f[..2].parse().map_err(|_| format!("invalid money: {s}"))?;
            let sign = if d < 0 { -1 } else { 1 };
            Ok(Money(d * 100 + sign * f))
        } else {
            s.parse::<i64>()
                .map(Money)
                .map_err(|_| format!("invalid money: {s}"))
        }
    }

    /// Multiply by a basis-point rate (1000 bp = 10%), rounded to nearest cent.
    pub fn mul_bp(self, rate_bp: u32) -> Money {
        let v = self.0 as i128 * rate_bp as i128;
        let rounded = if v >= 0 {
            (v + 5000) / 10000
        } else {
            (v - 5000) / 10000
        };
        Money(rounded as i64)
    }
}

impl fmt::Display for Money {
    /// Human format: dollars with two decimals, e.g. `3871.50` (no `$`, no commas).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        write!(f, "{}{}.{:02}", sign, abs / 100, abs % 100)
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Money, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Money;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("money as a string of integer cents, or a bare integer of cents")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Money, E> {
                v.trim().parse::<i64>().map(Money).map_err(E::custom)
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Money, E> {
                self.visit_str(&v)
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Money, E> {
                Ok(Money(v))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Money, E> {
                i64::try_from(v).map(Money).map_err(E::custom)
            }
        }
        deserializer.deserialize_any(V)
    }
}

impl Add for Money {
    type Output = Money;
    fn add(self, rhs: Money) -> Money {
        Money(self.0 + rhs.0)
    }
}
impl Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Money) -> Money {
        Money(self.0 - rhs.0)
    }
}
impl Neg for Money {
    type Output = Money;
    fn neg(self) -> Money {
        Money(-self.0)
    }
}
impl std::iter::Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        iter.fold(Money::ZERO, |a, b| a + b)
    }
}

// ---------------------------------------------------------------------------
// Fact results (IRS Fact Graph semantics)
// ---------------------------------------------------------------------------

/// The result of evaluating a fact. `Incomplete` means an input the fact
/// depends on has not been provided — it is *not* zero and *not* false.
/// `Placeholder(v)` means `v` was used as a stand-in and the value must not
/// be treated as final.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum FactResult<T> {
    Complete(T),
    Incomplete,
    Placeholder(T),
}

impl<T> FactResult<T> {
    pub fn is_complete(&self) -> bool {
        matches!(self, FactResult::Complete(_))
    }
    pub fn value(&self) -> Option<&T> {
        match self {
            FactResult::Complete(v) | FactResult::Placeholder(v) => Some(v),
            FactResult::Incomplete => None,
        }
    }
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> FactResult<U> {
        match self {
            FactResult::Complete(v) => FactResult::Complete(f(v)),
            FactResult::Placeholder(v) => FactResult::Placeholder(f(v)),
            FactResult::Incomplete => FactResult::Incomplete,
        }
    }
}

impl FactResult<Money> {
    /// Combine two money facts with `f`, propagating incompleteness.
    fn combine(self, other: FactResult<Money>, f: impl Fn(Money, Money) -> Money) -> Self {
        use FactResult::*;
        match (self, other) {
            (Incomplete, _) | (_, Incomplete) => Incomplete,
            (Placeholder(a), Placeholder(b)) | (Placeholder(a), Complete(b)) => {
                Placeholder(f(a, b))
            }
            (Complete(a), Placeholder(b)) => Placeholder(f(a, b)),
            (Complete(a), Complete(b)) => Complete(f(a, b)),
        }
    }
}

// ---------------------------------------------------------------------------
// Expression graph
// ---------------------------------------------------------------------------

/// A serde-tagged expression over money facts. Adapted from tenforty-graph's
/// `Op` set, with money kept in integer cents and completeness propagated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Read a fact by path, e.g. `/wages`.
    Input {
        path: String,
    },
    /// A literal amount, in cents.
    Literal {
        cents: i64,
    },
    Add {
        args: Vec<Op>,
    },
    Sub {
        left: Box<Op>,
        right: Box<Op>,
    },
    /// Multiply money by a dimensionless factor whose 100 cents = 1.0
    /// (e.g. `Literal { cents: 12 }` behaves like a 0.12 factor on the left).
    Mul {
        left: Box<Op>,
        right: Box<Op>,
    },
    Max {
        args: Vec<Op>,
    },
    Min {
        args: Vec<Op>,
    },
    Clamp {
        input: Box<Op>,
        min_cents: Option<i64>,
        max_cents: Option<i64>,
    },
    /// Marginal bracket tax. `table` is `(upper_threshold, rate_bp)` pairs in
    /// ascending order; the final pair's threshold is the top of the tax
    /// (use a very large value for the unbounded top bracket).
    BracketTax {
        table: Vec<(Money, u32)>,
        income: Box<Op>,
    },
    /// Quantize to the nearest multiple of `step_cents` (tax-table style).
    TaxTableQuantize {
        input: Box<Op>,
        step_cents: i64,
    },
}

/// Evaluate an [`Op`] against named facts, propagating completeness:
/// any `Incomplete` input makes the whole expression `Incomplete` (never 0).
pub fn eval(op: &Op, facts: &HashMap<String, FactResult<Money>>) -> FactResult<Money> {
    use FactResult::*;
    match op {
        Op::Input { path } => facts.get(path).cloned().unwrap_or(Incomplete),
        Op::Literal { cents } => Complete(Money::from_cents(*cents)),
        Op::Add { args } => args
            .iter()
            .map(|a| eval(a, facts))
            .fold(Complete(Money::ZERO), |acc, r| acc.combine(r, |a, b| a + b)),
        Op::Sub { left, right } => eval(left, facts).combine(eval(right, facts), |a, b| a - b),
        Op::Mul { left, right } => eval(left, facts).combine(eval(right, facts), |a, b| {
            Money::from_cents(((a.cents() as i128 * b.cents() as i128) / 100) as i64)
        }),
        Op::Max { args } => fold_extreme(args, facts, true),
        Op::Min { args } => fold_extreme(args, facts, false),
        Op::Clamp {
            input,
            min_cents,
            max_cents,
        } => eval(input, facts).map(|v| {
            let mut c = v.cents();
            if let Some(mn) = min_cents {
                c = c.max(*mn);
            }
            if let Some(mx) = max_cents {
                c = c.min(*mx);
            }
            Money::from_cents(c)
        }),
        Op::BracketTax { table, income } => eval(income, facts).map(|inc| bracket_tax(inc, table)),
        Op::TaxTableQuantize { input, step_cents } => eval(input, facts).map(|v| {
            if *step_cents <= 0 {
                return v;
            }
            let s = *step_cents;
            let c = v.cents();
            let q = (c + s / 2).div_euclid(s) * s;
            Money::from_cents(if c >= 0 { q } else { c.div_euclid(s) * s })
        }),
    }
}

fn fold_extreme(
    args: &[Op],
    facts: &HashMap<String, FactResult<Money>>,
    is_max: bool,
) -> FactResult<Money> {
    use FactResult::*;
    let mut acc: Option<FactResult<Money>> = None;
    for a in args {
        let r = eval(a, facts);
        acc = Some(match (acc, r) {
            (None, r) => r,
            (Some(Incomplete), _) | (_, Incomplete) => Incomplete,
            (Some(x), y) => x.combine(y, |p, q| {
                if is_max {
                    Money::from_cents(p.cents().max(q.cents()))
                } else {
                    Money::from_cents(p.cents().min(q.cents()))
                }
            }),
        });
    }
    acc.unwrap_or(Incomplete)
}

/// Marginal bracket tax on `income` given `(upper_threshold, rate_bp)` pairs.
pub fn bracket_tax(income: Money, table: &[(Money, u32)]) -> Money {
    let inc = income.cents();
    if inc <= 0 {
        return Money::ZERO;
    }
    let mut tax: i128 = 0;
    let mut prev: i64 = 0;
    for (threshold, rate_bp) in table {
        let top = threshold.cents();
        if inc <= prev {
            break;
        }
        let portion = inc.min(top) - prev;
        if portion > 0 {
            tax += (portion as i128 * *rate_bp as i128 + 5000) / 10000;
        }
        prev = top;
    }
    Money::from_cents(tax as i64)
}

// ---------------------------------------------------------------------------
// Filing status + per-year parameters (DATA, never hardcoded rates)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilingStatus {
    Single,
    Mfj,
    Hoh,
    Mfs,
}

impl FilingStatus {
    pub fn parse(s: &str) -> Result<FilingStatus, String> {
        match s.to_ascii_lowercase().as_str() {
            "single" => Ok(FilingStatus::Single),
            "mfj" | "married_filing_jointly" => Ok(FilingStatus::Mfj),
            "hoh" | "head_of_household" => Ok(FilingStatus::Hoh),
            "mfs" | "married_filing_separately" => Ok(FilingStatus::Mfs),
            other => Err(format!("unknown filing status: {other}")),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            FilingStatus::Single => "single",
            FilingStatus::Mfj => "mfj",
            FilingStatus::Hoh => "hoh",
            FilingStatus::Mfs => "mfs",
        }
    }
}

fn default_se_ss_rate_bp() -> u32 {
    1240
}
fn default_se_medicare_rate_bp() -> u32 {
    290
}
fn default_se_net_earnings_factor_bp() -> u32 {
    9235
}
fn default_ctc_per_child_cents() -> i64 {
    220_000
}
fn default_odc_per_dependent_cents() -> i64 {
    50_000
}
fn default_actc_cap_per_child_cents() -> i64 {
    170_000
}
fn default_ctc_phaseout_rate_bp() -> u32 {
    500
}
fn default_actc_earned_income_threshold_cents() -> i64 {
    250_000
}
fn default_actc_rate_bp() -> u32 {
    1500
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bracket {
    /// Upper income threshold in cents; `null` = unbounded top bracket.
    pub up_to_cents: Option<i64>,
    pub rate_bp: u32,
}

/// Capital-gain rate breakpoints for one filing status (money = cents):
/// preferential income is taxed at 0% up to `zero_rate_up_to_cents` of
/// taxable income, at 15% up to `twenty_rate_from_cents`, and at 20%
/// above that (Rev. Proc. 2024-40, §3.03, for TY2025).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapitalGainThresholds {
    pub zero_rate_up_to_cents: i64,
    pub twenty_rate_from_cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxParams {
    pub year: u32,
    /// Human-readable citation for every value in this file.
    pub source: String,
    pub standard_deduction_cents: HashMap<String, i64>,
    pub social_security_wage_base_cents: i64,
    /// Self-employment (Schedule SE) Social Security rate, basis points
    /// (12.4% per IRC §1401(a)).
    #[serde(default = "default_se_ss_rate_bp")]
    pub se_ss_rate_bp: u32,
    /// Self-employment Medicare rate, basis points (2.9% per IRC §1401(b)).
    #[serde(default = "default_se_medicare_rate_bp")]
    pub se_medicare_rate_bp: u32,
    /// Net-earnings factor for self-employment income, basis points
    /// (92.35% per IRC §1402(a); Schedule SE line 4a).
    #[serde(default = "default_se_net_earnings_factor_bp")]
    pub se_net_earnings_factor_bp: u32,
    pub ordinary_brackets: HashMap<String, Vec<Bracket>>,
    /// IRC §1211(b) capital-loss allowance per filing status (cents).
    #[serde(default)]
    pub capital_loss_limit_cents: HashMap<String, i64>,
    /// §1(j)(5)(B) maximum zero / 15-percent rate amounts per status.
    #[serde(default)]
    pub capital_gain_thresholds_cents: HashMap<String, CapitalGainThresholds>,
    /// Child tax credit per qualifying child (Schedule 8812, line 5:
    /// $2,200 for TY2025, as increased by the OBBBA).
    #[serde(default = "default_ctc_per_child_cents")]
    pub ctc_per_child_cents: i64,
    /// Credit for other dependents, per dependent (Schedule 8812,
    /// line 7: $500).
    #[serde(default = "default_odc_per_dependent_cents")]
    pub odc_per_dependent_cents: i64,
    /// Additional child tax credit cap per qualifying child (Schedule
    /// 8812, line 16b: $1,700 for TY2025).
    #[serde(default = "default_actc_cap_per_child_cents")]
    pub actc_cap_per_child_cents: i64,
    /// CTC/ODC phaseout threshold (modified AGI) per filing status
    /// (Schedule 8812, line 9: $400,000 MFJ, $200,000 all other
    /// statuses).
    #[serde(default)]
    pub ctc_phaseout_threshold_cents: HashMap<String, i64>,
    /// Phaseout rate applied to the excess MAGI after it is rounded up
    /// to the next multiple of $1,000 (Schedule 8812, lines 10–11: 5%,
    /// i.e. $50 per $1,000 or fraction thereof).
    #[serde(default = "default_ctc_phaseout_rate_bp")]
    pub ctc_phaseout_rate_bp: u32,
    /// Earned income above which the ACTC's 15% measure applies
    /// (Schedule 8812, Part II-A: $2,500).
    #[serde(default = "default_actc_earned_income_threshold_cents")]
    pub actc_earned_income_threshold_cents: i64,
    /// ACTC earned-income rate (Schedule 8812, line 20: 15%).
    #[serde(default = "default_actc_rate_bp")]
    pub actc_rate_bp: u32,
}

impl TaxParams {
    /// TY2025 parameters, embedded from `params/2025.json` (the file in the
    /// repo is the source of truth; embedding just makes the binary standalone).
    pub fn load_2025() -> Result<TaxParams, serde_json::Error> {
        serde_json::from_str(include_str!("../params/2025.json"))
    }

    pub fn load_file(path: &std::path::Path) -> Result<TaxParams, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    pub fn standard_deduction(&self, status: FilingStatus) -> Money {
        Money::from_cents(
            *self
                .standard_deduction_cents
                .get(status.as_str())
                .unwrap_or(&0),
        )
    }

    /// IRC §1211(b) capital-loss limit for the status ($3,000; $1,500 MFS).
    pub fn capital_loss_limit(&self, status: FilingStatus) -> Money {
        Money::from_cents(
            *self
                .capital_loss_limit_cents
                .get(status.as_str())
                .unwrap_or(&300_000),
        )
    }

    /// §1(j)(5)(B) capital-gain breakpoints for the status, if present.
    pub fn capital_gain_thresholds(&self, status: FilingStatus) -> Option<CapitalGainThresholds> {
        self.capital_gain_thresholds_cents
            .get(status.as_str())
            .copied()
    }

    /// CTC/ODC phaseout threshold (MAGI) for the status (Schedule
    /// 8812, line 9): $400,000 if married filing jointly, $200,000 for
    /// every other filing status.
    pub fn ctc_phaseout_threshold(&self, status: FilingStatus) -> Money {
        let fallback = if status == FilingStatus::Mfj {
            40_000_000
        } else {
            20_000_000
        };
        Money::from_cents(
            *self
                .ctc_phaseout_threshold_cents
                .get(status.as_str())
                .unwrap_or(&fallback),
        )
    }

    /// Bracket table in [`Op::BracketTax`] / [`bracket_tax`] form.
    pub fn bracket_table(&self, status: FilingStatus) -> Vec<(Money, u32)> {
        let Some(brackets) = self.ordinary_brackets.get(status.as_str()) else {
            return Vec::new();
        };
        brackets
            .iter()
            .map(|b| {
                (
                    Money::from_cents(b.up_to_cents.unwrap_or(i64::MAX)),
                    b.rate_bp,
                )
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// TY2025 Tax Table (DATA: extracted from the official IRS table, never
// synthesized from the bracket formulas — see the file's `source` field)
// ---------------------------------------------------------------------------

/// One row of the Tax Table: for taxable income in
/// `[from_dollars, to_dollars)` the tax is the per-status whole-dollar
/// amount. Row widths are the table's own (5/10/25 dollars at the bottom,
/// 50 dollars from $3,000 up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaxTableRow {
    pub from_dollars: i64,
    pub to_dollars: i64,
    pub single: i64,
    pub mfj: i64,
    pub mfs: i64,
    pub hoh: i64,
}

impl TaxTableRow {
    /// The table's tax (whole dollars) for a filing status.
    pub fn tax(&self, status: FilingStatus) -> i64 {
        match status {
            FilingStatus::Single => self.single,
            FilingStatus::Mfj => self.mfj,
            FilingStatus::Mfs => self.mfs,
            FilingStatus::Hoh => self.hoh,
        }
    }
}

/// The TY2025 Tax Table, embedded from `params/2025-tax-table.json`.
#[derive(Debug, Clone)]
pub struct TaxTable {
    pub year: u32,
    pub source: String,
    pub rows: Vec<TaxTableRow>,
}

#[derive(Deserialize)]
struct TaxTableFile {
    year: u32,
    source: String,
    #[allow(dead_code)]
    unit: String,
    #[allow(dead_code)]
    columns: Vec<String>,
    rows: Vec<[i64; 6]>,
}

impl TaxTable {
    /// The embedded TY2025 table (parsed once; the rows are immutable
    /// process-lifetime data).
    pub fn load_2025() -> &'static TaxTable {
        static TABLE: std::sync::OnceLock<TaxTable> = std::sync::OnceLock::new();
        TABLE.get_or_init(|| {
            let file: TaxTableFile =
                serde_json::from_str(include_str!("../params/2025-tax-table.json"))
                    .expect("bundled 2025-tax-table.json must parse");
            TaxTable {
                year: file.year,
                source: file.source,
                rows: file
                    .rows
                    .iter()
                    .map(|r| TaxTableRow {
                        from_dollars: r[0],
                        to_dollars: r[1],
                        single: r[2],
                        mfj: r[3],
                        mfs: r[4],
                        hoh: r[5],
                    })
                    .collect(),
            }
        })
    }

    /// Tax Table lookup for a taxable-income amount.
    ///
    /// The table applies only below $100,000 of taxable income; returns
    /// `None` at or above $100,000 (the Tax Computation Worksheet applies
    /// there) or for a negative amount. The row is selected by the whole
    /// dollars of the amount (the amount floored to dollars — the form
    /// itself is figured in whole dollars; see docs/KNOWN_LIMITS.md).
    /// The returned tax is a whole-dollar amount, as printed.
    pub fn tax(&self, status: FilingStatus, taxable: Money) -> Option<Money> {
        if taxable.cents() < 0 {
            return None;
        }
        let dollars = taxable.cents().div_euclid(100);
        if dollars >= 100_000 {
            return None;
        }
        let idx = self.rows.partition_point(|r| r.to_dollars <= dollars);
        let row = self.rows.get(idx)?;
        if row.from_dollars <= dollars && dollars < row.to_dollars {
            Some(Money::from_cents(row.tax(status) * 100))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(pairs: &[(&str, FactResult<Money>)]) -> HashMap<String, FactResult<Money>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn money_parse_format_and_serde() {
        assert_eq!(Money::from_cents(387150).to_string(), "3871.50");
        assert_eq!(Money::parse("3871.50").unwrap(), Money::from_cents(387150));
        assert_eq!(Money::parse("387150").unwrap(), Money::from_cents(387150));
        assert_eq!(Money::from_cents(-150).to_string(), "-1.50");
        let json = serde_json::to_string(&Money::from_cents(5000000)).unwrap();
        assert_eq!(json, "\"5000000\"");
        let back: Money = serde_json::from_str("\"5000000\"").unwrap();
        assert_eq!(back, Money::from_cents(5000000));
        let bare: Money = serde_json::from_str("5000000").unwrap();
        assert_eq!(bare, Money::from_cents(5000000));
        assert_eq!(
            Money::from_cents(10000) + Money::from_cents(250),
            Money::from_cents(10250)
        );
        assert_eq!(
            Money::from_cents(10000) - Money::from_cents(250),
            Money::from_cents(9750)
        );
    }

    #[test]
    fn missing_fact_is_incomplete_never_zero() {
        let f = facts(&[("wages", FactResult::Complete(Money::from_cents(100)))]);
        let op = Op::Add {
            args: vec![
                Op::Input {
                    path: "wages".into(),
                },
                Op::Input {
                    path: "interest".into(),
                },
            ],
        };
        assert_eq!(eval(&op, &f), FactResult::Incomplete);
    }

    #[test]
    fn placeholder_propagates() {
        let f = facts(&[
            ("a", FactResult::Complete(Money::from_cents(100))),
            ("b", FactResult::Placeholder(Money::from_cents(50))),
        ]);
        let op = Op::Add {
            args: vec![
                Op::Input { path: "a".into() },
                Op::Input { path: "b".into() },
            ],
        };
        assert_eq!(
            eval(&op, &f),
            FactResult::Placeholder(Money::from_cents(150))
        );
    }

    #[test]
    fn bracket_tax_single_34250() {
        let params = TaxParams::load_2025().unwrap();
        let table = params.bracket_table(FilingStatus::Single);
        // $34,250 taxable: 11925*10% + 22325*12% = 1192.50 + 2679.00 = 3871.50
        assert_eq!(
            bracket_tax(Money::from_cents(3425000), &table),
            Money::from_cents(387150)
        );
        assert_eq!(bracket_tax(Money::ZERO, &table), Money::ZERO);
    }

    #[test]
    fn max_min_clamp_and_quantize() {
        let f = facts(&[]);
        let max = Op::Max {
            args: vec![Op::Literal { cents: 5 }, Op::Literal { cents: 9 }],
        };
        assert_eq!(eval(&max, &f), FactResult::Complete(Money::from_cents(9)));
        let clamp = Op::Clamp {
            input: Box::new(Op::Literal { cents: -100 }),
            min_cents: Some(0),
            max_cents: None,
        };
        assert_eq!(eval(&clamp, &f), FactResult::Complete(Money::ZERO));
        let q = Op::TaxTableQuantize {
            input: Box::new(Op::Literal { cents: 3425037 }),
            step_cents: 5000,
        };
        assert_eq!(
            eval(&q, &f),
            FactResult::Complete(Money::from_cents(3425000))
        );
    }

    #[test]
    fn params_2025_standard_deductions() {
        let p = TaxParams::load_2025().unwrap();
        assert_eq!(p.year, 2025);
        assert_eq!(p.standard_deduction(FilingStatus::Single).cents(), 1575000);
        assert_eq!(p.standard_deduction(FilingStatus::Mfj).cents(), 3150000);
        assert_eq!(p.standard_deduction(FilingStatus::Hoh).cents(), 2362500);
        assert_eq!(p.standard_deduction(FilingStatus::Mfs).cents(), 1575000);
        assert!(p.source.contains("Rev. Proc. 2024-40"));
    }

    #[test]
    fn params_2025_capital_loss_limit_and_thresholds() {
        let p = TaxParams::load_2025().unwrap();
        // IRC 1211(b): $3,000, halved for MFS.
        assert_eq!(p.capital_loss_limit(FilingStatus::Single).cents(), 300_000);
        assert_eq!(p.capital_loss_limit(FilingStatus::Mfj).cents(), 300_000);
        assert_eq!(p.capital_loss_limit(FilingStatus::Hoh).cents(), 300_000);
        assert_eq!(p.capital_loss_limit(FilingStatus::Mfs).cents(), 150_000);
        // Rev. Proc. 2024-40, section 3.03 (maximum zero / 15% rate amounts).
        let t = |s: FilingStatus| p.capital_gain_thresholds(s).unwrap();
        assert_eq!(t(FilingStatus::Single).zero_rate_up_to_cents, 4_835_000);
        assert_eq!(t(FilingStatus::Single).twenty_rate_from_cents, 53_340_000);
        assert_eq!(t(FilingStatus::Mfj).zero_rate_up_to_cents, 9_670_000);
        assert_eq!(t(FilingStatus::Mfj).twenty_rate_from_cents, 60_005_000);
        assert_eq!(t(FilingStatus::Hoh).zero_rate_up_to_cents, 6_475_000);
        assert_eq!(t(FilingStatus::Hoh).twenty_rate_from_cents, 56_670_000);
        assert_eq!(t(FilingStatus::Mfs).zero_rate_up_to_cents, 4_835_000);
        assert_eq!(t(FilingStatus::Mfs).twenty_rate_from_cents, 30_000_000);
    }

    #[test]
    fn params_2025_self_employment() {
        let p = TaxParams::load_2025().unwrap();
        // SSA 2025 announcement (Oct. 10, 2024; Fact Sheet "2025 Social
        // Security Changes"): wage base $176,100.
        assert_eq!(p.social_security_wage_base_cents, 17_610_000);
        // IRC 1401(a): 12.4% Social Security; IRC 1401(b): 2.9% Medicare.
        assert_eq!(p.se_ss_rate_bp, 1240);
        assert_eq!(p.se_medicare_rate_bp, 290);
        // IRC 1402(a) / Schedule SE line 4a: net earnings = 92.35%.
        assert_eq!(p.se_net_earnings_factor_bp, 9235);
    }

    #[test]
    fn params_2025_child_credits() {
        let p = TaxParams::load_2025().unwrap();
        // Schedule 8812 (Form 1040) TY2025: CTC $2,200 per qualifying
        // child (line 5; OBBBA increase per the instructions' What's
        // New), ODC $500 (line 7), ACTC cap $1,700 per child (line 16b).
        assert_eq!(p.ctc_per_child_cents, 220_000);
        assert_eq!(p.odc_per_dependent_cents, 50_000);
        assert_eq!(p.actc_cap_per_child_cents, 170_000);
        // Phaseout thresholds (line 9): $400,000 MFJ; $200,000 for all
        // other filing statuses — MFS included.
        assert_eq!(
            p.ctc_phaseout_threshold(FilingStatus::Mfj).cents(),
            40_000_000
        );
        assert_eq!(
            p.ctc_phaseout_threshold(FilingStatus::Single).cents(),
            20_000_000
        );
        assert_eq!(
            p.ctc_phaseout_threshold(FilingStatus::Hoh).cents(),
            20_000_000
        );
        assert_eq!(
            p.ctc_phaseout_threshold(FilingStatus::Mfs).cents(),
            20_000_000
        );
        // Lines 10-11: 5% of the excess rounded up to a $1,000 multiple.
        assert_eq!(p.ctc_phaseout_rate_bp, 500);
        // Part II-A: 15% of earned income over $2,500.
        assert_eq!(p.actc_earned_income_threshold_cents, 250_000);
        assert_eq!(p.actc_rate_bp, 1500);
        assert!(p.source.contains("Schedule 8812"));
    }

    #[test]
    fn tax_table_structure_tiles_and_is_monotonic() {
        let table = TaxTable::load_2025();
        assert_eq!(table.year, 2025);
        // The official table has 2,062 rows: 0-5, 5-15, 15-25, then $25
        // rows to $3,000 and $50 rows to $100,000.
        assert_eq!(table.rows.len(), 2062);
        let mut expect = 0;
        for row in &table.rows {
            assert_eq!(row.from_dollars, expect, "rows must tile [0, 100000)");
            expect = row.to_dollars;
        }
        assert_eq!(expect, 100_000);
        for (i, row) in table.rows.iter().enumerate().skip(1) {
            let prev = &table.rows[i - 1];
            for s in [
                FilingStatus::Single,
                FilingStatus::Mfj,
                FilingStatus::Mfs,
                FilingStatus::Hoh,
            ] {
                assert!(row.tax(s) >= prev.tax(s), "column {s:?} must not decrease");
            }
        }
    }

    #[test]
    fn tax_table_lookup_boundaries() {
        let table = TaxTable::load_2025();
        let tax = |s: FilingStatus, cents: i64| table.tax(s, Money::from_cents(cents));
        // First rows of the printed table.
        assert_eq!(tax(FilingStatus::Single, 0), Some(Money::from_cents(0)));
        assert_eq!(tax(FilingStatus::Single, 499), Some(Money::from_cents(0))); // $4.99 -> row 0-5
        assert_eq!(tax(FilingStatus::Single, 500), Some(Money::from_cents(100))); // $5 -> row 5-15, tax $1
        assert_eq!(
            tax(FilingStatus::Mfj, 100_000),
            Some(Money::from_cents(10_100))
        ); // $1,000 row: $101
           // Golden-case rows (single).
        assert_eq!(
            tax(FilingStatus::Single, 3_425_000),
            Some(Money::from_cents(387_500))
        ); // 34,250 -> $3,875
        assert_eq!(
            tax(FilingStatus::Single, 3_424_999),
            Some(Money::from_cents(386_900))
        ); // $34,249.99 -> row below
           // Cents are floored to dollars for row selection.
        assert_eq!(
            tax(FilingStatus::Single, 3_425_099),
            Some(Money::from_cents(387_500))
        );
        // Top of the table and beyond.
        assert_eq!(
            tax(FilingStatus::Single, 9_999_999),
            Some(Money::from_cents(1_690_900))
        );
        assert_eq!(tax(FilingStatus::Single, 10_000_000), None); // $100,000: table no longer applies
        assert_eq!(tax(FilingStatus::Single, 10_000_100), None);
        assert_eq!(tax(FilingStatus::Single, -100), None);
    }
}
