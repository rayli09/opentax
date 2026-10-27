//! Filing-packet construction: the schedules that accompany Form 1040.
//!
//! [`build_packet`] turns a computed return into one [`PacketForm`] per
//! schedule (one per 1099-NEC engagement for Schedule C), each carrying
//! the AcroForm fill values for that form's geometrically verified field
//! map (`forms/*-2025-field-map.json`, derived exactly like the Form 1040
//! map: widget Rect vs `pdftotext -bbox` printed labels), whether the
//! form applies to this return at all, and human-readable notes covering
//! every mapped line that stays blank and why.
//!
//! [`schedules_view`] exposes the same derivations as an internal JSON
//! view (integer cents, internal keys — NOT printed-line keys) for the
//! CLI's compute output. The public line model is unchanged.
//!
//! Modeling rules that shape the packet (each also lands in the form's
//! manifest notes and docs/KNOWN_LIMITS.md):
//!
//! - Every stored 1099-B transaction is listed on Form 8949. In v1 all
//!   entries are treated as basis-reported: short-term → Part I Box A,
//!   long-term → Part II Box D (the input carries no basis-reported
//!   flag). Form 8949 totals therefore land on Schedule D lines 1b/8b —
//!   the printed TY2025 forms route 8949-reported totals there; the
//!   1a/8a aggregate shortcut is only for transactions NOT listed on
//!   Form 8949.
//! - Form 8949 has 11 transaction rows per part on the TY2025 fillable.
//!   A group with more rows fills the first 11 and reports the overflow
//!   in the manifest notes (continuation pages are out of scope);
//!   Schedule D still carries the engine totals for ALL transactions.
//! - Schedule C expenses are a single aggregate per engagement (the
//!   input models no itemization): the total is itemized as one Part V
//!   row, totaled on line 48, and carried to line 27b (TY2025: 27a is
//!   the Form 7205 energy-efficient-buildings deduction).
//! - Schedule SE line 8a takes the engine's W-2 Box 1 wage total as the
//!   proxy for Box 3 Social Security wages (the same proxy the engine's
//!   validation reports); the Social Security wage base on line 7 is
//!   pre-printed on the TY2025 form and has no fillable field.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};
use tax_core::{FactResult, FilingStatus, Money, TaxParams};

use crate::worksheets::{
    deductible_part_of_se_tax, schedule_1a_senior_deduction, schedule_8812, schedule_se_tax,
};
use crate::{
    format_amount, mask_ssn, normalize_ssn, BrokerTxn, ComputedReturn, F1040FieldMap, FillValue,
    Form1040Input, TxnTerm,
};

/// One schedule's worth of packet output.
#[derive(Debug, Clone, PartialEq)]
pub struct PacketForm {
    /// Canonical form id — also the template/file stem (`"f1040sd"`).
    pub form: &'static str,
    /// Human label (`"Schedule D"`).
    pub label: &'static str,
    /// Output file name inside the packet directory (`"f1040sd.pdf"`;
    /// Schedule C repeats as `"f1040sc.pdf"`, `"f1040sc-2.pdf"`, ...).
    pub file_name: String,
    /// Whether this form applies to the return at all.
    pub applicable: bool,
    /// Why not, when `applicable` is false.
    pub not_applicable_reason: Option<String>,
    /// Fill values resolved through the form's field map. Empty when the
    /// form does not apply.
    pub values: Vec<FillValue>,
    /// Caveats for the manifest: lines left blank and why, modeling
    /// divergences, overflow reports.
    pub notes: Vec<String>,
    /// This form's fragment of the internal `schedules` view.
    pub view: Value,
}

impl PacketForm {
    fn not_applicable(
        form: &'static str,
        label: &'static str,
        file_name: String,
        reason: String,
        view: Value,
    ) -> PacketForm {
        PacketForm {
            form,
            label,
            file_name,
            applicable: false,
            not_applicable_reason: Some(reason),
            values: Vec::new(),
            notes: Vec::new(),
            view,
        }
    }
}

/// The bundled field map for one packet form, parsed from the
/// geometrically derived JSON in `forms/`.
fn packet_field_map(form: &str) -> F1040FieldMap {
    let text = match form {
        "f1040sd" => include_str!("../forms/f1040sd-2025-field-map.json"),
        "f8949" => include_str!("../forms/f8949-2025-field-map.json"),
        "f1040s1" => include_str!("../forms/f1040s1-2025-field-map.json"),
        "f1040s2" => include_str!("../forms/f1040s2-2025-field-map.json"),
        "f1040sc" => include_str!("../forms/f1040sc-2025-field-map.json"),
        "f1040sse" => include_str!("../forms/f1040sse-2025-field-map.json"),
        "f10408812" => include_str!("../forms/f10408812-2025-field-map.json"),
        "f1040s1a" => include_str!("../forms/f1040s1a-2025-field-map.json"),
        other => panic!("no bundled field map for packet form {other}"),
    };
    serde_json::from_str(text).unwrap_or_else(|e| panic!("bundled {form} field map must parse: {e}"))
}

/// Resolve a value dictionary (map line key → formatted string) through
/// a field map into fill values. Checkbox entries take their on-state
/// from the map entry itself; the dictionary only decides WHETHER the
/// box is checked. SSN entries report masked.
fn resolve_values(map: &F1040FieldMap, dict: &HashMap<String, String>) -> Vec<FillValue> {
    map.entries
        .iter()
        .filter_map(|entry| {
            let raw = dict.get(entry.source_line())?;
            let value = if entry.kind == "checkbox" {
                entry.value.clone().unwrap_or_else(|| raw.clone())
            } else {
                raw.clone()
            };
            Some(FillValue {
                line: entry.line.clone(),
                field: entry.qname.clone(),
                value,
                display_value: (entry.kind == "identity_ssn").then(|| mask_ssn(raw)),
                section: entry
                    .kind
                    .starts_with("identity_")
                    .then(|| "identity".to_string()),
            })
        })
        .collect()
}

/// A computed line's value, when the engine produced one.
fn line(lines: &BTreeMap<String, FactResult<Money>>, key: &str) -> Option<Money> {
    lines.get(key).and_then(|f| f.value().copied())
}

fn amt(dict: &mut HashMap<String, String>, key: &str, value: Money) {
    dict.insert(key.to_string(), format_amount(value));
}

fn text(dict: &mut HashMap<String, String>, key: &str, value: &str) {
    if !value.is_empty() {
        dict.insert(key.to_string(), value.to_string());
    }
}

fn check(dict: &mut HashMap<String, String>, key: &str) {
    dict.insert(key.to_string(), "1".to_string());
}

/// The taxpayer's display name from the stored identity ("First Last",
/// either part optional), when any part is stored.
fn identity_name(input: &Form1040Input) -> Option<String> {
    let id = input.identity.as_ref()?;
    let parts: Vec<&str> = [
        id.person.first_name.as_deref(),
        id.person.last_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Fill the name/SSN header entries of one form's dictionary from the
/// stored identity, recording notes for whatever is absent. SSNs are
/// stored normalized; the value written is digits-only (the report
/// value is masked by [`resolve_values`]).
fn header(
    dict: &mut HashMap<String, String>,
    notes: &mut Vec<String>,
    input: &Form1040Input,
    name_key: &str,
    ssn_key: &str,
) {
    match identity_name(input) {
        Some(name) => text(dict, name_key, &name),
        None => notes.push(
            "header name left blank: no taxpayer name is stored in the return's identity"
                .to_string(),
        ),
    }
    match input
        .identity
        .as_ref()
        .and_then(|id| id.person.ssn.as_deref())
        .and_then(normalize_ssn)
    {
        Some(digits) => text(dict, ssn_key, &digits),
        None => notes.push(
            "header SSN left blank: no taxpayer SSN is stored in the return's identity"
                .to_string(),
        ),
    }
}

/// Per-term totals over a set of 1099-B transactions:
/// (proceeds, basis, adjustments, gain).
#[derive(Debug, Clone, Copy, Default)]
struct GroupTotals {
    proceeds: Money,
    basis: Money,
    adjustments: Money,
    gain: Money,
}

fn group_totals(txns: &[&BrokerTxn]) -> GroupTotals {
    let mut t = GroupTotals::default();
    for txn in txns {
        if let (Some(p), Some(b)) = (txn.proceeds, txn.cost_basis) {
            t.proceeds = t.proceeds + p;
            t.basis = t.basis + b;
            t.adjustments = t.adjustments + txn.adjustment;
            if let Some(g) = txn.gain() {
                t.gain = t.gain + g;
            }
        }
    }
    t
}

/// All stored 1099-B transactions across every 1099-B entry, in stored
/// order, split by holding period.
fn all_txns(input: &Form1040Input) -> (Vec<&BrokerTxn>, Vec<&BrokerTxn>) {
    let mut st = Vec::new();
    let mut lt = Vec::new();
    for b in &input.f1099_bs {
        for txn in &b.transactions {
            match txn.term {
                TxnTerm::Short => st.push(txn),
                TxnTerm::Long => lt.push(txn),
            }
        }
    }
    (st, lt)
}

/// An ISO `YYYY-MM-DD` date in the form's "(Mo., day, yr.)" display
/// (`MM/DD/YYYY`); anything else passes through unchanged.
fn form_date(iso: &str) -> String {
    let parts: Vec<&str> = iso.split('-').collect();
    if parts.len() == 3
        && parts[0].len() == 4
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        format!("{}/{}/{}", parts[1], parts[2], parts[0])
    } else {
        iso.to_string()
    }
}

// ---------------------------------------------------------------------------
// Schedule D + Form 8949
// ---------------------------------------------------------------------------

fn schedule_d(input: &Form1040Input, computed: &ComputedReturn) -> PacketForm {
    let lines = &computed.lines;
    if input.f1099_bs.is_empty() {
        return PacketForm::not_applicable(
            "f1040sd",
            "Schedule D",
            "f1040sd.pdf".to_string(),
            "no 1099-B entries are stored in this return".to_string(),
            json!({"applicable": false}),
        );
    }
    let (st, lt) = all_txns(input);
    let st_totals = group_totals(&st);
    let lt_totals = group_totals(&lt);
    let net_short = line(lines, "schd_net_short").unwrap_or(st_totals.gain);
    let net_long = line(lines, "schd_net_long").unwrap_or(lt_totals.gain);
    let combined = net_short + net_long;
    let line7 = line(lines, "7").unwrap_or(combined);

    let mut dict = HashMap::new();
    let mut notes = vec![
        "totals are entered on lines 1b/8b, not 1a/8a: every stored transaction is listed on Form 8949, and the printed TY2025 Schedule D routes Form 8949 Box A/Box D totals to lines 1b/8b (lines 1a/8a are only for transactions NOT reported on Form 8949)".to_string(),
        "lines 1a, 2, 3, 8a, 9, 10 stay blank: v1 treats every 1099-B entry as basis-reported (Box A short-term / Box D long-term); no Box B/C/E/F transactions, Form 8949 unreported-basis adjustments beyond column (g), or other Part I/II sources are modeled".to_string(),
        "lines 4, 5, 11, 12 stay blank: no Schedule D worksheet amounts (installment sales, like-kind exchanges, Forms 4797/6252/4684/6781/8824) are ingested".to_string(),
        "lines 6 and 14 stay blank: prior-year capital loss carryovers IN are computed by the engine for the current year (schd_carryover_short/long) but are not consumed as inputs in v1".to_string(),
        "line 13 stays blank: capital gain distributions (Form 1099-DIV box 2a) are not ingested".to_string(),
        "lines 18 and 19 stay blank: the 28% Rate Gain Worksheet and Unrecaptured Section 1250 Gain Worksheet are out of scope (no collectibles or section 1250 gain types are ingested); line 20 is answered accordingly".to_string(),
        "the qualified opportunity fund (QOF) question at the top of page 1 stays unanswered: no QOF disposition is modeled".to_string(),
    ];
    header(&mut dict, &mut notes, input, "name", "ssn");
    if !st.is_empty() {
        amt(&mut dict, "1b_d", st_totals.proceeds);
        amt(&mut dict, "1b_e", st_totals.basis);
        amt(&mut dict, "1b_g", st_totals.adjustments);
        amt(&mut dict, "1b_h", st_totals.gain);
    }
    amt(&mut dict, "7", net_short);
    if !lt.is_empty() {
        amt(&mut dict, "8b_d", lt_totals.proceeds);
        amt(&mut dict, "8b_e", lt_totals.basis);
        amt(&mut dict, "8b_g", lt_totals.adjustments);
        amt(&mut dict, "8b_h", lt_totals.gain);
    }
    amt(&mut dict, "15", net_long);
    amt(&mut dict, "16", combined);
    // Part III flow, following the printed form's own routing.
    if combined.cents() > 0 {
        // Line 16 is a gain: line 17 is reached.
        if net_long.cents() > 0 {
            check(&mut dict, "17_yes");
            // Lines 18/19 are blank in v1 scope and no Form 4952 is
            // filed, so line 20's answer is Yes — the same QDCG path
            // the engine's line 16 method takes.
            check(&mut dict, "20_yes");
        } else {
            check(&mut dict, "17_no");
            notes.push(
                "lines 18-21 skipped per the form's line 17 'No' routing; line 22 is still completed"
                    .to_string(),
            );
        }
    } else if combined.cents() < 0 {
        // Line 16 is a loss: skip 17-20, complete 21 and 22.
        amt(&mut dict, "21", line7);
        notes.push(
            "line 21 carries the section 1211(b)-limited loss (the smaller of the line 16 loss or the $3,000/$1,500 limit), matching Form 1040 line 7; lines 17-20 are skipped per the form's loss routing"
                .to_string(),
        );
    } else {
        notes.push(
            "line 16 is zero: lines 17-21 are skipped per the form's routing; line 22 is still completed"
                .to_string(),
        );
    }
    // Line 22 is always completed: qualified dividends on Form 1040
    // line 3a?
    let qualified = line(lines, "3a").unwrap_or(Money::ZERO);
    if qualified.cents() > 0 {
        check(&mut dict, "22_yes");
    } else {
        check(&mut dict, "22_no");
    }
    let map = packet_field_map("f1040sd");
    PacketForm {
        form: "f1040sd",
        label: "Schedule D",
        file_name: "f1040sd.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values: resolve_values(&map, &dict),
        notes,
        view: json!({
            "applicable": true,
            "net_short_cents": net_short.cents(),
            "net_long_cents": net_long.cents(),
            "combined_cents": combined.cents(),
            "carryover_short_cents": line(lines, "schd_carryover_short").map(|m| m.cents()),
            "carryover_long_cents": line(lines, "schd_carryover_long").map(|m| m.cents()),
        }),
    }
}

/// Transaction rows on one Form 8949 page part (TY2025 fillable).
const F8949_ROWS_PER_PART: usize = 11;

fn form_8949(input: &Form1040Input) -> PacketForm {
    if input.f1099_bs.is_empty() {
        return PacketForm::not_applicable(
            "f8949",
            "Form 8949",
            "f8949.pdf".to_string(),
            "no 1099-B entries are stored in this return".to_string(),
            json!({"applicable": false}),
        );
    }
    let (st, lt) = all_txns(input);
    let mut dict = HashMap::new();
    let mut notes = vec![
        "v1 categorization: every stored 1099-B transaction is treated as basis-reported — short-term transactions are reported in Part I with Box A checked, long-term in Part II with Box D checked (the 1099-B input carries no basis-reported flag)".to_string(),
    ];
    header(&mut dict, &mut notes, input, "name", "ssn");
    // Page 2 repeats the name/SSN header; fill it from the same identity
    // but discard the duplicate absence notes.
    let mut scratch_notes = Vec::new();
    header(&mut dict, &mut scratch_notes, input, "name2", "ssn2");

    let mut fill_group = |txns: &[&BrokerTxn], prefix: &str, box_key: &str| -> (usize, GroupTotals) {
        if txns.is_empty() {
            return (0, GroupTotals::default());
        }
        check(&mut dict, box_key);
        let listed = &txns[..txns.len().min(F8949_ROWS_PER_PART)];
        for (i, txn) in listed.iter().enumerate() {
            let row = i + 1;
            text(&mut dict, &format!("{prefix}_r{row}_a"), &txn.description);
            if let Some(d) = &txn.date_acquired {
                text(&mut dict, &format!("{prefix}_r{row}_b"), &form_date(d));
            }
            if let Some(d) = &txn.date_sold {
                text(&mut dict, &format!("{prefix}_r{row}_c"), &form_date(d));
            }
            if let Some(p) = txn.proceeds {
                amt(&mut dict, &format!("{prefix}_r{row}_d"), p);
            }
            if let Some(b) = txn.cost_basis {
                amt(&mut dict, &format!("{prefix}_r{row}_e"), b);
            }
            if txn.adjustment.cents() != 0 {
                amt(&mut dict, &format!("{prefix}_r{row}_g"), txn.adjustment);
                if let Some(code) = &txn.adjustment_code {
                    text(&mut dict, &format!("{prefix}_r{row}_f"), code);
                }
            }
            if let Some(g) = txn.gain() {
                amt(&mut dict, &format!("{prefix}_r{row}_h"), g);
            }
        }
        let totals = group_totals(listed);
        amt(&mut dict, &format!("{prefix}_tot_d"), totals.proceeds);
        amt(&mut dict, &format!("{prefix}_tot_e"), totals.basis);
        amt(&mut dict, &format!("{prefix}_tot_g"), totals.adjustments);
        amt(&mut dict, &format!("{prefix}_tot_h"), totals.gain);
        (listed.len(), totals)
    };
    let (st_listed, st_page_totals) = fill_group(&st, "st", "box_a");
    let (lt_listed, lt_page_totals) = fill_group(&lt, "lt", "box_d");

    if st.len() > F8949_ROWS_PER_PART || lt.len() > F8949_ROWS_PER_PART {
        notes.push(format!(
            "OVERFLOW: {} short-term and {} long-term transactions are stored, but one Form 8949 page part holds {} rows; only the first {} of each are listed and the page totals cover the listed rows only. Continuation pages are out of scope for this packet — the unlisted transactions are NOT in this file, though Schedule D carries the full engine totals",
            st.len(),
            lt.len(),
            F8949_ROWS_PER_PART,
            F8949_ROWS_PER_PART
        ));
    }
    let no_dates = st
        .iter()
        .chain(lt.iter())
        .filter(|t| t.date_acquired.is_none() || t.date_sold.is_none())
        .count();
    if no_dates > 0 {
        notes.push(format!(
            "{no_dates} transaction(s) have no date acquired and/or date sold stored: columns (b)/(c) stay blank for those rows (dates are display-only and never computed on)"
        ));
    }
    let uncoded = st
        .iter()
        .chain(lt.iter())
        .filter(|t| t.adjustment.cents() != 0 && t.adjustment_code.is_none())
        .count();
    if uncoded > 0 {
        notes.push(format!(
            "{uncoded} transaction(s) carry an adjustment amount with no adjustment code stored: column (f) stays blank for those rows (the code, e.g. W for wash sales, comes from the broker statement)"
        ));
    }
    let incomplete_rows = st
        .iter()
        .chain(lt.iter())
        .filter(|t| t.proceeds.is_none() || t.cost_basis.is_none())
        .count();
    if incomplete_rows > 0 {
        notes.push(format!(
            "{incomplete_rows} transaction(s) are missing proceeds and/or cost basis as stored: their amount cells stay blank (a complete return cannot contain these — the engine marks the dependent lines Incomplete)"
        ));
    }
    let map = packet_field_map("f8949");
    PacketForm {
        form: "f8949",
        label: "Form 8949",
        file_name: "f8949.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values: resolve_values(&map, &dict),
        notes,
        view: json!({
            "applicable": true,
            "short_term_transactions": st.len(),
            "long_term_transactions": lt.len(),
            "short_term_listed": st_listed,
            "long_term_listed": lt_listed,
            "short_term_totals": totals_json(&st_page_totals),
            "long_term_totals": totals_json(&lt_page_totals),
        }),
    }
}

fn totals_json(t: &GroupTotals) -> Value {
    json!({
        "proceeds_cents": t.proceeds.cents(),
        "basis_cents": t.basis.cents(),
        "adjustments_cents": t.adjustments.cents(),
        "gain_cents": t.gain.cents(),
    })
}

// ---------------------------------------------------------------------------
// Schedule 1 + Schedule 2
// ---------------------------------------------------------------------------

fn schedule_1(input: &Form1040Input, computed: &ComputedReturn) -> PacketForm {
    let lines = &computed.lines;
    let schc_net = line(lines, "schc_net_profit").unwrap_or(Money::ZERO);
    let se_deduction = line(lines, "10").unwrap_or(Money::ZERO);
    if input.f1099_necs.is_empty() {
        return PacketForm::not_applicable(
            "f1040s1",
            "Schedule 1",
            "f1040s1.pdf".to_string(),
            "no 1099-NEC entries are stored: in v1 Schedule C business income and the deductible part of SE tax are Schedule 1's only sources"
                .to_string(),
            json!({"applicable": false}),
        );
    }
    let mut dict = HashMap::new();
    let mut notes = vec![
        "printed line 9 ('Total other income' from lines 8a-8z) stays blank: no 8a-8z other-income items are modeled in v1; the Schedule C total enters on line 3 and the total additional income on printed line 10".to_string(),
        "all other Part I lines (1, 2, 4-7) and Part II lines (11-14, 16-25) stay blank: no such income or adjustments are modeled in v1".to_string(),
    ];
    header(&mut dict, &mut notes, input, "name", "ssn");
    amt(&mut dict, "3", schc_net);
    amt(&mut dict, "10", line(lines, "8").unwrap_or(schc_net));
    amt(&mut dict, "15", se_deduction);
    amt(&mut dict, "26", se_deduction);
    let map = packet_field_map("f1040s1");
    PacketForm {
        form: "f1040s1",
        label: "Schedule 1",
        file_name: "f1040s1.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values: resolve_values(&map, &dict),
        notes,
        view: json!({
            "applicable": true,
            "business_income_cents": schc_net.cents(),
            "total_additional_income_cents": line(lines, "8").map(|m| m.cents()),
            "deductible_part_of_se_tax_cents": se_deduction.cents(),
        }),
    }
}

fn schedule_2(input: &Form1040Input, computed: &ComputedReturn) -> PacketForm {
    let lines = &computed.lines;
    let se_tax = line(lines, "sched2_se_tax").unwrap_or(Money::ZERO);
    if se_tax.cents() == 0 {
        return PacketForm::not_applicable(
            "f1040s2",
            "Schedule 2",
            "f1040s2.pdf".to_string(),
            "Form 1040 line 23 (other taxes, including self-employment tax) is zero for this return"
                .to_string(),
            json!({"applicable": false, "se_tax_cents": 0}),
        );
    }
    let mut dict = HashMap::new();
    let mut notes = vec![
        "only line 4 (self-employment tax) and the line 21 total are filled: no other Schedule 2 taxes (AMT, excess advance premium tax credit, additional taxes on IRAs/HSAs, household employment taxes) are modeled in v1".to_string(),
        "the Form 4361/4029 exemption checkboxes beside line 4 stay unchecked: no minister or religious-order exemption is modeled".to_string(),
    ];
    header(&mut dict, &mut notes, input, "name", "ssn");
    amt(&mut dict, "4", se_tax);
    amt(&mut dict, "21", line(lines, "23").unwrap_or(se_tax));
    let map = packet_field_map("f1040s2");
    PacketForm {
        form: "f1040s2",
        label: "Schedule 2",
        file_name: "f1040s2.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values: resolve_values(&map, &dict),
        notes,
        view: json!({"applicable": true, "se_tax_cents": se_tax.cents()}),
    }
}

// ---------------------------------------------------------------------------
// Schedule C (per 1099-NEC engagement) + Schedule SE
// ---------------------------------------------------------------------------

fn schedules_c(input: &Form1040Input) -> Vec<PacketForm> {
    if input.f1099_necs.is_empty() {
        return vec![PacketForm::not_applicable(
            "f1040sc",
            "Schedule C",
            "f1040sc.pdf".to_string(),
            "no 1099-NEC entries are stored in this return".to_string(),
            json!({"applicable": false, "engagements": []}),
        )];
    }
    let map = packet_field_map("f1040sc");
    let multiple = input.f1099_necs.len() > 1;
    input
        .f1099_necs
        .iter()
        .enumerate()
        .map(|(i, nec)| {
            let gross = nec.nonemployee_compensation;
            let expenses = nec.expenses;
            let net = gross - expenses;
            let mut dict = HashMap::new();
            let mut notes = vec![
                "expenses are a single aggregate in v1 (the 1099-NEC entry models no itemization): the total is listed as one Part V 'Other Expenses' row, totaled on line 48, and carried to line 27b (TY2025 line 27a is the Form 7205 deduction, not other expenses)".to_string(),
                "Part I lines 2, 4, 6 and all Part II category lines (8-27a) stay blank: returns/allowances, cost of goods sold, other income, and itemized expense categories are not modeled".to_string(),
                "header boxes A (principal business or profession) and B (business code) stay blank: not modeled — complete them from the business's NAICS code before filing".to_string(),
                "header box E (business address), F (accounting method), G (material participation), H (started/acquired), I and J (Form 1099 questions) stay blank: not modeled in v1".to_string(),
                "line 30 (business use of home) stays blank and line 32's at-risk boxes stay unchecked: neither is modeled; if this Schedule C shows a loss, one line 32 box must be checked before filing".to_string(),
            ];
            // Proprietor header: the taxpayer's name/SSN from identity.
            match identity_name(input) {
                Some(name) => text(&mut dict, "proprietor_name", &name),
                None => notes.push(
                    "proprietor name left blank: no taxpayer name is stored in the return's identity"
                        .to_string(),
                ),
            }
            match input
                .identity
                .as_ref()
                .and_then(|id| id.person.ssn.as_deref())
                .and_then(normalize_ssn)
            {
                Some(digits) => text(&mut dict, "ssn", &digits),
                None => notes.push(
                    "proprietor SSN left blank: no taxpayer SSN is stored in the return's identity"
                        .to_string(),
                ),
            }
            // Business header: the engagement (payer) names the business.
            text(&mut dict, "business_name", &nec.payer_name);
            match &nec.payer_ein {
                Some(ein) if !ein.trim().is_empty() => text(&mut dict, "ein", ein),
                _ => notes.push(
                    "header box D (EIN) left blank: no payer EIN is stored for this engagement"
                        .to_string(),
                ),
            }
            amt(&mut dict, "1", gross);
            amt(&mut dict, "3", gross);
            amt(&mut dict, "5", gross);
            amt(&mut dict, "7", gross);
            if expenses.cents() != 0 {
                text(
                    &mut dict,
                    "partv_item1_desc",
                    "Business expenses — single aggregate (not itemized in v1)",
                );
                amt(&mut dict, "partv_item1_amt", expenses);
                amt(&mut dict, "48", expenses);
                amt(&mut dict, "27b", expenses);
                amt(&mut dict, "28", expenses);
            } else {
                amt(&mut dict, "28", Money::ZERO);
                notes.push(
                    "no expenses are stored for this engagement: Part V, line 48 and line 27b stay blank and line 28 is zero"
                        .to_string(),
                );
            }
            amt(&mut dict, "29", net);
            amt(&mut dict, "31", net);
            let file_name = if i == 0 {
                "f1040sc.pdf".to_string()
            } else {
                format!("f1040sc-{}.pdf", i + 1)
            };
            if multiple {
                notes.push(format!(
                    "this is Schedule C {} of {} (one per 1099-NEC engagement)",
                    i + 1,
                    input.f1099_necs.len()
                ));
            }
            PacketForm {
                form: "f1040sc",
                label: "Schedule C",
                file_name,
                applicable: true,
                not_applicable_reason: None,
                values: resolve_values(&map, &dict),
                notes,
                view: json!({
                    "applicable": true,
                    "payer_name": nec.payer_name,
                    "gross_receipts_cents": gross.cents(),
                    "expenses_cents": expenses.cents(),
                    "net_cents": net.cents(),
                }),
            }
        })
        .collect()
}

fn schedule_se(input: &Form1040Input, computed: &ComputedReturn, params: &TaxParams) -> PacketForm {
    let lines = &computed.lines;
    let schc_net = line(lines, "schc_net_profit").unwrap_or(Money::ZERO);
    if input.f1099_necs.is_empty() || schc_net.cents() <= 0 {
        let reason = if input.f1099_necs.is_empty() {
            "no 1099-NEC entries are stored in this return".to_string()
        } else {
            "the total Schedule C net profit is not positive, so there are no net earnings from self-employment"
                .to_string()
        };
        return PacketForm::not_applicable(
            "f1040sse",
            "Schedule SE",
            "f1040sse.pdf".to_string(),
            reason,
            json!({"applicable": false}),
        );
    }
    let wages: Money = input.w2s.iter().map(|w| w.wages).sum();
    let se = schedule_se_tax(schc_net, wages, params);
    let wage_base = Money::from_cents(params.social_security_wage_base_cents);
    let ss_room = Money::from_cents((wage_base - wages).cents().max(0));
    let deductible = deductible_part_of_se_tax(se.tax);

    let mut dict = HashMap::new();
    let mut notes = vec![
        "line 8a is filled with the engine's proxy: total W-2 Box 1 wages. Schedule SE line 8a asks for Box 3 (Social Security) wages, which the return does not store separately; the engine's SE computation uses the same Box 1 proxy (see the nec_ss_wage_proxy validation finding)".to_string(),
        "line 7 (the $176,100 Social Security wage base for 2025) is pre-printed on the TY2025 form and has no fillable field".to_string(),
        "lines 1a/1b (farm income), 4b (optional methods), 5a/5b (church employee income), 8b/8c (unreported tips / Form 8919 wages) stay blank: none are modeled in v1".to_string(),
        "the computation follows the engine's simplified Schedule SE exactly; where the printed form's flow differs (e.g. the line 8a instruction to skip lines 8b-10 when wages reach the wage base), the filled values are still arithmetically the form's: line 9 is floored at zero and line 10 follows from it".to_string(),
    ];
    if se.net_earnings.cents() < 40_000 {
        notes.push(
            "note: net earnings are under $400 — the printed form's line 4c instruction says no SE tax is owed below $400 of net earnings, but the engine does not apply that filing threshold and computes the tax; the packet follows the engine (Schedule 2 line 4 carries the same amount)"
                .to_string(),
        );
    }
    header(&mut dict, &mut notes, input, "name", "ssn");
    amt(&mut dict, "2", schc_net);
    amt(&mut dict, "3", schc_net);
    amt(&mut dict, "4a", se.net_earnings);
    amt(&mut dict, "4c", se.net_earnings);
    amt(&mut dict, "6", se.net_earnings);
    amt(&mut dict, "8a", wages);
    amt(&mut dict, "8d", wages);
    amt(&mut dict, "9", ss_room);
    amt(&mut dict, "10", se.ss_part);
    amt(&mut dict, "11", se.medicare_part);
    amt(&mut dict, "12", se.tax);
    amt(&mut dict, "13", deductible);
    let map = packet_field_map("f1040sse");
    PacketForm {
        form: "f1040sse",
        label: "Schedule SE",
        file_name: "f1040sse.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values: resolve_values(&map, &dict),
        notes,
        view: json!({
            "applicable": true,
            "schc_net_profit_cents": schc_net.cents(),
            "net_earnings_cents": se.net_earnings.cents(),
            "w2_wages_proxy_cents": wages.cents(),
            "ss_room_cents": ss_room.cents(),
            "ss_part_cents": se.ss_part.cents(),
            "medicare_part_cents": se.medicare_part.cents(),
            "tax_cents": se.tax.cents(),
            "deductible_part_cents": deductible.cents(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Schedule 8812 + Schedule 1-A
// ---------------------------------------------------------------------------

fn schedule_8812_form(
    input: &Form1040Input,
    computed: &ComputedReturn,
    params: &TaxParams,
) -> PacketForm {
    let lines = &computed.lines;
    if input.dependents.is_empty() {
        return PacketForm::not_applicable(
            "f10408812",
            "Schedule 8812",
            "f10408812.pdf".to_string(),
            "no dependents are stored in this return".to_string(),
            json!({"applicable": false}),
        );
    }
    let agi = line(lines, "11").unwrap_or(Money::ZERO);
    let income_tax = line(lines, "16").unwrap_or(Money::ZERO);
    let earned_income = line(lines, "sched8812_earned_income")
        .unwrap_or_else(|| input.w2s.iter().map(|w| w.wages).sum());
    // Recompute the worksheet from the same primitives the engine uses;
    // the struct carries every Part I / Part II-A amount the form needs.
    let s = schedule_8812(
        &input.dependents,
        input.status,
        agi,
        income_tax,
        earned_income,
        params,
    );
    let threshold = params.ctc_phaseout_threshold(input.status);
    // Line 10: MAGI excess over the threshold, rounded up to the next
    // multiple of $1,000 (the worksheets.rs formula).
    let excess_cents = (agi - threshold).cents().max(0);
    let line10 = Money::from_cents(if excess_cents == 0 {
        0
    } else {
        (excess_cents + 100_000 - 1) / 100_000 * 100_000
    });
    // Part II-A intermediates the struct does not carry, recomputed with
    // the same formulas: line 17 = min(16a, 16b); lines 19/20 from the
    // earned-income excess over $2,500 at 15%.
    let line16a = s.credit_after_phaseout - s.nonrefundable_credit;
    let line16b = Money::from_cents(i64::from(s.ctc_children) * params.actc_cap_per_child_cents);
    let line17 = Money::from_cents(line16a.cents().min(line16b.cents()));
    let ei_excess = Money::from_cents(
        (s.earned_income - Money::from_cents(params.actc_earned_income_threshold_cents))
            .cents()
            .max(0),
    );
    let line20 = ei_excess.mul_bp(params.actc_rate_bp);

    let mut dict = HashMap::new();
    let mut notes = vec![
        "lines 2a-2d (excluded Puerto Rico / foreign income) stay blank: none is modeled, so line 3 (MAGI) equals line 1 (AGI)".to_string(),
        "Part II-B (certain filers with three or more qualifying children / Puerto Rico residents) is not implemented: lines 21-26 stay blank and line 27 is the Part II-A amount, which can understate the ACTC when line 16b is $5,100 or more and line 20 is less than line 17 (see docs/KNOWN_LIMITS.md)".to_string(),
        "line 15 is reserved for future use on the printed form and stays blank".to_string(),
    ];
    header(&mut dict, &mut notes, input, "name", "ssn");
    amt(&mut dict, "1", agi);
    amt(&mut dict, "3", agi);
    dict.insert("4".to_string(), s.ctc_children.to_string());
    amt(
        &mut dict,
        "5",
        Money::from_cents(i64::from(s.ctc_children) * params.ctc_per_child_cents),
    );
    dict.insert("6".to_string(), s.odc_dependents.to_string());
    amt(
        &mut dict,
        "7",
        Money::from_cents(i64::from(s.odc_dependents) * params.odc_per_dependent_cents),
    );
    amt(&mut dict, "8", s.tentative_credit);
    amt(&mut dict, "9", threshold);
    amt(&mut dict, "10", line10);
    amt(&mut dict, "11", s.phaseout_reduction);
    amt(&mut dict, "12", s.credit_after_phaseout);
    if s.credit_after_phaseout.cents() > 0 {
        check(&mut dict, "12_yes");
        amt(&mut dict, "13", s.credit_limit);
        amt(&mut dict, "14", s.nonrefundable_credit);
        // Part II-A.
        amt(&mut dict, "16a", line16a);
        dict.insert("16b_count".to_string(), s.ctc_children.to_string());
        amt(&mut dict, "16b", line16b);
        amt(&mut dict, "17", line17);
        amt(&mut dict, "18a", s.earned_income);
        if s.earned_income.cents() > params.actc_earned_income_threshold_cents {
            check(&mut dict, "19_yes");
            amt(&mut dict, "19", ei_excess);
        } else {
            check(&mut dict, "19_no");
            notes.push(
                "line 19 stays blank on the 'No' branch (earned income not over $2,500), per the printed form's instruction; line 20 is then zero"
                    .to_string(),
            );
        }
        amt(&mut dict, "20", line20);
        if line16b.cents() >= 510_000 {
            check(&mut dict, "next_yes");
            if line20.cents() < line17.cents() {
                notes.push(
                    "the printed form routes this return to Part II-B (line 16b is $5,100 or more and line 20 is less than line 17); Part II-B is not implemented, so line 27 remains the Part II-A amount"
                        .to_string(),
                );
            }
        } else {
            check(&mut dict, "next_no");
        }
        amt(&mut dict, "27", s.additional_child_tax_credit);
    } else {
        check(&mut dict, "12_no");
        notes.push(
            "line 12 is answered 'No' (the tentative credit does not exceed the phaseout reduction): the form stops there — lines 13, 14 and all of Part II stay blank and no child tax credit or ACTC is claimed"
                .to_string(),
        );
    }
    let map = packet_field_map("f10408812");
    PacketForm {
        form: "f10408812",
        label: "Schedule 8812",
        file_name: "f10408812.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values: resolve_values(&map, &dict),
        notes,
        view: json!({
            "applicable": true,
            "ctc_children": s.ctc_children,
            "odc_dependents": s.odc_dependents,
            "tentative_credit_cents": s.tentative_credit.cents(),
            "phaseout_reduction_cents": s.phaseout_reduction.cents(),
            "credit_after_phaseout_cents": s.credit_after_phaseout.cents(),
            "credit_limit_cents": s.credit_limit.cents(),
            "nonrefundable_credit_cents": s.nonrefundable_credit.cents(),
            "earned_income_cents": s.earned_income.cents(),
            "additional_child_tax_credit_cents": s.additional_child_tax_credit.cents(),
        }),
    }
}

fn schedule_1a(input: &Form1040Input, computed: &ComputedReturn, params: &TaxParams) -> PacketForm {
    let lines = &computed.lines;
    let agi = line(lines, "11").unwrap_or(Money::ZERO);
    let bonus = schedule_1a_senior_deduction(input.identity.as_ref(), input.status, agi, params);
    if bonus.deduction.cents() <= 0 && bonus.eligible_persons == 0 {
        let reason = match bonus.disqualifier.as_deref() {
            Some("mfs") => {
                "married filing separately: Schedule 1-A Part V bars the enhanced deduction for seniors"
                    .to_string()
            }
            _ => {
                "no eligible senior (a stored identity with a valid SSN and a date of birth before January 2, 1961) in this return"
                    .to_string()
            }
        };
        return PacketForm::not_applicable(
            "f1040s1a",
            "Schedule 1-A",
            "f1040s1a.pdf".to_string(),
            reason,
            json!({"applicable": false, "eligible_persons": 0}),
        );
    }
    let threshold = params.senior_bonus_phaseout_threshold(input.status);
    let excess = Money::from_cents((bonus.magi - threshold).cents().max(0));
    let line34 = excess.mul_bp(params.senior_bonus_phaseout_rate_bp);
    // Lines 36a/36b: the per-person amount for each eligible person,
    // using the same eligibility test as the engine (age 65+ with an
    // SSN on file; spouse counted only on a joint return).
    let eligible = |p: &crate::PersonIdentity| p.is_age_65(params) && p.ssn.is_some();
    let (taxpayer_eligible, spouse_eligible) = match &input.identity {
        Some(id) => (
            eligible(&id.person),
            input.status == FilingStatus::Mfj
                && id.spouse.as_ref().map(|s| eligible(s)).unwrap_or(false),
        ),
        None => (false, false),
    };

    let mut dict = HashMap::new();
    let mut notes = vec![
        "Parts II (no tax on tips), III (no tax on overtime) and IV (no tax on car loan interest) stay blank: those deductions are not modeled in v1 and no input can produce them; Part VI line 38 is therefore the Part V line 37 amount alone".to_string(),
        "Part I lines 2a-2e (excluded Puerto Rico / foreign income) stay blank: none is modeled, so line 3 (MAGI) equals line 1 (AGI)".to_string(),
    ];
    header(&mut dict, &mut notes, input, "name", "ssn");
    amt(&mut dict, "1", agi);
    amt(&mut dict, "3", bonus.magi);
    amt(&mut dict, "31", bonus.magi);
    amt(&mut dict, "32", threshold);
    amt(&mut dict, "33", excess);
    amt(&mut dict, "34", line34);
    amt(&mut dict, "35", bonus.per_person);
    if taxpayer_eligible {
        amt(&mut dict, "36a", bonus.per_person);
    }
    if spouse_eligible {
        amt(&mut dict, "36b", bonus.per_person);
    }
    amt(&mut dict, "37", bonus.deduction);
    amt(&mut dict, "38", bonus.deduction);
    let map = packet_field_map("f1040s1a");
    PacketForm {
        form: "f1040s1a",
        label: "Schedule 1-A",
        file_name: "f1040s1a.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values: resolve_values(&map, &dict),
        notes,
        view: json!({
            "applicable": true,
            "magi_cents": bonus.magi.cents(),
            "eligible_persons": bonus.eligible_persons,
            "per_person_cents": bonus.per_person.cents(),
            "deduction_cents": bonus.deduction.cents(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Packet assembly + the internal schedules view
// ---------------------------------------------------------------------------

/// Build every packet schedule for a computed return, in filing order:
/// Schedule D, Form 8949, Schedule 1, Schedule 2, Schedule C (one per
/// 1099-NEC engagement), Schedule SE, Schedule 8812, Schedule 1-A. Form
/// 1040 itself is produced by the existing `build_fill_values_*` path.
pub fn build_packet(
    input: &Form1040Input,
    computed: &ComputedReturn,
    params: &TaxParams,
) -> Vec<PacketForm> {
    let mut forms = vec![
        schedule_d(input, computed),
        form_8949(input),
        schedule_1(input, computed),
        schedule_2(input, computed),
    ];
    forms.extend(schedules_c(input));
    forms.push(schedule_se(input, computed, params));
    forms.push(schedule_8812_form(input, computed, params));
    forms.push(schedule_1a(input, computed, params));
    forms
}

/// The internal `schedules` view of a computed return: per-form
/// applicability and the internal (non-printed-line) values each
/// schedule carries, in integer cents. Reuses the exact derivations the
/// packet fills from, so the view can never disagree with the packet.
pub fn schedules_view(
    input: &Form1040Input,
    computed: &ComputedReturn,
    params: &TaxParams,
) -> Value {
    let mut view = serde_json::Map::new();
    for form in build_packet(input, computed, params) {
        // Schedule C produces one PacketForm per engagement; fold them
        // into one view entry with an engagements array.
        if form.form == "f1040sc" {
            let entry = view
                .entry("schedule_c")
                .or_insert_with(|| json!({"applicable": false, "engagements": []}));
            if form.applicable {
                entry["applicable"] = json!(true);
                if let Some(list) = entry["engagements"].as_array_mut() {
                    list.push(form.view.clone());
                }
            }
            continue;
        }
        let key = match form.form {
            "f1040sd" => "schedule_d",
            "f8949" => "form_8949",
            "f1040s1" => "schedule_1",
            "f1040s2" => "schedule_2",
            "f1040sse" => "schedule_se",
            "f10408812" => "schedule_8812",
            "f1040s1a" => "schedule_1a",
            other => other,
        };
        view.insert(key.to_string(), form.view);
    }
    Value::Object(view)
}
