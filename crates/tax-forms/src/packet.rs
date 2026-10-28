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

use crate::ca540::{exemption_counts, CaComputed};
use crate::worksheets::{
    deductible_part_of_se_tax, schedule_1a_senior_deduction, schedule_8812, schedule_se_tax,
};
use crate::{
    format_amount, mask_ssn, normalize_ssn, BrokerTxn, ComputedReturn, DependentRelationship,
    F1040FieldMap, FillValue, Form1040Input, NrComputed, TxnTerm,
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
        "f540" => include_str!("../forms/f540-2025-field-map.json"),
        "f1040nr" => include_str!("../forms/f1040nr-2025-field-map.json"),
        "f1040soi" => include_str!("../forms/f1040soi-2025-field-map.json"),
        "f8843" => include_str!("../forms/f8843-2025-field-map.json"),
        other => panic!("no bundled field map for packet form {other}"),
    };
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("bundled {form} field map must parse: {e}"))
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
            "header SSN left blank: no taxpayer SSN is stored in the return's identity".to_string(),
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

    let mut fill_group =
        |txns: &[&BrokerTxn], prefix: &str, box_key: &str| -> (usize, GroupTotals) {
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
        let reason = match bonus.disqualifier {
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
            input.status == FilingStatus::Mfj && id.spouse.as_ref().map(eligible).unwrap_or(false),
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

// ---------------------------------------------------------------------------
// California — Form 540 + Schedule CA (540)
// ---------------------------------------------------------------------------

/// The Schedule CA (540) not-applicable reason for an in-scope
/// California return: the CA engine states the Schedule CA Part I
/// totals (Form 540 lines 14/16) as zero for every v1 return, and
/// Schedule CA is required only when there ARE adjustments.
const SCH_CA_NOT_APPLICABLE_REASON: &str = "no California adjustments in v1 scope: the CA engine states Schedule CA (540), Part I totals (Form 540 lines 14 and 16) as zero for every in-scope return \u{2014} California conforms to the federal treatment of all v1 facts, including the deductible part of self-employment tax (see docs/KNOWN_LIMITS.md) \u{2014} and Schedule CA is required only when there are adjustments, so no Schedule CA is filed with this return";

/// The printed relationship text for the Form 540 Dependents section.
fn ca_relationship_label(relationship: DependentRelationship) -> &'static str {
    match relationship {
        DependentRelationship::Child => "Child",
        DependentRelationship::Stepchild => "Stepchild",
        DependentRelationship::FosterChild => "Foster child",
        DependentRelationship::Other => "Other",
    }
}

/// Build the California Form 540 packet form from the CA engine's
/// stated lines, the stored identity, and the exemption counts (the
/// counts are the one thing the form prints that the engine states
/// only as amounts; they come from the same `exemption_counts`
/// derivation the engine's amounts are built from).
fn form_540(input: &Form1040Input, ca: &CaComputed, params: &TaxParams) -> PacketForm {
    let mut dict: HashMap<String, String> = HashMap::new();
    let mut notes: Vec<String> = vec![
        "county at time of filing (Side 1) left blank: county is not stored on the return"
            .to_string(),
        "principal-residence address fields left blank: not stored; if the mailing address above is not the principal/physical residence at filing time, enter that address by hand".to_string(),
        "line 12 (California state wages) left blank: it is informational on the printed form and not a line the CA engine states (it is the total of W-2 Box 16)".to_string(),
        "lines 40\u{2013}47 (credits), 61, 63, 72\u{2013}74, 77, 91\u{2013}96, 98, 99, and 110\u{2013}115 left blank: not computed in CA v1 (see docs/KNOWN_LIMITS.md); lines 111/114 (amount you owe) add lines 94/96/110 to line 100, so a filer with a balance due starts from the printed line 100".to_string(),
        "signature block, direct-deposit fields, and the voter / health-care / organ-donor elections left blank: bank routing and account numbers are not stored, and the signer signs".to_string(),
        "Form 540 carries no occupation field; stored occupations are not printed on it".to_string(),
    ];

    // Running name/SSN headers (a separate field on every side).
    header(&mut dict, &mut notes, input, "header_name", "header_ssn");

    // Side 1 identity block.
    match input.identity.as_ref() {
        None => notes.push(
            "Form 540 name/address/date-of-birth fields left blank: no taxpayer identity is stored on this return"
                .to_string(),
        ),
        Some(id) => {
            let person = &id.person;
            if let Some(v) = person.first_name.as_deref() {
                text(&mut dict, "taxpayer_first_name", v);
            }
            if let Some(v) = person.last_name.as_deref() {
                text(&mut dict, "taxpayer_last_name", v);
            }
            if let Some(digits) = person.ssn.as_deref().and_then(normalize_ssn) {
                text(&mut dict, "taxpayer_ssn", &digits);
            }
            if let Some(dob) = person.date_of_birth.as_deref() {
                text(&mut dict, "taxpayer_dob", &form_date(dob));
            }
            if person.first_name.is_some() || person.last_name.is_some() {
                notes.push(
                    "middle initial and suffix fields left blank: neither is stored in the return's identity"
                        .to_string(),
                );
            }
            if let Some(address) = id.address.as_ref() {
                if let Some(v) = address.street.as_deref() {
                    text(&mut dict, "address_street", v);
                }
                if let Some(v) = address.apt.as_deref() {
                    text(&mut dict, "address_apt", v);
                }
                if let Some(v) = address.city.as_deref() {
                    text(&mut dict, "address_city", v);
                }
                if let Some(v) = address.state.as_deref() {
                    text(&mut dict, "address_state", v);
                }
                if let Some(v) = address.zip.as_deref() {
                    text(&mut dict, "address_zip", v);
                }
            }
            if let Some(spouse) = id.spouse.as_ref() {
                if let Some(v) = spouse.first_name.as_deref() {
                    text(&mut dict, "spouse_first_name", v);
                }
                if let Some(v) = spouse.last_name.as_deref() {
                    text(&mut dict, "spouse_last_name", v);
                }
                if let Some(digits) = spouse.ssn.as_deref().and_then(normalize_ssn) {
                    text(&mut dict, "spouse_ssn", &digits);
                }
                if let Some(dob) = spouse.date_of_birth.as_deref() {
                    text(&mut dict, "spouse_dob", &form_date(dob));
                }
                if input.status == FilingStatus::Mfs {
                    let full = [
                        spouse.first_name.as_deref(),
                        spouse.last_name.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                    if !full.is_empty() {
                        text(&mut dict, "mfs_spouse_name", &full);
                    }
                }
            }
            if id.can_be_claimed_as_dependent {
                check(&mut dict, "claimed_as_dependent");
            }
        }
    }

    // Filing status: exactly one radio entry resolves, via a
    // per-status dictionary key (see the map's source_line values).
    check(
        &mut dict,
        &format!("filing_status_{}", input.status.as_str()),
    );

    // Exemption counts (boxes) + amounts (engine lines 7-11).
    let (personal, blind, senior, dependents) = exemption_counts(input, params);
    text(&mut dict, "7_count", &personal.to_string());
    text(&mut dict, "8_count", &blind.to_string());
    text(&mut dict, "9_count", &senior.to_string());
    text(&mut dict, "10_count", &dependents.to_string());

    // Dependents section (Side 2): the form has exactly three
    // columns. The stored name splits like the federal export: first
    // word = first name, the remainder = last name.
    if input.dependents.len() > 3 {
        notes.push(format!(
            "{} dependents are stored but the Form 540 Dependents section has only \
             three columns; the first three are printed (exemption credits are still \
             computed for all {})",
            input.dependents.len(),
            input.dependents.len()
        ));
    }
    for (i, dep) in input.dependents.iter().take(3).enumerate() {
        let n = i + 1;
        let mut words = dep.name.split_whitespace();
        let first = words.next().unwrap_or("");
        let last = words.collect::<Vec<_>>().join(" ");
        if !first.is_empty() {
            text(&mut dict, &format!("dep{n}_first_name"), first);
        }
        if !last.is_empty() {
            text(&mut dict, &format!("dep{n}_last_name"), &last);
        }
        if let Some(digits) = dep.ssn.as_deref().and_then(normalize_ssn) {
            text(&mut dict, &format!("dep{n}_ssn"), &digits);
        }
        text(
            &mut dict,
            &format!("dep{n}_relationship"),
            ca_relationship_label(dep.relationship),
        );
    }

    // Engine-stated amount lines. Lines 14/16/34 are stated zeros and
    // are printed so the paper arithmetic chain (15 = 13 - 14,
    // 17 = 15 + 16, 35 = 33 + 34) reads correctly.
    for key in [
        "7", "8", "9", "10", "11", "13", "14", "15", "16", "17", "18", "19", "31", "32", "33",
        "34", "35", "48", "64", "71", "75", "76", "78",
    ] {
        if let Some(v) = line(&ca.lines, key) {
            amt(&mut dict, key, v);
        }
    }
    // Line 62 (Behavioral Health Services Tax): printed only when
    // the tax actually applies (> $1,000,000 of taxable income).
    match line(&ca.lines, "62") {
        Some(v) if v.cents() > 0 => amt(&mut dict, "62", v),
        Some(_) => notes.push(
            "line 62 (Behavioral Health Services Tax) left blank: the CA engine states it as zero (it applies only above $1,000,000 of taxable income)"
                .to_string(),
        ),
        None => {}
    }
    // Lines 97/100 are the form's mutually exclusive overpaid/due
    // pair: each is printed only when greater than zero (the federal
    // export treats lines 34/37 the same way).
    let mut balance_printed = false;
    for key in ["97", "100"] {
        if let Some(v) = line(&ca.lines, key) {
            if v.cents() > 0 {
                amt(&mut dict, key, v);
                balance_printed = true;
            }
        }
    }
    if !balance_printed {
        notes.push(
            "lines 97 and 100 are both zero (total payments exactly equal total tax); neither is printed"
                .to_string(),
        );
    }
    // Line 31's source checkbox, from the engine's method.
    match ca.line31_method.as_deref() {
        Some("ca_tax_table") => check(&mut dict, "31_tax_table"),
        Some("ca_rate_schedule") => check(&mut dict, "31_rate_schedule"),
        _ => {}
    }

    let values = resolve_values(&packet_field_map("f540"), &dict);
    let view_lines: serde_json::Map<String, Value> = ca
        .lines
        .iter()
        .filter_map(|(k, f)| f.value().map(|m| (k.clone(), json!(m.cents()))))
        .collect();
    PacketForm {
        form: "f540",
        label: "Form 540",
        file_name: "f540.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values,
        notes,
        view: json!({
            "applicable": true,
            "lines": view_lines,
            "line31_method": ca.line31_method,
        }),
    }
}

/// Build the California packet forms, in filing order after the
/// federal forms: Form 540, then Schedule CA (540).
///
/// With no state block stored, both forms are `not_applicable` (a
/// federal-only packet gains exactly these two manifest entries).
/// With a California block, Form 540 is filled from the CA engine's
/// computation (`ca` must be `Some` — the caller computes it) and
/// Schedule CA is always `not_applicable` in v1: the engine states
/// its Part I totals (Form 540 lines 14/16) as zero for every
/// in-scope return, and Schedule CA is required only when there ARE
/// adjustments — filling a zero form would misrepresent the filing.
pub fn build_ca_packet(
    input: &Form1040Input,
    ca: Option<&CaComputed>,
    params: &TaxParams,
) -> Vec<PacketForm> {
    match &input.state {
        None => vec![
            PacketForm::not_applicable(
                "f540",
                "Form 540",
                "f540.pdf".to_string(),
                "no state return is selected on this return (tax return set-state); federal-only packet"
                    .to_string(),
                json!({"applicable": false}),
            ),
            PacketForm::not_applicable(
                "f540ca",
                "Schedule CA (540)",
                "f540ca.pdf".to_string(),
                "no California return is selected on this return".to_string(),
                json!({"applicable": false}),
            ),
        ],
        Some(_) => {
            let ca = ca.expect(
                "build_ca_packet: a CA computation is required when a state block is stored",
            );
            vec![
                form_540(input, ca, params),
                PacketForm::not_applicable(
                    "f540ca",
                    "Schedule CA (540)",
                    "f540ca.pdf".to_string(),
                    SCH_CA_NOT_APPLICABLE_REASON.to_string(),
                    json!({"applicable": false}),
                ),
            ]
        }
    }
}

// ---------------------------------------------------------------------------
// Nonresident alien — Form 1040-NR + Schedule OI + Form 8843
// ---------------------------------------------------------------------------

/// The Form 1040-NR amount lines the f1040nr field map consumes, in
/// form order. Lines 34/37 are handled separately (the mutually
/// exclusive overpaid/owed pair, printed only when positive).
const NR_AMOUNT_LINES: &[&str] = &[
    "1a", "1k", "1z", "8", "9", "11a", "11b", "12", "14", "15", "16", "18", "22", "24", "25a",
    "25d", "25g", "32", "33",
];

/// The mapped Form 1040-NR amount lines that are missing or
/// `Incomplete` in `nr` — the single-form PDF export refuses on
/// exactly this set (mirroring the resident export's mapped-lines
/// rule; the packet export is stricter and refuses on any
/// incompleteness).
pub fn nr_mapped_incomplete(nr: &NrComputed) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in NR_AMOUNT_LINES.iter().chain(["34", "37"].iter()) {
        match nr.lines.get(*key) {
            Some(FactResult::Complete(_)) => {}
            _ => out.push(key.to_string()),
        }
    }
    out
}

/// Build the Form 1040-NR packet form from the NR engine's stated
/// lines and the stored identity. Only lines the engine states are
/// printed; everything else the form carries stays blank under a
/// manifest note (see the f1040nr map's derivation for the full list).
fn form_1040nr(input: &Form1040Input, nr: &NrComputed) -> PacketForm {
    let mut dict: HashMap<String, String> = HashMap::new();
    let mut notes: Vec<String> = vec![
        "lines 1b\u{2013}1j, 2\u{2013}7, 10, 13a\u{2013}13c, 17, 19\u{2013}21, 23a\u{2013}23d, 25b/25c/25e/25f, 26, 28\u{2013}31, 35a, 36, and 38 left blank: the Form 1040-NR engine states no amounts for them in v1 (Schedule NEC, credits, and estimated payments are out of scope; see docs/KNOWN_LIMITS.md)"
            .to_string(),
        "foreign address block (page 1) left blank: the stored identity carries one U.S. mailing address (printed in the home-address block) and no foreign address — complete the foreign country / province / postal code fields by hand if the filer has a foreign address to report"
            .to_string(),
        "Digital Assets question (page 1) left unanswered: whether the filer received or disposed of a digital asset is not a stored fact — answer Yes or No by hand"
            .to_string(),
        "signature block, direct-deposit fields, and the refund-to-foreign-address field left blank: bank routing and account numbers are not stored, and the signer signs"
            .to_string(),
    ];

    // Identity header: split name fields (the NR form prints first
    // and last name separately), identifying number, U.S. address,
    // occupation.
    match input.identity.as_ref() {
        None => notes.push(
            "Form 1040-NR name/address/occupation fields left blank: no taxpayer identity is stored on this return"
                .to_string(),
        ),
        Some(id) => {
            let person = &id.person;
            if let Some(v) = person.first_name.as_deref() {
                text(&mut dict, "taxpayer_first_name", v);
            }
            if let Some(v) = person.last_name.as_deref() {
                text(&mut dict, "taxpayer_last_name", v);
            }
            if let Some(digits) = person.ssn.as_deref().and_then(normalize_ssn) {
                text(&mut dict, "taxpayer_ssn", &digits);
            } else {
                notes.push(
                    "identifying number left blank: no taxpayer SSN/ITIN is stored in the return's identity"
                        .to_string(),
                );
            }
            if let Some(v) = person.occupation.as_deref() {
                text(&mut dict, "taxpayer_occupation", v);
            }
            if let Some(address) = id.address.as_ref() {
                if let Some(v) = address.street.as_deref() {
                    text(&mut dict, "address_street", v);
                }
                if let Some(v) = address.apt.as_deref() {
                    text(&mut dict, "address_apt", v);
                }
                if let Some(v) = address.city.as_deref() {
                    text(&mut dict, "address_city", v);
                }
                if let Some(v) = address.state.as_deref() {
                    text(&mut dict, "address_state", v);
                }
                if let Some(v) = address.zip.as_deref() {
                    text(&mut dict, "address_zip", v);
                }
            }
        }
    }

    // Filing status: the NR student route files SINGLE only (the
    // engine refuses any other status).
    check(&mut dict, "filing_status_single");

    for key in NR_AMOUNT_LINES {
        if let Some(v) = line(&nr.lines, key) {
            amt(&mut dict, key, v);
        }
    }
    // Lines 34/37 are the form's mutually exclusive overpaid/owed
    // pair: each prints only when greater than zero (the resident
    // and CA exports treat their pairs the same way).
    let mut balance_printed = false;
    for key in ["34", "37"] {
        if let Some(v) = line(&nr.lines, key) {
            if v.cents() > 0 {
                amt(&mut dict, key, v);
                balance_printed = true;
            }
        }
    }
    if !balance_printed {
        notes.push(
            "lines 34 and 37 are both zero (total payments exactly equal total tax); neither is printed"
                .to_string(),
        );
    }
    if !input.dependents.is_empty() {
        notes.push(format!(
            "{} dependent(s) are stored but the Dependents table is NOT printed on the Form 1040-NR route in v1: the NR engine claims no dependent credits (see the `nra_credits_not_modeled` validation finding)",
            input.dependents.len()
        ));
    }

    let values = resolve_values(&packet_field_map("f1040nr"), &dict);
    let view_lines: serde_json::Map<String, Value> = nr
        .lines
        .iter()
        .filter_map(|(k, f)| f.value().map(|m| (k.clone(), json!(m.cents()))))
        .collect();
    PacketForm {
        form: "f1040nr",
        label: "Form 1040-NR",
        file_name: "f1040nr.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values,
        notes,
        view: json!({
            "applicable": true,
            "lines": view_lines,
            "tax_method": nr.tax_method,
        }),
    }
}

/// Build the Schedule OI (Form 1040-NR) packet form. Applicable when
/// the return carries something Schedule OI exists to report in v1 —
/// a treaty claim (item L rows from the engine) or stored days-of-
/// presence facts (item H). Otherwise `not_applicable`: the remaining
/// items are yes/no and travel-date questions whose answers are not
/// stored facts, and printing an otherwise-empty schedule would
/// misrepresent them as answered.
fn form_1040soi(input: &Form1040Input, nr: &NrComputed) -> PacketForm {
    let block = input
        .nra
        .as_ref()
        .expect("build_nr_packet: an nra block is required");
    let facts = block.form_8843.as_ref();
    let has_days = facts
        .map(|d| {
            d.days_present_current_year.is_some()
                || d.days_present_prior_year_1.is_some()
                || d.days_present_prior_year_2.is_some()
        })
        .unwrap_or(false);
    if nr.schedule_oi.is_empty() && !has_days {
        return PacketForm::not_applicable(
            "f1040soi",
            "Schedule OI (Form 1040-NR)",
            "f1040soi.pdf".to_string(),
            "no treaty claim and no days-of-presence facts are stored on this return: Schedule OI's remaining items are questions whose answers are not stored facts (see docs/KNOWN_LIMITS.md)"
                .to_string(),
            json!({"applicable": false}),
        );
    }

    let mut dict: HashMap<String, String> = HashMap::new();
    let mut notes: Vec<String> = vec![
        "items B (country of tax residence), C, D, F, I, J, K, L2, L3, and M left blank: their answers are not stored facts — complete them by hand (item B is not the same fact as the citizenship in item A)"
            .to_string(),
        "item G (dates entered and left the United States) left blank: travel dates are not stored"
            .to_string(),
    ];
    header(&mut dict, &mut notes, input, "header_name", "header_ssn");
    let citizenship = block
        .normalized_citizenship()
        .unwrap_or_else(|| block.citizenship.clone());
    text(&mut dict, "citizenship", &citizenship);
    text(&mut dict, "visa_type", &block.visa_type);

    // Item H: days of presence (Form 8843 line 4a facts, by year).
    if let Some(d) = facts {
        if let Some(v) = d.days_present_prior_year_2 {
            text(&mut dict, "days_2023", &v.to_string());
        }
        if let Some(v) = d.days_present_prior_year_1 {
            text(&mut dict, "days_2024", &v.to_string());
        }
        if let Some(v) = d.days_present_current_year {
            text(&mut dict, "days_2025", &v.to_string());
        }
        let missing: Vec<&str> = [
            ("2023", d.days_present_prior_year_2),
            ("2024", d.days_present_prior_year_1),
            ("2025", d.days_present_current_year),
        ]
        .into_iter()
        .filter(|(_, v)| v.is_none())
        .map(|(y, _)| y)
        .collect();
        if !missing.is_empty() {
            notes.push(format!(
                "item H days present in {} left blank: not stored in the return's Form 8843 facts — complete by hand (days are never estimated)",
                missing.join(", ")
            ));
        }
    }

    // Item L: one row per engine Schedule OI entry (the form has
    // three rows); column (c) stays blank (prior-year claim months
    // are not stored).
    if nr.schedule_oi.len() > 3 {
        notes.push(format!(
            "{} treaty claims are computed but the Schedule OI item L table has only three rows; the first three are printed and the line 1(e) total still carries all {}",
            nr.schedule_oi.len(),
            nr.schedule_oi.len()
        ));
    }
    for (i, entry) in nr.schedule_oi.iter().take(3).enumerate() {
        let n = i + 1;
        text(&mut dict, &format!("oi_l{n}_country"), &entry.country);
        text(
            &mut dict,
            &format!("oi_l{n}_article"),
            &entry.treaty_article,
        );
        amt(&mut dict, &format!("oi_l{n}_amount"), entry.exempt_amount);
    }
    if !nr.schedule_oi.is_empty() {
        if let Some(v) = line(&nr.lines, "1k") {
            amt(&mut dict, "1k", v);
        }
        notes.push(
            "item L column (c) (number of months claimed in prior tax years) left blank for every row: prior-year claim months are not stored"
                .to_string(),
        );
    }

    let values = resolve_values(&packet_field_map("f1040soi"), &dict);
    PacketForm {
        form: "f1040soi",
        label: "Schedule OI (Form 1040-NR)",
        file_name: "f1040soi.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values,
        notes,
        view: json!({
            "applicable": true,
            "schedule_oi": nr.schedule_oi,
            "days_present": {
                "2023": facts.and_then(|d| d.days_present_prior_year_2),
                "2024": facts.and_then(|d| d.days_present_prior_year_1),
                "2025": facts.and_then(|d| d.days_present_current_year),
            },
        }),
    }
}

/// Join the stored parts of one Form 8843 name/address/phone line
/// (the printed form carries all three in a single field). Absent
/// parts are reported, never invented.
fn join_8843_line(
    dict: &mut HashMap<String, String>,
    notes: &mut Vec<String>,
    key: &str,
    line_label: &str,
    parts: [(&str, &Option<String>); 3],
) {
    let present: Vec<&str> = parts
        .iter()
        .filter_map(|(_, v)| v.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if present.is_empty() {
        notes.push(format!(
            "Form 8843 {line_label} left blank: no facts are stored for it"
        ));
        return;
    }
    let missing: Vec<&str> = parts
        .iter()
        .filter(|(_, v)| v.as_deref().map(str::trim).unwrap_or("").is_empty())
        .map(|(name, _)| *name)
        .collect();
    if !missing.is_empty() {
        notes.push(format!(
            "Form 8843 {line_label} printed without {}: not stored — complete by hand",
            missing.join(" and ")
        ));
    }
    text(dict, key, &present.join(", "));
}

/// Build the Form 8843 packet form from the nra block and its stored
/// Form 8843 facts. The form is always applicable on the NR route
/// (an exempt individual files it with the return); absent facts
/// stay blank under manifest notes.
fn form_8843_packet(input: &Form1040Input) -> PacketForm {
    let block = input
        .nra
        .as_ref()
        .expect("build_nr_packet: an nra block is required");
    let mut dict: HashMap<String, String> = HashMap::new();
    let mut notes: Vec<String> = vec![
        "both address fields left blank BY THE FORM'S OWN INSTRUCTION: Form 8843 says to fill in addresses only when filing the form by itself; in this packet it is attached to the Form 1040-NR, which carries the filer's address"
            .to_string(),
        "line 1a: only the visa type is printed — the filer's exact date of entry into the United States is not stored (only the first calendar year in status), complete the date by hand"
            .to_string(),
        "lines 3a/3b (passport-issuing country and passport number) left blank: identity stores no passport facts"
            .to_string(),
        "Part II (teachers and trainees) left blank: this route is students only (Part III)"
            .to_string(),
        "lines 13/14 (lawful-permanent-resident application) left blank: whether the filer applied for a green card is not a stored fact — answer by hand"
            .to_string(),
        "line 12 is answered No from the validated nra block: the determination refuses filers with more than 5 calendar years in status, so this filer was not exempt for more than 5 calendar years"
            .to_string(),
    ];

    // Header: split names + TIN (the 8843 header prints first and
    // last name separately, like the 1040-NR).
    match input.identity.as_ref() {
        None => notes.push(
            "Form 8843 name/TIN fields left blank: no taxpayer identity is stored on this return"
                .to_string(),
        ),
        Some(id) => {
            if let Some(v) = id.person.first_name.as_deref() {
                text(&mut dict, "taxpayer_first_name", v);
            }
            if let Some(v) = id.person.last_name.as_deref() {
                text(&mut dict, "taxpayer_last_name", v);
            }
            if let Some(digits) = id.person.ssn.as_deref().and_then(normalize_ssn) {
                text(&mut dict, "taxpayer_ssn", &digits);
            }
        }
    }

    let citizenship = block
        .normalized_citizenship()
        .unwrap_or_else(|| block.citizenship.clone());
    text(&mut dict, "visa_1a", &block.visa_type);
    text(&mut dict, "status_1b", &block.visa_type);
    text(&mut dict, "citizenship", &citizenship);

    // Part III, line 11: visa type for each year 2019-2024 the block
    // states the filer held this status (from the first calendar
    // year in status on). Years before that stay blank.
    for year in 2019..=2024u32 {
        if year >= block.first_calendar_year_in_status {
            text(&mut dict, &format!("visa_{year}"), &block.visa_type);
        }
    }
    if block.first_calendar_year_in_status > 2019 {
        notes.push(format!(
            "line 11 visa types before {} left blank: the block's first calendar year in this status is {} — a prior different status (or absence) is not stored",
            block.first_calendar_year_in_status, block.first_calendar_year_in_status
        ));
    }

    // Line 12: No, by construction of the validated block.
    check(&mut dict, "q12_no");

    match block.form_8843.as_ref() {
        None => notes.push(
            "no Form 8843 facts are stored on this return (tax return set-nra with a form_8843 object): lines 4a/4b and Part III lines 9\u{2013}10 are left blank — complete them by hand"
                .to_string(),
        ),
        Some(d) => {
            if let Some(v) = d.days_present_current_year {
                text(&mut dict, "days_2025", &v.to_string());
                // Line 4b: for an exempt individual, every day of
                // presence in the status is excludable for the
                // substantial presence test — the same number.
                text(&mut dict, "days_excluded_2025", &v.to_string());
            }
            if let Some(v) = d.days_present_prior_year_1 {
                text(&mut dict, "days_2024", &v.to_string());
            }
            if let Some(v) = d.days_present_prior_year_2 {
                text(&mut dict, "days_2023", &v.to_string());
            }
            let missing: Vec<&str> = [
                ("2025", d.days_present_current_year),
                ("2024", d.days_present_prior_year_1),
                ("2023", d.days_present_prior_year_2),
            ]
            .into_iter()
            .filter(|(_, v)| v.is_none())
            .map(|(y, _)| y)
            .collect();
            if !missing.is_empty() {
                notes.push(format!(
                    "line 4a days present in {} left blank (and line 4b with 2025): not stored — complete by hand (days are never estimated)",
                    missing.join(", ")
                ));
            }
            join_8843_line(
                &mut dict,
                &mut notes,
                "school_9",
                "line 9 (academic institution)",
                [
                    ("name", &d.school_name),
                    ("address", &d.school_address),
                    ("telephone number", &d.school_phone),
                ],
            );
            join_8843_line(
                &mut dict,
                &mut notes,
                "director_10",
                "line 10 (program director)",
                [
                    ("name", &d.director_name),
                    ("address", &d.director_address),
                    ("telephone number", &d.director_phone),
                ],
            );
        }
    }

    let values = resolve_values(&packet_field_map("f8843"), &dict);
    PacketForm {
        form: "f8843",
        label: "Form 8843",
        file_name: "f8843.pdf".to_string(),
        applicable: true,
        not_applicable_reason: None,
        values,
        notes,
        view: json!({
            "applicable": true,
            "facts_stored": block.form_8843.is_some(),
        }),
    }
}

/// Build the nonresident-alien packet forms, in filing order: Form
/// 1040-NR, Schedule OI (Form 1040-NR), Form 8843. The resident
/// forms and the California forms are never part of an NR packet
/// (the CA selection conflicts with the nra block at compute time).
pub fn build_nr_packet(input: &Form1040Input, nr: &NrComputed) -> Vec<PacketForm> {
    vec![
        form_1040nr(input, nr),
        form_1040soi(input, nr),
        form_8843_packet(input),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compute_1040, DependentInput, DependentRelationship, I1099BInput, I1099NecInput, W2Input,
    };

    fn params() -> TaxParams {
        TaxParams::load_2025().unwrap()
    }

    fn m(dollars: i64) -> Money {
        Money::from_cents(dollars * 100)
    }

    fn base_input() -> Form1040Input {
        Form1040Input {
            f1042ss: Vec::new(),
            nra: None,
            status: FilingStatus::Single,
            w2s: vec![W2Input {
                employer_ein: "12-3456789".into(),
                wages: m(50_000),
                federal_withholding: m(4_000),
                social_security_wages: None,
                social_security_tax: None,
                medicare_wages: None,
                medicare_tax: None,
                state_wages: None,
                state_withholding: None,
            }],
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

    fn txn(description: &str, proceeds: i64, basis: i64, term: TxnTerm) -> BrokerTxn {
        BrokerTxn {
            description: description.into(),
            proceeds: Some(m(proceeds)),
            cost_basis: Some(m(basis)),
            adjustment: Money::ZERO,
            term,
            date_acquired: None,
            date_sold: None,
            adjustment_code: None,
        }
    }

    fn form<'a>(forms: &'a [PacketForm], id: &str) -> &'a PacketForm {
        forms.iter().find(|f| f.form == id).unwrap()
    }

    fn val<'a>(form: &'a PacketForm, line: &str) -> Option<&'a str> {
        form.values
            .iter()
            .find(|v| v.line == line)
            .map(|v| v.value.as_str())
    }

    #[test]
    fn schedule_d_loss_routing_fills_line_21_only() {
        // Net ST loss $6,000, no other capital activity: line 16 is a
        // loss, so lines 17-20 are skipped and line 21 takes the
        // section 1211(b)-limited $3,000 (single).
        let mut input = base_input();
        input.f1099_bs = vec![I1099BInput {
            payer_name: "Broker".into(),
            payer_ein: None,
            transactions: vec![txn("100 sh LOSER", 1_000, 7_000, TxnTerm::Short)],
            federal_withholding: Money::ZERO,
        }];
        let computed = compute_1040(&input, &params());
        let forms = build_packet(&input, &computed, &params());
        let sd = form(&forms, "f1040sd");
        assert!(sd.applicable);
        assert_eq!(val(sd, "7"), Some("-6000.00"));
        assert_eq!(val(sd, "16"), Some("-6000.00"));
        assert_eq!(val(sd, "21"), Some("-3000.00"));
        assert_eq!(val(sd, "17_yes"), None);
        assert_eq!(val(sd, "17_no"), None);
        assert_eq!(val(sd, "20_yes"), None);
        assert_eq!(val(sd, "22_no"), Some("2"), "no qualified dividends");
        // The 8949 page carries the same transaction.
        let f = form(&forms, "f8949");
        assert_eq!(val(f, "st_r1_h"), Some("-6000.00"));
        assert_eq!(val(f, "st_tot_h"), Some("-6000.00"));
    }

    #[test]
    fn form_8949_overflow_is_loud_and_totals_cover_listed_rows() {
        let mut input = base_input();
        let txns: Vec<BrokerTxn> = (0..12)
            .map(|i| txn(&format!("LOT {i}"), 100, 50, TxnTerm::Short))
            .collect();
        input.f1099_bs = vec![I1099BInput {
            payer_name: "Broker".into(),
            payer_ein: None,
            transactions: txns,
            federal_withholding: Money::ZERO,
        }];
        let computed = compute_1040(&input, &params());
        let forms = build_packet(&input, &computed, &params());
        let f = form(&forms, "f8949");
        assert_eq!(f.view["short_term_transactions"], json!(12));
        assert_eq!(f.view["short_term_listed"], json!(11));
        // Page totals cover the 11 listed rows: 11 x $50 gain.
        assert_eq!(val(f, "st_tot_h"), Some("550.00"));
        assert!(
            f.notes.iter().any(|n| n.contains("OVERFLOW")),
            "overflow must be a loud manifest note: {:?}",
            f.notes
        );
        // Schedule D still carries the full engine total: 12 x $50.
        let sd = form(&forms, "f1040sd");
        assert_eq!(val(sd, "1b_h"), Some("600.00"));
    }

    #[test]
    fn two_nec_engagements_make_two_schedule_cs() {
        let mut input = base_input();
        input.f1099_necs = vec![
            I1099NecInput {
                payer_name: "Client A".into(),
                payer_ein: None,
                nonemployee_compensation: m(10_000),
                federal_withholding: Money::ZERO,
                expenses: m(1_000),
                state_withholding: None,
            },
            I1099NecInput {
                payer_name: "Client B".into(),
                payer_ein: Some("98-7654321".into()),
                nonemployee_compensation: m(5_000),
                federal_withholding: Money::ZERO,
                expenses: Money::ZERO,
                state_withholding: None,
            },
        ];
        let computed = compute_1040(&input, &params());
        let forms = build_packet(&input, &computed, &params());
        let cs: Vec<&PacketForm> = forms.iter().filter(|f| f.form == "f1040sc").collect();
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].file_name, "f1040sc.pdf");
        assert_eq!(cs[1].file_name, "f1040sc-2.pdf");
        assert_eq!(val(cs[0], "business_name"), Some("Client A"));
        assert_eq!(val(cs[0], "31"), Some("9000.00"));
        assert_eq!(val(cs[1], "business_name"), Some("Client B"));
        assert_eq!(val(cs[1], "ein"), Some("98-7654321"));
        // Schedule 1 line 3 and Schedule SE line 2 carry the TOTAL net.
        assert_eq!(val(form(&forms, "f1040s1"), "3"), Some("14000.00"));
        assert_eq!(val(form(&forms, "f1040sse"), "2"), Some("14000.00"));
        // The schedules view folds both engagements into one entry.
        let view = schedules_view(&input, &computed, &params());
        assert_eq!(
            view["schedule_c"]["engagements"].as_array().unwrap().len(),
            2
        );
    }

    #[test]
    fn schedule_8812_stop_branch_when_no_credit_remains() {
        // A dependent who classifies for no credit (an adult "other"
        // relative who fails the more-than-half-the-year residency
        // test): tentative credit is zero, so line 12 is answered
        // No and lines 13/14/Part II stay blank.
        let mut input = base_input();
        input.dependents = vec![DependentInput {
            name: "Adult Relative".into(),
            ssn: None,
            relationship: DependentRelationship::Other,
            age: 40,
            months_lived_with_taxpayer: 3,
            full_time_student: false,
            permanently_disabled: false,
        }];
        let computed = compute_1040(&input, &params());
        let forms = build_packet(&input, &computed, &params());
        let s = form(&forms, "f10408812");
        assert!(s.applicable);
        assert_eq!(val(s, "4"), Some("0"));
        assert_eq!(val(s, "8"), Some("0.00"));
        assert_eq!(val(s, "12_no"), Some("1"));
        assert_eq!(val(s, "12_yes"), None);
        assert_eq!(val(s, "13"), None);
        assert_eq!(val(s, "14"), None);
        assert_eq!(val(s, "27"), None);
    }

    #[test]
    fn nec_loss_skips_se_and_schedule_2_but_not_schedule_c() {
        let mut input = base_input();
        input.f1099_necs = vec![I1099NecInput {
            payer_name: "Client Loss".into(),
            payer_ein: None,
            nonemployee_compensation: m(1_000),
            federal_withholding: Money::ZERO,
            expenses: m(3_000),
            state_withholding: None,
        }];
        let computed = compute_1040(&input, &params());
        let forms = build_packet(&input, &computed, &params());
        let cs = form(&forms, "f1040sc");
        assert!(cs.applicable);
        assert_eq!(val(cs, "31"), Some("-2000.00"));
        assert!(!form(&forms, "f1040sse").applicable);
        assert!(!form(&forms, "f1040s2").applicable);
        assert_eq!(val(form(&forms, "f1040s1"), "3"), Some("-2000.00"));
    }

    // -----------------------------------------------------------------
    // California packet forms
    // -----------------------------------------------------------------

    fn ca_input() -> Form1040Input {
        let mut input = base_input();
        input.w2s[0].state_wages = Some(m(50_000));
        input.w2s[0].state_withholding = Some(m(1_500));
        input.state = Some(crate::StateBlock {
            state: "CA".into(),
            residency: "full_year".into(),
        });
        input.identity = Some(crate::TaxpayerIdentity {
            person: crate::PersonIdentity {
                first_name: Some("Case".into()),
                last_name: Some("Tester".into()),
                ssn: Some("123456789".into()),
                date_of_birth: Some("1990-06-15".into()),
                occupation: Some("Engineer".into()),
                blind: false,
            },
            address: Some(crate::MailingAddress {
                street: Some("1 Main St".into()),
                apt: None,
                city: Some("Fremont".into()),
                state: Some("CA".into()),
                zip: Some("94538".into()),
            }),
            spouse: None,
            can_be_claimed_as_dependent: false,
        });
        input
    }

    fn ca_forms(input: &Form1040Input) -> Vec<PacketForm> {
        let fed = compute_1040(input, &params());
        let ca_params = tax_core::CaParams::load_2025().unwrap();
        let ca = crate::compute_ca540(input, &fed, &params(), &ca_params);
        build_ca_packet(input, Some(&ca), &params())
    }

    #[test]
    fn ca_forms_not_applicable_without_state_block() {
        let input = base_input();
        let forms = build_ca_packet(&input, None, &params());
        assert_eq!(forms.len(), 2);
        assert!(!form(&forms, "f540").applicable);
        assert!(!form(&forms, "f540ca").applicable);
    }

    #[test]
    fn ca_540_filled_from_engine_and_identity() {
        let input = ca_input();
        let forms = ca_forms(&input);
        let f540 = form(&forms, "f540");
        assert!(f540.applicable);
        // Golden case ca-001 shape: $50,000 of CA wages, $1,500 withheld.
        assert_eq!(val(f540, "13"), Some("50000.00"));
        assert_eq!(val(f540, "19"), Some("44294.00"));
        assert_eq!(val(f540, "31"), Some("1193.00"));
        assert_eq!(val(f540, "71"), Some("1500.00"));
        assert_eq!(val(f540, "97"), Some("460.00"));
        assert_eq!(val(f540, "100"), None);
        assert_eq!(val(f540, "62"), None); // a stated zero stays blank
        assert_eq!(val(f540, "7_count"), Some("1"));
        assert_eq!(val(f540, "31_tax_table"), Some("Yes"));
        // The filing-status radio carries the form's literal on-state.
        let status = f540
            .values
            .iter()
            .find(|v| v.line == "filing_status")
            .unwrap();
        assert_eq!(status.value, "1 . Single.");
        assert_eq!(status.field, "540_form_1036 RB");
        // Identity: names split, DOB reformatted, SSN full in the fill
        // value but masked in the report value.
        assert_eq!(val(f540, "taxpayer_first_name"), Some("Case"));
        assert_eq!(val(f540, "taxpayer_dob"), Some("06/15/1990"));
        let ssn = f540
            .values
            .iter()
            .find(|v| v.line == "taxpayer_ssn")
            .unwrap();
        assert_eq!(ssn.value, "123456789");
        assert_eq!(ssn.display_value.as_deref(), Some("***-**-6789"));
        // Schedule CA is never filled in v1.
        let sch = form(&forms, "f540ca");
        assert!(!sch.applicable);
        assert!(sch
            .not_applicable_reason
            .as_deref()
            .unwrap()
            .contains("no California adjustments"));
    }

    // -----------------------------------------------------------------
    // Nonresident-alien packet forms
    // -----------------------------------------------------------------

    fn nr002_input() -> Form1040Input {
        // NR-002 shape: CN F-1 (first calendar year), W-2 $3,000 with
        // $500 withheld + treaty-exempt 1042-S code 20 of $5,000.
        let mut input = base_input();
        input.w2s = vec![W2Input {
            employer_ein: "12-3456789".into(),
            wages: m(3_000),
            federal_withholding: m(500),
            social_security_wages: None,
            social_security_tax: None,
            medicare_wages: None,
            medicare_tax: None,
            state_wages: None,
            state_withholding: None,
        }];
        input.f1042ss = vec![crate::F1042sInput {
            recipient_name: None,
            payer_name: "State University".into(),
            income_code: 20,
            gross_income: m(5_000),
            federal_withholding: Money::ZERO,
            exemption_code: Some("04".into()),
            treaty_country: Some("CN".into()),
            treaty_article: Some("Article 20".into()),
        }];
        input.identity = Some(crate::TaxpayerIdentity {
            person: crate::PersonIdentity {
                first_name: Some("Case".into()),
                last_name: Some("Tester".into()),
                ssn: Some("123456789".into()),
                date_of_birth: None,
                occupation: Some("Student".into()),
                blind: false,
            },
            address: Some(crate::MailingAddress {
                street: Some("1 College Ave".into()),
                apt: None,
                city: Some("Berkeley".into()),
                state: Some("CA".into()),
                zip: Some("94720".into()),
            }),
            spouse: None,
            can_be_claimed_as_dependent: false,
        });
        input.nra = Some(crate::NraBlock {
            visa_type: "F-1".into(),
            citizenship: "CN".into(),
            first_calendar_year_in_status: 2025,
            calendar_years_in_status_including_current: 1,
            form_8843: Some(crate::Form8843Data {
                school_name: Some("State University".into()),
                school_address: Some("100 University Dr, Berkeley, CA 94720".into()),
                school_phone: Some("510-555-0100".into()),
                director_name: Some("Dr. Director".into()),
                director_address: Some("200 Program Hall, Berkeley, CA 94720".into()),
                director_phone: Some("510-555-0101".into()),
                days_present_current_year: Some(200),
                days_present_prior_year_1: None,
                days_present_prior_year_2: None,
            }),
        });
        input
    }

    fn nr_forms(input: &Form1040Input) -> Vec<PacketForm> {
        let treaties = tax_core::TreatyParams::load_2025().unwrap();
        let nr = crate::compute_1040nr(input, &params(), &treaties);
        assert!(nr.complete, "findings: {:?}", nr.findings);
        build_nr_packet(input, &nr)
    }

    #[test]
    fn nr_packet_fills_1040nr_oi_and_8843() {
        let input = nr002_input();
        let forms = nr_forms(&input);
        let main = form(&forms, "f1040nr");
        assert!(main.applicable);
        assert_eq!(val(main, "1a"), Some("3000.00"));
        assert_eq!(val(main, "1k"), Some("5000.00"));
        assert_eq!(val(main, "16"), Some("303.00"));
        assert_eq!(val(main, "25a"), Some("500.00"));
        assert_eq!(val(main, "25g"), Some("0.00"));
        assert_eq!(val(main, "34"), Some("197.00"));
        assert_eq!(val(main, "37"), None);
        assert_eq!(val(main, "taxpayer_first_name"), Some("Case"));
        assert_eq!(val(main, "taxpayer_occupation"), Some("Student"));
        let status = main
            .values
            .iter()
            .find(|v| v.line == "filing_status")
            .unwrap();
        assert_eq!(status.value, "1");
        let ssn = main
            .values
            .iter()
            .find(|v| v.line == "taxpayer_ssn")
            .unwrap();
        assert_eq!(ssn.value, "123456789");
        assert_eq!(ssn.display_value.as_deref(), Some("***-**-6789"));

        let oi = form(&forms, "f1040soi");
        assert!(oi.applicable);
        assert_eq!(val(oi, "citizenship"), Some("CN"));
        assert_eq!(val(oi, "visa_type"), Some("F-1"));
        assert_eq!(val(oi, "days_2025"), Some("200"));
        assert_eq!(val(oi, "days_2024"), None);
        assert_eq!(val(oi, "oi_l1_country"), Some("CN"));
        assert_eq!(val(oi, "oi_l1_article"), Some("20(c)"));
        assert_eq!(val(oi, "oi_l1_amount"), Some("5000.00"));
        assert_eq!(val(oi, "1k"), Some("5000.00"));

        let s = form(&forms, "f8843");
        assert!(s.applicable);
        assert_eq!(val(s, "visa_1a"), Some("F-1"));
        assert_eq!(val(s, "citizenship"), Some("CN"));
        assert_eq!(val(s, "days_2025"), Some("200"));
        assert_eq!(val(s, "days_excluded_2025"), Some("200"));
        assert_eq!(
            val(s, "school_9"),
            Some("State University, 100 University Dr, Berkeley, CA 94720, 510-555-0100")
        );
        assert_eq!(
            val(s, "director_10"),
            Some("Dr. Director, 200 Program Hall, Berkeley, CA 94720, 510-555-0101")
        );
        // First year in status is 2025: no line-11 years (2019-2024)
        // are filled.
        assert_eq!(val(s, "visa_2024"), None);
        let q12 = s.values.iter().find(|v| v.line == "q12_no").unwrap();
        assert_eq!(q12.value, "2");
        // Addresses stay blank by the form's own instruction (no
        // address_* values at all).
        assert!(s.values.iter().all(|v| !v.line.starts_with("address")));
    }

    #[test]
    fn nr_schedule_oi_not_applicable_without_treaty_or_days() {
        // DE citizen, no treaty modeled, no 8843 facts: Schedule OI
        // has nothing v1 can state.
        let mut input = nr002_input();
        input.f1042ss = vec![];
        input.w2s[0].wages = m(8_000);
        input.w2s[0].federal_withholding = m(300);
        if let Some(block) = input.nra.as_mut() {
            block.citizenship = "DE".into();
            block.form_8843 = None;
        }
        let forms = nr_forms(&input);
        let oi = form(&forms, "f1040soi");
        assert!(!oi.applicable);
        assert!(oi
            .not_applicable_reason
            .as_deref()
            .unwrap()
            .contains("no treaty claim"));
        // The 8843 is still emitted, with hand-complete notes.
        let s = form(&forms, "f8843");
        assert!(s.applicable);
        assert!(s.notes.iter().any(|n| n.contains("no Form 8843 facts")));
    }
}
