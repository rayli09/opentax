//! Form 1040-NR (TY2025), U.S. Nonresident Alien Income Tax Return —
//! the student edition: F-1 / J-1 (student) visa holders in their
//! exempt-individual years, filing SINGLE, whose income is wages
//! (Form W-2 and/or Form 1042-S income code 20) and scholarship or
//! fellowship grants (Form 1042-S income code 15). Companion route to
//! [`crate::compute_1040`]: when a return carries an [`crate::NraBlock`],
//! this engine computes it INSTEAD of the resident Form 1040.
//!
//! Line keys in [`NrComputed::lines`] are the PRINTED Form 1040-NR
//! line numbers, plus the internal keys `eci_wages_gross` (W-2 box 1
//! total plus code-20 1042-S gross, before any treaty exclusion),
//! `treaty_wage_exclusion` (the total personal-services exemption:
//! the already-excluded 1042-S part plus the amount claimed on the
//! return), `treaty_claim_on_return` (the part of the exemption
//! claimed against W-2 wages on this return), and
//! `taxable_scholarship` (the code-15 amount taxed via Schedule 1
//! (Form 1040) line 8r, which flows into line 8).
//!
//! Treaty mechanics (Instructions for Form 1040-NR (2025), "Line
//! 1a", "Amounts Exempt Under a Treaty", and "Line 1k"): wages that
//! are exempt under a treaty are NOT reported on line 1a; the total
//! exempt amount is reported on line 1k and detailed in Schedule OI
//! (Form 1040-NR), item L. The instructions work the same fact
//! pattern both ways, and the engine reproduces both:
//! (a) the employer was given Form 8233 and split the pay — the
//! exempt part arrives on a Form 1042-S with treaty markers
//! (exemption code "04" or a treaty article cited), the taxable part
//! on Form W-2: line 1a is the W-2 amount, line 1k the 1042-S
//! amount; (b) everything is on the W-2 — the filer claims the
//! exemption on the return: line 1a is W-2 box 1 MINUS the claim,
//! line 1k is the claim. The on-return claim is
//! max(0, treaty cap − already-excluded), capped by the W-2 wages
//! themselves (an exemption cannot exceed the income it exempts).
//! Taxable scholarship or fellowship grants not reported on Form W-2
//! are reported on Schedule 1 (Form 1040), line 8r, and reach line 8
//! of Form 1040-NR through Schedule 1, line 10 (the same
//! instructions' TIP under Line 1a).
//!
//! Scope (docs/KNOWN_LIMITS.md): no Schedule NEC (income not
//! effectively connected), no Schedule A itemizing, no credits —
//! Form 1040-NR lines 19/20/28 are stated zeros and any stored
//! dependent earns a finding, not a credit (only U.S. nationals and
//! residents of Canada/Mexico claim dependents on U.S.-citizen
//! terms, and India Article 21(2) students on the limited terms of
//! Pub. 519, chapter 5; China-treaty filers cannot claim a dependent
//! at all — Instructions for Form 1040-NR (2025), "Dependents").
//! No EIC (line 27 is reserved on the form), no Schedule 1-A senior
//! deduction, no self-employment tax, no estimated payments (line 26).

use std::collections::BTreeMap;
use tax_core::{FactResult, FilingStatus, Money, TaxParams, TreatyParams};

use crate::worksheets::tax_on_amount;
use crate::{Form1040Input, NraDetermination};

/// A deterministic finding about the Form 1040-NR section, mirroring
/// [`crate::ca540::CaFinding`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NrFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

/// One Schedule OI (Form 1040-NR), item L row: income exempt from
/// tax under a tax treaty. The rows' amounts sum to Form 1040-NR
/// line 1k.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScheduleOiEntry {
    /// Treaty country (ISO alpha-2), e.g. `"CN"`.
    pub country: String,
    /// Treaty article as cited, e.g. `"20(c)"`.
    pub treaty_article: String,
    /// Amount of income exempt under the treaty.
    pub exempt_amount: Money,
}

/// A computed Form 1040-NR section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NrComputed {
    /// Printed Form 1040-NR line number → fact result, plus the
    /// internal keys described in the module docs.
    pub lines: BTreeMap<String, FactResult<Money>>,
    /// True only when every computed line is `Complete`.
    pub complete: bool,
    /// The line keys whose facts are `Incomplete`.
    pub incomplete: Vec<String>,
    /// Human-readable simplifications that apply to this computation.
    pub notes: Vec<String>,
    /// Deterministic findings for `return validate`.
    pub findings: Vec<NrFinding>,
    /// Which method figured line 16: `"tax_table"` or
    /// `"tax_computation"`; `None` when line 16 is not computed.
    pub tax_method: Option<String>,
    /// Schedule OI, item L data (empty when nothing is exempt).
    pub schedule_oi: Vec<ScheduleOiEntry>,
    /// The residency determination the computation acted on.
    pub determination: Option<NraDetermination>,
}

/// The Form 1040-NR line keys, in form order (used for the
/// refuse-path incomplete map).
const NR_LINE_KEYS: &[&str] = &[
    "1a",
    "1k",
    "1z",
    "8",
    "9",
    "11a",
    "11b",
    "12",
    "14",
    "15",
    "16",
    "18",
    "22",
    "24",
    "25a",
    "25d",
    "25g",
    "32",
    "33",
    "34",
    "37",
    "eci_wages_gross",
    "treaty_wage_exclusion",
    "treaty_claim_on_return",
    "taxable_scholarship",
];

/// Store-entry label in the CLI's naming convention (`f1042s_01`, …)
/// for the i-th entry of a kind (0-based index).
fn label(kind: &str, i: usize) -> String {
    format!("{kind}_{:02}", i + 1)
}

/// Compute Form 1040-NR for a student return. `input.nra` must be
/// set (the CLI routes on it); when it is absent, or the block fails
/// validation, or the filing status is not single, the section is
/// wholly `Incomplete` with an error finding — the engine refuses
/// rather than guessing a residency.
pub fn compute_1040nr(
    input: &Form1040Input,
    params: &TaxParams,
    treaties: &TreatyParams,
) -> NrComputed {
    let mut findings: Vec<NrFinding> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    let refuse = |findings: Vec<NrFinding>, notes: Vec<String>| -> NrComputed {
        let lines: BTreeMap<String, FactResult<Money>> = NR_LINE_KEYS
            .iter()
            .map(|k| (k.to_string(), FactResult::Incomplete))
            .collect();
        NrComputed {
            incomplete: NR_LINE_KEYS.iter().map(|k| k.to_string()).collect(),
            lines,
            complete: false,
            notes,
            findings,
            tax_method: None,
            schedule_oi: Vec::new(),
            determination: None,
        }
    };

    let Some(block) = &input.nra else {
        findings.push(NrFinding {
            code: "nra_block_missing",
            severity: "error",
            message: "compute_1040nr called on a return with no nra block; set one with \
                      `tax return set-nra`"
                .to_string(),
            entry: None,
        });
        return refuse(findings, notes);
    };
    let determination = match block.determination(params.year, treaties) {
        Ok(d) => d,
        Err(e) => {
            findings.push(NrFinding {
                code: "nra_determination_failed",
                severity: "error",
                message: e,
                entry: None,
            });
            return refuse(findings, notes);
        }
    };
    if input.status != FilingStatus::Single {
        findings.push(NrFinding {
            code: "nra_status_not_supported",
            severity: "error",
            message: format!(
                "the Form 1040-NR student route supports filing status SINGLE only in v1; \
                 this return is {} (the married routes on Form 1040-NR — married filing \
                 separately, and the India Article 21(2) married-apart single-status \
                 exception — are not implemented)",
                input.status.as_str()
            ),
            entry: None,
        });
        return refuse(findings, notes);
    }

    let citizenship = block
        .normalized_citizenship()
        .expect("determination validated the citizenship");
    let treaty = treaties.country(&citizenship);
    if treaty.is_none() {
        findings.push(NrFinding {
            code: "treaty_not_modeled",
            severity: "warning",
            message: format!(
                "citizenship {citizenship} has no treaty provisions in \
                 params/2025-treaties.json (only CN and IN are modeled for TY2025): the \
                 Form 1040-NR is computed WITHOUT treaty benefits — no wage exemption, no \
                 standard deduction. If a U.S. treaty with {citizenship} gives this \
                 student a benefit, v1 does not apply it"
            ),
            entry: None,
        });
    }

    // -----------------------------------------------------------------
    // Income documents: W-2s and Forms 1042-S.
    // -----------------------------------------------------------------
    let w2_wages: Money = input.w2s.iter().map(|w| w.wages).sum();
    let w2_withholding: Money = input.w2s.iter().map(|w| w.federal_withholding).sum();

    let mut code20_gross = Money::ZERO;
    let mut code20_exempt = Money::ZERO; // already excluded by the payer (treaty-marked)
    let mut code15_gross = Money::ZERO;
    let mut code15_marked_exempt = Money::ZERO;
    let mut s_withholding = Money::ZERO;
    let mut unmodeled_code = false;
    let mut schedule_oi: Vec<ScheduleOiEntry> = Vec::new();

    for (i, s) in input.f1042ss.iter().enumerate() {
        let entry = label("f1042s", i);
        s_withholding = s_withholding + s.federal_withholding;
        match s.income_code {
            20 => {
                code20_gross = code20_gross + s.gross_income;
                if s.indicates_treaty_exemption() {
                    code20_exempt = code20_exempt + s.gross_income;
                }
            }
            15 => {
                code15_gross = code15_gross + s.gross_income;
                if s.indicates_treaty_exemption() {
                    code15_marked_exempt = code15_marked_exempt + s.gross_income;
                }
            }
            other => {
                unmodeled_code = true;
                findings.push(NrFinding {
                    code: "f1042s_income_code_not_modeled",
                    severity: "error",
                    message: format!(
                        "Form 1042-S from {}: income code {other} is not modeled by the \
                         Form 1040-NR engine in v1 (only 15 scholarship/fellowship grants \
                         and 20 compensation during studying/training are). Its gross \
                         income ({}) is EXCLUDED from the computation and the NR section \
                         is incomplete — never silently taxed, never silently dropped",
                        s.payer_name, s.gross_income
                    ),
                    entry: Some(entry),
                });
            }
        }
    }

    // -----------------------------------------------------------------
    // Treaty wage exemption (personal-services cap, e.g. CN $5,000).
    // -----------------------------------------------------------------
    let ps_cap: Option<Money> = treaty
        .and_then(|t| t.personal_services_exemption_cents)
        .map(Money::from_cents);
    // Code-20 amounts the payer already excluded count toward the cap
    // ONLY when the treaty actually grants the exemption; a treaty
    // marker on a treaty that grants none is a discrepancy to flag.
    let mut already_excluded = Money::ZERO;
    if code20_exempt.cents() > 0 {
        match (ps_cap, treaty) {
            (Some(_), _) => already_excluded = code20_exempt,
            (None, Some(t)) => findings.push(NrFinding {
                code: "f1042s_exemption_not_recognized",
                severity: "warning",
                message: format!(
                    "a Form 1042-S reports {} of compensation as exempt under a treaty, \
                     but the {} student article grants no personal-services exemption in \
                     the modeled provisions: the amount is taxed as wages on line 1a",
                    code20_exempt, t.country_name
                ),
                entry: None,
            }),
            (None, None) => findings.push(NrFinding {
                code: "f1042s_exemption_not_recognized",
                severity: "warning",
                message: format!(
                    "a Form 1042-S reports {code20_exempt} of compensation as exempt \
                     under a treaty, but citizenship {citizenship} has no modeled \
                     treaty: the amount is taxed as wages on line 1a"
                ),
                entry: None,
            }),
        }
    }
    let claim_on_return = match ps_cap {
        Some(cap) => {
            let remaining = Money::from_cents((cap - already_excluded).cents().max(0));
            // The claim attaches to W-2 wages only, and cannot exceed
            // them (the exemption exempts income; it is not a credit).
            Money::from_cents(remaining.cents().min(w2_wages.cents()))
        }
        None => Money::ZERO,
    };
    let wage_exclusion = already_excluded + claim_on_return;
    if let (Some(cap), Some(t)) = (ps_cap, treaty) {
        if wage_exclusion.cents() > 0 {
            schedule_oi.push(ScheduleOiEntry {
                country: citizenship.clone(),
                treaty_article: t.personal_services_article.clone().unwrap_or_default(),
                exempt_amount: wage_exclusion,
            });
        }
        if wage_exclusion.cents() == cap.cents() && (w2_wages + code20_gross).cents() > cap.cents()
        {
            findings.push(NrFinding {
                code: "treaty_wage_cap_reached",
                severity: "info",
                message: format!(
                    "the {} treaty personal-services exemption is capped at {cap} per \
                     taxable year (Article {}): total wages of {} exceed it, so {} of \
                     wages remain taxable on line 1a",
                    t.country_name,
                    t.personal_services_article.as_deref().unwrap_or(""),
                    w2_wages + code20_gross,
                    w2_wages + code20_gross - wage_exclusion
                ),
                entry: None,
            });
        }
    }

    // -----------------------------------------------------------------
    // Scholarship / fellowship grants (income code 15).
    // -----------------------------------------------------------------
    let scholarship_exempt_ok = treaty.map(|t| t.scholarship_grants_exempt) == Some(true);
    let mut scholarship_exempt = Money::ZERO;
    if code15_marked_exempt.cents() > 0 {
        if scholarship_exempt_ok {
            scholarship_exempt = code15_marked_exempt;
            let t = treaty.expect("scholarship_grants_exempt implies a treaty");
            schedule_oi.push(ScheduleOiEntry {
                country: citizenship.clone(),
                treaty_article: t.scholarship_article.clone().unwrap_or_default(),
                exempt_amount: scholarship_exempt,
            });
        } else {
            findings.push(NrFinding {
                code: "f1042s_exemption_not_recognized",
                severity: "warning",
                message: format!(
                    "a Form 1042-S reports {code15_marked_exempt} of scholarship or \
                     fellowship grants as exempt under a treaty, but the modeled treaty \
                     provisions for {citizenship} do not exempt U.S.-source scholarship \
                     grants: the amount is taxed on Schedule 1 (Form 1040), line 8r, \
                     through line 8"
                ),
                entry: None,
            });
        }
    }
    let taxable_scholarship = code15_gross - scholarship_exempt;
    if taxable_scholarship.cents() > 0 {
        findings.push(NrFinding {
            code: "scholarship_taxable",
            severity: "info",
            message: format!(
                "scholarship or fellowship grants of {taxable_scholarship} are taxed on \
                 Schedule 1 (Form 1040), line 8r, through Form 1040-NR line 8: amounts \
                 a payer reports on Form 1042-S (income code 15) are the taxable part — \
                 the IRC 117 qualified-scholarship exclusion (tuition and required \
                 fees for a degree candidate; Pub 519 (2025), chapter 3) is applied by \
                 the payer before reporting, and v1 does not re-figure it"
            ),
            entry: None,
        });
    }

    // -----------------------------------------------------------------
    // FICA: an exempt individual's purpose-of-admission employment is
    // not covered employment (Pub 519 (2025), chapter 8).
    // -----------------------------------------------------------------
    for (i, w2) in input.w2s.iter().enumerate() {
        let ss = w2.social_security_tax.unwrap_or(Money::ZERO);
        let med = w2.medicare_tax.unwrap_or(Money::ZERO);
        if ss.cents() > 0 || med.cents() > 0 {
            findings.push(NrFinding {
                code: "fica_withheld_while_exempt",
                severity: "warning",
                message: format!(
                    "W-2 {} (employer EIN {}): Social Security tax {} and Medicare tax \
                     {} were withheld, but as an exempt individual ({} student, \
                     calendar year {} of 5) this filer's employment to carry out the \
                     purpose of admission is not covered employment — no FICA tax \
                     should have been withheld (Pub 519 (2025), chapter 8, 'Students \
                     and Exchange Visitors'). Ask the employer for a refund first; if \
                     the employer will not refund the full amount, file Form 843 \
                     (Claim for Refund) with the supporting documents Pub 519 lists \
                     (Form W-2 copy, visa, I-94, and Form I-20 for F-1 / DS-2019 for \
                     J-1; the employer documents erroneously withheld FICA on Form \
                     8316). This CLI does not prepare those forms, and the withheld \
                     FICA is NOT credited on Form 1040-NR",
                    label("w2", i),
                    w2.employer_ein,
                    ss,
                    med,
                    block.visa_type,
                    block.calendar_years_in_status_including_current
                ),
                entry: Some(label("w2", i)),
            });
        }
    }

    // -----------------------------------------------------------------
    // Resident-route documents present on an NR return.
    // -----------------------------------------------------------------
    if !input.f1099_ints.is_empty() || input.interest_income.cents() != 0 {
        findings.push(NrFinding {
            code: "nra_bank_interest_exempt",
            severity: "info",
            message: "interest income is NOT taxed on the Form 1040-NR route: interest \
                      on bank deposits that is not effectively connected with a U.S. \
                      trade or business is excluded from a nonresident alien's gross \
                      income (IRC 871(i); Pub 519 (2025), chapter 3, 'Interest \
                      Income'). The stored 1099-INT amounts and any directly recorded \
                      interest are ignored here (non-deposit interest would belong on \
                      Schedule NEC, which v1 does not model)"
                .to_string(),
            entry: None,
        });
    }
    if !input.f1099_divs.is_empty()
        || !input.f1099_bs.is_empty()
        || !input.f1099_necs.is_empty()
        || input.ordinary_dividends.cents() != 0
    {
        findings.push(NrFinding {
            code: "nra_schedule_nec_not_modeled",
            severity: "warning",
            message: "1099-DIV, 1099-B, or 1099-NEC entries are stored, but dividends, \
                      capital gains, and nonemployee compensation of a nonresident \
                      alien are not effectively connected wages: they are taxed (if at \
                      all) on Schedule NEC (Form 1040-NR) at flat or treaty rates, \
                      which v1 does NOT model. Those entries are excluded from this \
                      computation (their withholding is likewise not credited) — the \
                      wages-only result below does not settle them"
                .to_string(),
            entry: None,
        });
    }
    if !input.dependents.is_empty() {
        findings.push(NrFinding {
            code: "nra_credits_not_modeled",
            severity: "warning",
            message: format!(
                "{} dependent(s) are stored, but no dependent credit is computed on \
                 the Form 1040-NR student route in v1: only U.S. nationals and \
                 residents of Canada and Mexico claim dependents on the same terms as \
                 U.S. citizens, residents of India who are Article 21(2) students \
                 claim them on the limited terms of Pub 519, chapter 5, and residents \
                 of China cannot claim a qualifying dependent at all (Instructions \
                 for Form 1040-NR (2025), 'Dependents'). Lines 19 and 28 are stated \
                 zeros",
                input.dependents.len()
            ),
            entry: None,
        });
    }

    // -----------------------------------------------------------------
    // The Form 1040-NR chain.
    // -----------------------------------------------------------------
    // Line 1a excludes ALL treaty-exempt wages (both reporting
    // patterns land here): W-2 wages minus the on-return claim, plus
    // the taxable part of code-20 income.
    let line1a = w2_wages - claim_on_return + (code20_gross - already_excluded);
    let line1k = wage_exclusion + scholarship_exempt;
    let line1z = line1a;
    let line8 = taxable_scholarship;
    let line9 = line1z + line8;
    let line11a = line9; // line 10 adjustments: none in scope
    let line12 = match treaty {
        Some(t) if t.standard_deduction => {
            findings.push(NrFinding {
                code: "nra_standard_deduction_india",
                severity: "info",
                message: format!(
                    "line 12 takes the standard deduction of {} for a single filer: \
                     Article {} of the U.S.–{} treaty entitles a student to the same \
                     exemptions, reliefs or reductions available to residents, and the \
                     Instructions for Form 1040-NR (2025), Line 12, apply it as the \
                     Form 1040 standard deduction amount for the filing status",
                    params.standard_deduction(FilingStatus::Single),
                    t.standard_deduction_article.as_deref().unwrap_or("21(2)"),
                    t.country_name
                ),
                entry: None,
            });
            params.standard_deduction(FilingStatus::Single)
        }
        _ => {
            findings.push(NrFinding {
                code: "nra_no_standard_deduction",
                severity: "info",
                message: "line 12 is 0: nonresident aliens get NO standard deduction \
                          (the sole student exception modeled in v1 is India, under \
                          Article 21(2) of the U.S.–India treaty; itemized deductions \
                          on Schedule A (Form 1040-NR) are not computed in v1)"
                    .to_string(),
                entry: None,
            });
            Money::ZERO
        }
    };
    let line14 = line12;
    let line15 = Money::from_cents((line11a - line14).cents().max(0));
    // Line 16: the Instructions for Form 1040-NR send the filer to the
    // Instructions for Form 1040, Line 16 — the Tax Table below
    // $100,000 of taxable income, the Tax Computation Worksheet above
    // (no preferential capital-gain income exists on this route).
    let (line16, tax_method) = if line15.cents() < 10_000_000 {
        (
            tax_on_amount(line15, FilingStatus::Single, params),
            "tax_table",
        )
    } else {
        (
            tax_on_amount(line15, FilingStatus::Single, params),
            "tax_computation",
        )
    };
    let line18 = line16;
    let line22 = line18;
    let line24 = line22;
    let line25a = w2_withholding;
    let line25d = line25a; // lines 25b/25c are zero in scope
    let line25g = s_withholding;
    let line32 = Money::ZERO; // line 28 ACTC and lines 29-31: not modeled
    let line33 = line25d + line25g + line32;
    let (line34, line37) = if line33.cents() > line24.cents() {
        (line33 - line24, Money::ZERO)
    } else {
        (Money::ZERO, line24 - line33)
    };

    let mut lines: BTreeMap<String, FactResult<Money>> = BTreeMap::new();
    // Lines downstream of the income documents are poisoned by an
    // unmodeled 1042-S income code (missing != 0); the deduction line
    // (a parameter) and the withholding lines (stated on the forms)
    // stay stated.
    let mut put = |k: &str, v: Money, poisoned: bool| {
        lines.insert(
            k.to_string(),
            if poisoned {
                FactResult::Incomplete
            } else {
                FactResult::Complete(v)
            },
        );
    };
    put("1a", line1a, unmodeled_code);
    put("1k", line1k, unmodeled_code);
    put("1z", line1z, unmodeled_code);
    put("8", line8, unmodeled_code);
    put("9", line9, unmodeled_code);
    put("11a", line11a, unmodeled_code);
    put("11b", line11a, unmodeled_code);
    put("12", line12, false);
    put("14", line14, unmodeled_code);
    put("15", line15, unmodeled_code);
    put("16", line16, unmodeled_code);
    put("18", line18, unmodeled_code);
    put("22", line22, unmodeled_code);
    put("24", line24, unmodeled_code);
    put("25a", line25a, false);
    put("25d", line25d, false);
    put("25g", line25g, false);
    put("32", line32, false);
    put("33", line33, unmodeled_code);
    put("34", line34, unmodeled_code);
    put("37", line37, unmodeled_code);
    put("eci_wages_gross", w2_wages + code20_gross, false);
    put("treaty_wage_exclusion", wage_exclusion, false);
    put("treaty_claim_on_return", claim_on_return, false);
    put("taxable_scholarship", taxable_scholarship, false);

    let incomplete: Vec<String> = lines
        .iter()
        .filter(|(_, f)| matches!(f, FactResult::Incomplete))
        .map(|(k, _)| k.clone())
        .collect();
    notes.push(
        "Form 1040-NR student route (v1): effectively connected wages and scholarship \
         only; Schedule NEC, Schedule A itemized deductions, all credits, and estimated \
         payments are out of scope (docs/KNOWN_LIMITS.md)"
            .to_string(),
    );

    NrComputed {
        complete: incomplete.is_empty(),
        incomplete,
        lines,
        notes,
        findings,
        tax_method: if unmodeled_code {
            None
        } else {
            Some(tax_method.to_string())
        },
        schedule_oi,
        determination: Some(determination),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{F1042sInput, Form8843Data, NraBlock, W2Input};

    fn treaties() -> TreatyParams {
        TreatyParams::load_2025().expect("treaty params load")
    }

    fn params() -> TaxParams {
        TaxParams::load_2025().expect("federal params load")
    }

    fn w2(wages: i64, wh: i64) -> W2Input {
        W2Input {
            employer_ein: "12-3456789".into(),
            wages: Money::from_cents(wages),
            federal_withholding: Money::from_cents(wh),
            social_security_wages: None,
            social_security_tax: None,
            medicare_wages: None,
            medicare_tax: None,
            state_wages: None,
            state_withholding: None,
        }
    }

    fn s1042(code: u32, gross: i64, wh: i64, exempt: bool) -> F1042sInput {
        F1042sInput {
            recipient_name: None,
            payer_name: "State University".into(),
            income_code: code,
            gross_income: Money::from_cents(gross),
            federal_withholding: Money::from_cents(wh),
            exemption_code: exempt.then(|| "04".to_string()),
            treaty_country: None,
            treaty_article: None,
        }
    }

    fn nr_input(visa: &str, cc: &str, years: u32) -> Form1040Input {
        Form1040Input {
            f1042ss: Vec::new(),
            nra: Some(NraBlock {
                visa_type: visa.into(),
                citizenship: cc.into(),
                first_calendar_year_in_status: 2025 - years + 1,
                calendar_years_in_status_including_current: years,
                form_8843: Some(Form8843Data::default()),
            }),
            status: FilingStatus::Single,
            w2s: vec![],
            interest_income: Money::ZERO,
            ordinary_dividends: Money::ZERO,
            f1099_ints: vec![],
            f1099_divs: vec![],
            f1099_bs: vec![],
            f1099_necs: vec![],
            dependents: vec![],
            identity: None,
            state: None,
        }
    }

    fn cents(nr: &NrComputed, key: &str) -> i64 {
        nr.lines[key].value().expect("line has a value").cents()
    }

    #[test]
    fn cn_all_on_w2_claims_exemption_on_return() {
        // NR-001 shape: W-2 $8,000, WH $300, CN F-1.
        let mut input = nr_input("F-1", "CN", 2);
        input.w2s = vec![w2(800_000, 30_000)];
        let nr = compute_1040nr(&input, &params(), &treaties());
        assert!(nr.complete, "findings: {:?}", nr.findings);
        assert_eq!(cents(&nr, "1a"), 300_000);
        assert_eq!(cents(&nr, "1k"), 500_000);
        assert_eq!(cents(&nr, "16"), 30_300);
        assert_eq!(cents(&nr, "37"), 300);
        assert_eq!(nr.schedule_oi.len(), 1);
        assert_eq!(nr.schedule_oi[0].treaty_article, "20(c)");
    }

    #[test]
    fn cn_split_reporting_same_result() {
        // NR-002 shape: W-2 $3,000 WH $500 + exempt 1042-S code 20 $5,000.
        let mut input = nr_input("F-1", "CN", 1);
        input.w2s = vec![w2(300_000, 50_000)];
        input.f1042ss = vec![s1042(20, 500_000, 0, true)];
        let nr = compute_1040nr(&input, &params(), &treaties());
        assert!(nr.complete);
        assert_eq!(cents(&nr, "1a"), 300_000);
        assert_eq!(cents(&nr, "treaty_claim_on_return"), 0);
        assert_eq!(cents(&nr, "34"), 19_700);
    }

    #[test]
    fn in_gets_standard_deduction() {
        // NR-003 shape: IN F-1, W-2 $20,000, WH $1,800.
        let mut input = nr_input("F-1", "IN", 3);
        input.w2s = vec![w2(2_000_000, 180_000)];
        let nr = compute_1040nr(&input, &params(), &treaties());
        assert!(nr.complete);
        assert_eq!(cents(&nr, "12"), 1_575_000);
        assert_eq!(cents(&nr, "15"), 425_000);
        assert_eq!(cents(&nr, "16"), 42_800);
        assert_eq!(cents(&nr, "34"), 137_200);
    }

    #[test]
    fn cn_scholarship_exempt_under_art_20b() {
        // NR-004 shape: W-2 $6,500 WH $200 + exempt code 15 $4,000.
        let mut input = nr_input("F-1", "CN", 2);
        input.w2s = vec![w2(650_000, 20_000)];
        input.f1042ss = vec![s1042(15, 400_000, 0, true)];
        let nr = compute_1040nr(&input, &params(), &treaties());
        assert!(nr.complete);
        assert_eq!(cents(&nr, "taxable_scholarship"), 0);
        assert_eq!(cents(&nr, "1k"), 900_000);
        assert_eq!(cents(&nr, "16"), 15_100);
        assert_eq!(cents(&nr, "34"), 4_900);
        assert_eq!(nr.schedule_oi.len(), 2);
    }

    #[test]
    fn unmodeled_income_code_poisons_income_lines() {
        let mut input = nr_input("F-1", "CN", 1);
        input.w2s = vec![w2(300_000, 0)];
        input.f1042ss = vec![s1042(16, 100_000, 0, false)]; // interest on 1042-S
        let nr = compute_1040nr(&input, &params(), &treaties());
        assert!(!nr.complete);
        assert!(matches!(nr.lines["1a"], FactResult::Incomplete));
        assert!(matches!(nr.lines["25g"], FactResult::Complete(_)));
        assert!(nr
            .findings
            .iter()
            .any(|f| f.code == "f1042s_income_code_not_modeled"));
    }

    #[test]
    fn over_five_years_refused() {
        let input = nr_input("F-1", "CN", 6);
        let nr = compute_1040nr(&input, &params(), &treaties());
        assert!(!nr.complete);
        assert!(nr
            .findings
            .iter()
            .any(|f| f.code == "nra_determination_failed"));
    }

    #[test]
    fn fica_withheld_flagged_for_exempt_student() {
        let mut input = nr_input("F-1", "CN", 1);
        let mut w = w2(300_000, 10_000);
        w.social_security_tax = Some(Money::from_cents(18_600));
        input.w2s = vec![w];
        let nr = compute_1040nr(&input, &params(), &treaties());
        assert!(nr
            .findings
            .iter()
            .any(|f| f.code == "fica_withheld_while_exempt"));
    }

    #[test]
    fn unmodeled_country_computes_without_treaty() {
        let mut input = nr_input("J-1", "DE", 2);
        input.w2s = vec![w2(800_000, 30_000)];
        let nr = compute_1040nr(&input, &params(), &treaties());
        assert!(nr.complete);
        assert_eq!(cents(&nr, "1a"), 800_000); // no exemption
        assert_eq!(cents(&nr, "12"), 0); // no standard deduction
        assert!(nr.findings.iter().any(|f| f.code == "treaty_not_modeled"));
    }
}
