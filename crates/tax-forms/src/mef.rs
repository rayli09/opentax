//! MeF (Modernized e-File) XML export: the return as an IRS-shaped XML
//! document — `Return` / `ReturnHeader` / `ReturnData` wrapping one
//! element per form (`IRS1040`, `IRS1040Schedule1`, …), in the
//! `http://www.irs.gov/efile` namespace.
//!
//! # Sources (every emitted element traces to one of these)
//!
//! - **Per-form element names, order, and types**: the IRS TY2025 1040
//!   MeF schema package, version **2025v5.2**, MaturityLevel "Final
//!   Release", ReleaseDate February 19, 2026 (each XSD's own annotation;
//!   the IRS posts TY2025 v5.2 in its schema version history with that
//!   date). The package is distributed only through the IRS Secure
//!   Object Repository (SOR) to registered software developers, so the
//!   copies used here come from a public mirror of the package and are
//!   vendored, with provenance, under `scripts/mef-schemas/2025v5.2/`.
//!   The XSDs annotate most amount elements with their paper line
//!   number; the mapping below follows those annotations.
//! - **Shared types** (`USAmountType` = whole-dollar integer,
//!   `SSNType` = 9 digits, name/address shapes): `efileTypes.xsd`.
//!   The vendored copy is the authentic TY2015 (2015v3.0) Common file —
//!   the TY2025 Common file is SOR-gated as well; see
//!   `scripts/mef-schemas/README.md` for the disclosed substitution.
//! - **Envelope**: IRS Publication 4164 (Rev. 12-2025), §2.10.1 — the
//!   1040 package is `Return1040.xsd` wrapping `ReturnHeader1040x` +
//!   `ReturnData1040`, return type "1040". The envelope XSDs are not
//!   publicly available at all, so the header's element vocabulary
//!   follows Code for America's production vita-min transmitter
//!   (`submission_builder/return_header1040.rb`), which passes IRS
//!   Acceptance Testing with this structure.
//!
//! # Honesty tiers (see also docs/KNOWN_LIMITS.md)
//!
//! - **Form documents: Tier A.** Built to the official TY2025v5.2
//!   per-form XSDs and validated against them by
//!   `scripts/mef_acceptance.py` (the Rust binary itself performs no
//!   XSD validation, and the export report says so:
//!   `validation.xsd_validated` is `false` for the export itself).
//! - **Envelope: Tier B.** Structured per Pub 4164 + vita-min, NOT
//!   XSD-validated (no public schema exists to validate against).
//! - **Not transmissible.** The header omits the originator (EFIN),
//!   software ID, PIN/signature, and filing-security groups: this tool
//!   holds no IRS e-file credentials, so those transmission-time
//!   elements cannot be populated honestly. Real filing still requires
//!   an authorized transmitter and ATS. Nothing here claims otherwise.
//!
//! # Emission policy
//!
//! - Amounts are **whole dollars** (the MeF amount types are integer
//!   dollars): each amount is rounded from the engine's cents value,
//!   half away from zero, per field, following the IRS rounding
//!   convention. Every field whose cents value was not already whole
//!   is listed in the export's validation notes, and any total that no
//!   longer cross-foots against its rounded components is noted too.
//!   The JSON/PDF exports keep cents and remain the precise artifacts.
//! - An amount element is emitted when the engine computed the
//!   underlying line and either the rounded value is non-zero or the
//!   element is a structural total (Form 1040 lines 9, 11, 12, 14, 15,
//!   16, 18, 22, 24, 33 and the analogous schedule totals). Zero-valued
//!   detail lines stay absent, as they are blank on paper. Checkbox
//!   indicators (`CheckboxType`) are emitted only when true, as `X`;
//!   `BooleanType` indicators are emitted as `true`/`false` when the
//!   underlying fact is computed.
//! - Which schedules appear mirrors the filing packet's applicability
//!   rules exactly (see [`crate::packet::build_packet`]), and schedule
//!   values are recomputed from the same worksheet primitives the
//!   packet uses, so the XML cannot disagree with the filled PDFs.
//! - **Identity is required** (unlike the PDF export, which tolerates
//!   its absence and reports the identity fields skipped): a
//!   `ReturnHeader` cannot be honestly populated without the taxpayer's
//!   name and SSN. The full SSN appears in the XML — it is the filing
//!   artifact — and never in the CLI's stdout report.
//! - `IRSW2` documents are NOT emitted: the return stores each W-2's
//!   EIN and amounts, but not the employer's name, name control, or
//!   address, which the IRSW2 schema requires. Form 1040 lines 1a/25a
//!   still carry the W-2 totals.

use tax_core::{FactResult, FilingStatus, Money, TaxParams};

use crate::worksheets::{
    classify_dependent, classify_eic_child, deductible_part_of_se_tax,
    schedule_1a_senior_deduction, schedule_8812, schedule_se_tax, DependentCreditClass,
};
use crate::{
    normalize_ssn, ComputedReturn, DependentInput, DependentRelationship, Form1040Input,
    PersonIdentity, TaxpayerIdentity, TxnTerm,
};

/// The MeF schema package version this export targets: the IRS TY2025
/// 1040 package, v5.2 (Final Release, February 19, 2026). Emitted as
/// the `returnVersion` attribute and reported as `schema_version`.
pub const MEF_SCHEMA_VERSION: &str = "2025v5.2";

/// A finished MeF export: the XML document plus the facts the CLI
/// reports (document inventory, rounding record, honesty notes). The
/// XML contains full SSNs; nothing in this struct besides `xml` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MefExport {
    /// The complete XML document (UTF-8, `http://www.irs.gov/efile`
    /// as the default namespace).
    pub xml: String,
    /// Form element names in `ReturnData` order (`IRS1040` first).
    pub documents: Vec<String>,
    /// `Form/Element` paths of amount fields whose cents value was not
    /// whole dollars and was rounded at serialization.
    pub rounded_fields: Vec<String>,
    /// Honesty notes for the export report's `validation.notes`:
    /// validation tier and scope, header omissions, lossy mappings,
    /// cross-foot breaks from rounding. No PII (names/SSNs) in here.
    pub notes: Vec<String>,
}

/// Why a MeF export cannot be produced. `code` is the CLI error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MefExportError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for MefExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MefExportError {}

fn err(code: &'static str, message: impl Into<String>) -> MefExportError {
    MefExportError {
        code,
        message: message.into(),
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// XML-escape a text node (`&`, `<`, `>`).
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Round a cents amount to whole dollars, half away from zero (the
/// IRS per-field rounding convention for MeF's integer-dollar amount
/// types): $2,543.32 → 2543, $1,271.66 → 1272, −$1.50 → −2.
pub fn whole_dollars(amount: Money) -> i64 {
    let cents = amount.cents();
    let sign = cents.signum();
    (cents.abs() + 50) / 100 * sign
}

/// IRS name control for an individual: the first four letters of the
/// last name, uppercased (fewer when the name is shorter).
fn name_control(last_name: &str) -> String {
    last_name
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .take(4)
        .collect::<String>()
        .to_uppercase()
}

/// A person's full name as one string, from the stored parts.
fn full_name(person: &PersonIdentity) -> Option<String> {
    let first = person.first_name.as_deref().map(str::trim)?;
    let last = person.last_name.as_deref().map(str::trim)?;
    if first.is_empty() || last.is_empty() {
        return None;
    }
    Some(format!("{first} {last}"))
}

/// Split a dependent's single stored name into (first, last): first
/// token / last token. A single-token name yields (token, token) —
/// the caller notes the heuristic when it fires.
fn split_name(name: &str) -> (String, String) {
    let tokens: Vec<&str> = name.split_whitespace().collect();
    match tokens.as_slice() {
        [] => (String::new(), String::new()),
        [only] => (only.to_string(), only.to_string()),
        [first, .., last] => (first.to_string(), last.to_string()),
    }
}

/// The `DependentRelationshipCd` value for the Form 1040
/// `DependentDetail` (TY2025 enumeration: SON, DAUGHTER, STEPCHILD,
/// FOSTER CHILD, BROTHER, …, OTHER, NONE). The return records
/// relationship without gender, so a plain child — whose specific
/// code would be SON or DAUGHTER — maps to OTHER; the caller notes
/// this lossy mapping whenever it fires. STEPCHILD and FOSTER CHILD
/// are gender-neutral in the enumeration and map exactly.
fn relationship_cd_1040(rel: DependentRelationship) -> &'static str {
    match rel {
        DependentRelationship::Child => "OTHER",
        DependentRelationship::Stepchild => "STEPCHILD",
        DependentRelationship::FosterChild => "FOSTER CHILD",
        DependentRelationship::Other => "OTHER",
    }
}

/// The Schedule EIC `ChildRelationshipCd` value, when one exists: the
/// TY2025 enumeration for that form has NO generic OTHER value, so a
/// plain child (SON/DAUGHTER undistinguished in the return) cannot be
/// represented and yields `None` — the caller then omits Schedule EIC
/// rather than inventing a value.
fn relationship_cd_eic(rel: DependentRelationship) -> Option<&'static str> {
    match rel {
        DependentRelationship::Stepchild => Some("STEPCHILD"),
        DependentRelationship::FosterChild => Some("FOSTER CHILD"),
        DependentRelationship::Child | DependentRelationship::Other => None,
    }
}

/// Accumulates one form document's XML and the rounding record.
struct Emitter {
    /// Current form element name, for rounding paths.
    doc: &'static str,
    rounded: Vec<String>,
}

impl Emitter {
    fn new(doc: &'static str) -> Emitter {
        Emitter {
            doc,
            rounded: Vec::new(),
        }
    }

    /// Serialize one amount element at the given indent, recording
    /// the field in the rounding list when its cents value was not
    /// already whole dollars. Returns the rounded dollar value.
    fn amt(&mut self, out: &mut String, indent: usize, elem: &str, value: Money) -> i64 {
        let dollars = whole_dollars(value);
        if value.cents() % 100 != 0 {
            self.rounded.push(format!("{}/{}", self.doc, elem));
        }
        self.raw(out, indent, elem, &dollars.to_string());
        dollars
    }

    /// Like [`Emitter::amt`], but the element is omitted entirely
    /// when the value is zero (a paper-blank detail line).
    fn amt_nz(&mut self, out: &mut String, indent: usize, elem: &str, value: Money) {
        if value.cents() != 0 {
            self.amt(out, indent, elem, value);
        }
    }

    /// A plain text element (already-escaped here).
    fn text(&self, out: &mut String, indent: usize, elem: &str, value: &str) {
        self.raw(out, indent, elem, &esc(value));
    }

    /// A checked checkbox (`CheckboxType` is the single value `X`).
    fn checkbox(&self, out: &mut String, indent: usize, elem: &str) {
        self.raw(out, indent, elem, "X");
    }

    /// A `BooleanType` indicator (`true` / `false`).
    fn boolean(&self, out: &mut String, indent: usize, elem: &str, value: bool) {
        self.raw(out, indent, elem, if value { "true" } else { "false" });
    }

    fn raw(&self, out: &mut String, indent: usize, elem: &str, content: &str) {
        for _ in 0..indent {
            out.push_str("  ");
        }
        out.push('<');
        out.push_str(elem);
        out.push('>');
        out.push_str(content);
        out.push_str("</");
        out.push_str(elem);
        out.push_str(">\n");
    }

    fn open(&self, out: &mut String, indent: usize, elem: &str) {
        for _ in 0..indent {
            out.push_str("  ");
        }
        out.push('<');
        out.push_str(elem);
        out.push_str(">\n");
    }

    fn close(&self, out: &mut String, indent: usize, elem: &str) {
        for _ in 0..indent {
            out.push_str("  ");
        }
        out.push_str("</");
        out.push_str(elem);
        out.push_str(">\n");
    }
}

/// A computed line's value, or zero when the key is absent. Callers
/// gate on `computed.complete`, so an absent key here means the
/// engine does not produce that line for this return at all.
fn fact(lines: &std::collections::BTreeMap<String, FactResult<Money>>, key: &str) -> Money {
    lines
        .get(key)
        .and_then(|f| f.value().copied())
        .unwrap_or(Money::ZERO)
}

/// Cross-foot check on rounded values: when a total no longer equals
/// the sum of its rounded components (an expected artifact of
/// per-field rounding), record exactly that.
fn cross_foot(notes: &mut Vec<String>, label: &str, total: i64, components_sum: i64) {
    if total != components_sum {
        notes.push(format!(
            "whole-dollar rounding: {label} is {total} but its rounded components sum to \
             {components_sum} (the IRS rounds each field independently; the cents-exact \
             values cross-foot in the JSON/PDF exports)"
        ));
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Build the MeF XML document for a computed return.
///
/// `generated_at` is the `ReturnTs` timestamp (ISO 8601, supplied by
/// the caller so this function stays deterministic and testable).
///
/// Errors: `unsupported_tax_year` (the engine and the schema package
/// are TY2025), `return_incomplete` (a computed line the export needs
/// is incomplete — the filing artifact must be whole), and
/// `identity_required` (no identity, or the identity lacks the
/// taxpayer's name or SSN — the ReturnHeader cannot be honestly
/// populated; see the module docs for why MeF is stricter here than
/// the PDF export).
pub fn build_mef(
    input: &Form1040Input,
    computed: &ComputedReturn,
    params: &TaxParams,
    tax_year: u32,
    generated_at: &str,
) -> Result<MefExport, MefExportError> {
    if tax_year != 2025 {
        return Err(err(
            "unsupported_tax_year",
            format!(
                "MeF export targets the TY2025 schema package ({MEF_SCHEMA_VERSION}); \
                 got tax year {tax_year}"
            ),
        ));
    }
    if !computed.complete {
        return Err(err(
            "return_incomplete",
            "the return has incomplete lines; a MeF document must be complete — \
             resolve the missing inputs first",
        ));
    }
    let identity = input.identity.as_ref().ok_or_else(|| {
        err(
            "identity_required",
            "identity required for MeF export: no taxpayer identity is stored on this \
             return (set it with `tax return set-identity`). The ReturnHeader's Filer \
             block needs the taxpayer's name and SSN; unlike the PDF export, which \
             tolerates a missing identity and reports those fields skipped, a MeF \
             document without them would be a fabrication.",
        )
    })?;
    let primary_ssn = identity
        .person
        .ssn
        .as_deref()
        .and_then(normalize_ssn)
        .ok_or_else(|| {
            err(
                "identity_required",
                "identity required for MeF export: the stored identity has no valid \
                 taxpayer SSN for the ReturnHeader Filer block",
            )
        })?;
    let primary_name = full_name(&identity.person).ok_or_else(|| {
        err(
            "identity_required",
            "identity required for MeF export: the stored identity has no taxpayer \
             first/last name for the ReturnHeader Filer block",
        )
    })?;

    let mut notes: Vec<String> = vec![
        format!(
            "form documents (IRS1040 and its schedules) are built to the official IRS \
             TY2025 MeF schema package, version {MEF_SCHEMA_VERSION} (Final Release, \
             February 19, 2026): element names, order, and types follow the published \
             per-form XSDs, vendored with provenance under scripts/mef-schemas/. The \
             `tax` binary itself does not run XSD validation (xsd_validated is false \
             for this export); scripts/mef_acceptance.py validates the emitted form \
             documents against those XSDs, resolving the shared Common types from the \
             authentic TY2015 efileTypes.xsd — the TY2025 Common file is distributed \
             only through the IRS Secure Object Repository — with harness-supplied \
             definitions for three post-2015 leaf types (MilesNNType, \
             SpecialConditionTextType, Text2Type) that gate no emitted element"
        ),
        "the Return/ReturnHeader envelope is NOT XSD-validated: the TY2025 envelope \
         schemas (Return1040.xsd, ReturnHeader1040x.xsd) are likewise distributed \
         only through the IRS Secure Object Repository. The envelope structure \
         follows IRS Publication 4164 (Rev. 12-2025), section 2.10.1, and the \
         header element vocabulary of Code for America's production vita-min \
         transmitter"
            .to_string(),
        "the ReturnHeader omits the OriginatorGrp (EFIN), SoftwareId, PIN/signature, \
         and filing-security groups: this tool holds no IRS e-file credentials, so \
         those transmission-time elements cannot be populated. This XML CANNOT be \
         transmitted to the IRS as-is; filing still requires an authorized e-file \
         provider/EFIN holder and IRS Acceptance Testing (ATS)"
            .to_string(),
        "MeF amounts are whole dollars: each amount was rounded from the engine's \
         cents value, half away from zero, per field, per the IRS rounding \
         convention. The JSON and PDF exports keep cents and are the precise \
         artifacts"
            .to_string(),
        "IRS1040/VirtualCurAcquiredDurTYInd (the digital-assets question) is required \
         by the schema on every Form 1040. The return records no answer to it and no \
         digital-asset transaction exists anywhere in the return's modeled scope, so \
         it is emitted as false — confirm before any real filing"
            .to_string(),
    ];
    let mut rounded: Vec<String> = Vec::new();

    // Form documents, in filing order (attachment sequence): the 1040
    // first, then its schedules. documentIds are assigned in this
    // order, starting at 1.
    let mut documents: Vec<(String, String)> = Vec::new();

    let mut em = Emitter::new("IRS1040");
    let mut body = String::new();
    emit_irs1040(
        input, computed, params, identity, &mut em, &mut body, &mut notes,
    );
    rounded.append(&mut em.rounded);
    documents.push(("IRS1040".to_string(), body));

    if !input.f1099_necs.is_empty() {
        let mut em = Emitter::new("IRS1040Schedule1");
        let mut body = String::new();
        emit_schedule_1(computed, &mut em, &mut body);
        rounded.append(&mut em.rounded);
        documents.push(("IRS1040Schedule1".to_string(), body));
    }

    let se_tax = fact(&computed.lines, "sched2_se_tax");
    if se_tax.cents() > 0 {
        let mut em = Emitter::new("IRS1040Schedule2");
        let mut body = String::new();
        emit_schedule_2(computed, &mut em, &mut body);
        rounded.append(&mut em.rounded);
        documents.push(("IRS1040Schedule2".to_string(), body));
    }

    for nec in &input.f1099_necs {
        let mut em = Emitter::new("IRS1040ScheduleC");
        let mut body = String::new();
        emit_schedule_c(nec, &primary_name, &primary_ssn, &mut em, &mut body);
        rounded.append(&mut em.rounded);
        documents.push(("IRS1040ScheduleC".to_string(), body));
    }
    if !input.f1099_necs.is_empty() {
        notes.push(
            "Schedule C: the schema requires one accounting-method indicator and the \
             return does not record an accounting method; the engine's Schedule C \
             model (gross receipts minus a single expense total, no inventory or \
             receivables) is a cash-method model, so MethodOfAccountingCashInd is \
             emitted. The PDF packet leaves the accounting-method box blank for the \
             same reason — confirm the method before any real filing"
                .to_string(),
        );
    }

    if !input.f1099_bs.is_empty() {
        let mut em = Emitter::new("IRS1040ScheduleD");
        let mut body = String::new();
        emit_schedule_d(input, computed, &mut em, &mut body, &mut notes);
        rounded.append(&mut em.rounded);
        documents.push(("IRS1040ScheduleD".to_string(), body));

        let mut em = Emitter::new("IRS8949");
        let mut body = String::new();
        emit_8949(input, &mut em, &mut body);
        rounded.append(&mut em.rounded);
        documents.push(("IRS8949".to_string(), body));
    }

    let schc_net = fact(&computed.lines, "schc_net_profit");
    if !input.f1099_necs.is_empty() && schc_net.cents() > 0 {
        let mut em = Emitter::new("IRS1040ScheduleSE");
        let mut body = String::new();
        emit_schedule_se(
            input, computed, params, identity, &mut em, &mut body, &mut notes,
        );
        rounded.append(&mut em.rounded);
        documents.push(("IRS1040ScheduleSE".to_string(), body));
        notes.push(
            "Schedule SE line 8a takes the engine's proxy — total W-2 Box 1 wages — \
             for Box 3 Social Security wages, which the return does not store \
             separately (the same proxy the return validator reports)"
                .to_string(),
        );
    }

    if !input.dependents.is_empty() {
        let mut em = Emitter::new("IRS1040Schedule8812");
        let mut body = String::new();
        emit_schedule_8812(input, computed, params, &mut em, &mut body);
        rounded.append(&mut em.rounded);
        documents.push(("IRS1040Schedule8812".to_string(), body));
    }

    emit_schedule_eic_check(
        input,
        computed,
        tax_year,
        &mut documents,
        &mut rounded,
        &mut notes,
    );

    let agi = fact(&computed.lines, "11");
    let bonus = schedule_1a_senior_deduction(input.identity.as_ref(), input.status, agi, params);
    if bonus.deduction.cents() > 0 || bonus.eligible_persons > 0 {
        let mut em = Emitter::new("IRS1040Schedule1A");
        let mut body = String::new();
        emit_schedule_1a(input, &bonus, params, agi, &mut em, &mut body);
        rounded.append(&mut em.rounded);
        documents.push(("IRS1040Schedule1A".to_string(), body));
    }

    if !input.w2s.is_empty() {
        notes.push(
            "IRSW2 documents are not emitted: the return stores each W-2's EIN and \
             amounts, but not the employer's name, name control, or address, which \
             the IRSW2 schema requires. Form 1040 lines 1a/1z and 25a carry the W-2 \
             totals"
                .to_string(),
        );
    }

    if !rounded.is_empty() {
        notes.push(format!(
            "fields rounded from cents to whole dollars ({}): {}",
            rounded.len(),
            rounded.join(", ")
        ));
    }

    // Envelope.
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<Return xmlns=\"http://www.irs.gov/efile\" returnVersion=\"{MEF_SCHEMA_VERSION}\">\n"
    ));
    emit_header(
        identity,
        input.status,
        tax_year,
        generated_at,
        &mut xml,
        &mut notes,
    );
    xml.push_str(&format!(
        "  <ReturnData documentCnt=\"{}\">\n",
        documents.len()
    ));
    let mut document_names = Vec::new();
    for (idx, (name, body)) in documents.iter().enumerate() {
        document_names.push(name.clone());
        xml.push_str(&format!("    <{name} documentId=\"{}\">\n", idx + 1));
        xml.push_str(body);
        xml.push_str(&format!("    </{name}>\n"));
    }
    xml.push_str("  </ReturnData>\n</Return>\n");

    Ok(MefExport {
        xml,
        documents: document_names,
        rounded_fields: rounded,
        notes,
    })
}

/// Schedule EIC appears only when the EIC was actually claimed with
/// qualifying children AND every qualifying child can be represented
/// in the form's relationship enumeration (see
/// [`relationship_cd_eic`]); otherwise the omission is noted, never
/// papered over with an invented code.
fn emit_schedule_eic_check(
    input: &Form1040Input,
    computed: &ComputedReturn,
    tax_year: u32,
    documents: &mut Vec<(String, String)>,
    rounded: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let eic = fact(&computed.lines, "27");
    if eic.cents() <= 0 {
        return;
    }
    let qualifying: Vec<&DependentInput> = input
        .dependents
        .iter()
        .filter(|d| classify_eic_child(d))
        .collect();
    if qualifying.is_empty() {
        // Childless EIC: no Schedule EIC exists for it. Not a gap.
        return;
    }
    if qualifying.iter().any(|d| d.ssn.is_none()) {
        notes.push(
            "Schedule EIC is not emitted: an EIC qualifying child has no SSN stored, \
             and the form requires one"
                .to_string(),
        );
        return;
    }
    if qualifying
        .iter()
        .any(|d| relationship_cd_eic(d.relationship).is_none())
    {
        notes.push(
            "Schedule EIC is not emitted: the form's ChildRelationshipCd enumeration \
             (SON, DAUGHTER, STEPCHILD, FOSTER CHILD, …) has no value for a child \
             whose son/daughter distinction the return does not record, and no code \
             is invented here. The EIC itself is on Form 1040 line 27 \
             (EarnedIncomeCreditAmt)"
                .to_string(),
        );
        return;
    }
    let mut em = Emitter::new("IRS1040ScheduleEIC");
    let mut body = String::new();
    if qualifying.len() > 3 {
        notes.push(format!(
            "Schedule EIC lists at most 3 qualifying children (the form's limit); \
             this return has {} EIC qualifying children, so the first 3 are listed",
            qualifying.len()
        ));
    }
    for dep in qualifying.iter().take(3) {
        emit_eic_child(dep, tax_year, &mut em, &mut body, notes);
    }
    rounded.append(&mut em.rounded);
    documents.push(("IRS1040ScheduleEIC".to_string(), body));
}

fn emit_eic_child(
    dep: &DependentInput,
    tax_year: u32,
    em: &mut Emitter,
    out: &mut String,
    notes: &mut Vec<String>,
) {
    let (first, last) = split_name(&dep.name);
    if !dep.name.contains(char::is_whitespace) {
        notes.push(
            "a dependent's stored name has a single token; it is used as both the \
             first and last name on Schedule EIC (the return stores one name field \
             per dependent)"
                .to_string(),
        );
    }
    let ssn = dep
        .ssn
        .as_deref()
        .and_then(normalize_ssn)
        .unwrap_or_default();
    em.open(out, 3, "QualifyingChildInformation");
    em.text(
        out,
        4,
        "QualifyingChildNameControlTxt",
        &name_control(&last),
    );
    em.open(out, 4, "ChildFirstAndLastName");
    em.text(out, 5, "PersonFirstNm", &first);
    em.text(out, 5, "PersonLastNm", &last);
    em.close(out, 4, "ChildFirstAndLastName");
    em.text(out, 4, "QualifyingChildSSN", &ssn);
    // Age is stored as of December 31 of the tax year, so the birth
    // year is exact: tax_year - age.
    em.text(out, 4, "ChildBirthYr", &(tax_year - dep.age).to_string());
    let student_under_24 = dep.full_time_student && dep.age < 24;
    em.boolean(out, 4, "ChildIsAStudentUnder24Ind", student_under_24);
    if !student_under_24 {
        em.boolean(
            out,
            4,
            "ChildPermanentlyDisabledInd",
            dep.permanently_disabled,
        );
    }
    if let Some(cd) = relationship_cd_eic(dep.relationship) {
        em.text(out, 4, "ChildRelationshipCd", cd);
    }
    em.text(
        out,
        4,
        "MonthsChildLivedWithYouCnt",
        &format!("{:02}", dep.months_lived_with_taxpayer),
    );
    em.close(out, 3, "QualifyingChildInformation");
}

// ---------------------------------------------------------------------------
// ReturnHeader (Tier B envelope — see module docs)
// ---------------------------------------------------------------------------

fn emit_header(
    identity: &TaxpayerIdentity,
    status: FilingStatus,
    tax_year: u32,
    generated_at: &str,
    xml: &mut String,
    notes: &mut Vec<String>,
) {
    // Presence of the name/SSN is gated in build_mef; re-derive here.
    let primary_ssn = identity
        .person
        .ssn
        .as_deref()
        .and_then(normalize_ssn)
        .unwrap_or_default();
    let primary_name = full_name(&identity.person).unwrap_or_default();
    xml.push_str("  <ReturnHeader binaryAttachmentCnt=\"0\">\n");
    let mut h = String::new();
    let e = Emitter::new("ReturnHeader");
    e.text(&mut h, 2, "ReturnTs", generated_at);
    e.text(&mut h, 2, "TaxYr", &tax_year.to_string());
    e.text(&mut h, 2, "TaxPeriodBeginDt", &format!("{tax_year}-01-01"));
    e.text(&mut h, 2, "TaxPeriodEndDt", &format!("{tax_year}-12-31"));
    e.text(&mut h, 2, "ReturnTypeCd", "1040");
    e.open(&mut h, 2, "Filer");
    e.text(&mut h, 3, "PrimarySSN", &primary_ssn);
    let spouse = identity.spouse.as_ref();
    let joint = status == FilingStatus::Mfj;
    if joint {
        match spouse
            .and_then(|s| s.ssn.as_deref())
            .and_then(normalize_ssn)
        {
            Some(spouse_ssn) => e.text(&mut h, 3, "SpouseSSN", &spouse_ssn),
            None => notes.push(
                "the ReturnHeader Filer has no SpouseSSN: this is a joint return but \
                 no spouse SSN is stored in the identity"
                    .to_string(),
            ),
        }
    }
    // Name line as it appears on the return: the taxpayer, plus the
    // spouse on a joint return.
    let name_line = match (joint, spouse.and_then(full_name)) {
        (true, Some(spouse_name)) => format!("{primary_name} & {spouse_name}"),
        _ => primary_name.clone(),
    };
    e.text(&mut h, 3, "NameLine1Txt", &name_line);
    if let Some(last) = identity.person.last_name.as_deref() {
        e.text(&mut h, 3, "PrimaryNameControlTxt", &name_control(last));
    }
    if joint {
        if let Some(spouse_last) = spouse.and_then(|s| s.last_name.as_deref()) {
            e.text(
                &mut h,
                3,
                "SpouseNameControlTxt",
                &name_control(spouse_last),
            );
        }
    }
    match &identity.address {
        Some(addr) => {
            let street = addr.street.as_deref().unwrap_or("").trim();
            let city = addr.city.as_deref().unwrap_or("").trim();
            let state = addr.state.as_deref().unwrap_or("").trim();
            let zip = addr.zip.as_deref().unwrap_or("").trim();
            if !street.is_empty() && !city.is_empty() && !state.is_empty() && !zip.is_empty() {
                e.open(&mut h, 3, "USAddress");
                e.text(&mut h, 4, "AddressLine1Txt", street);
                if let Some(apt) = addr.apt.as_deref().map(str::trim) {
                    if !apt.is_empty() {
                        e.text(&mut h, 4, "AddressLine2Txt", apt);
                    }
                }
                e.text(&mut h, 4, "CityNm", city);
                e.text(&mut h, 4, "StateAbbreviationCd", state);
                e.text(&mut h, 4, "ZIPCd", zip);
                e.close(&mut h, 3, "USAddress");
            } else {
                notes.push(
                    "the ReturnHeader Filer has no USAddress: the stored mailing \
                     address is incomplete (street, city, state, and ZIP are all \
                     required to emit one)"
                        .to_string(),
                );
            }
        }
        None => notes.push(
            "the ReturnHeader Filer has no USAddress: no mailing address is stored \
             in the identity"
                .to_string(),
        ),
    }
    e.close(&mut h, 2, "Filer");
    xml.push_str(&h);
    xml.push_str("  </ReturnHeader>\n");
}

// ---------------------------------------------------------------------------
// Form 1040 (element order follows IRS1040Type in the TY2025v5.2 XSD;
// paper line numbers per the XSDs' own LineNumber annotations)
// ---------------------------------------------------------------------------

fn emit_irs1040(
    input: &Form1040Input,
    computed: &ComputedReturn,
    params: &TaxParams,
    identity: &TaxpayerIdentity,
    em: &mut Emitter,
    out: &mut String,
    notes: &mut Vec<String>,
) {
    let lines = &computed.lines;
    let l = |key: &str| fact(lines, key);

    em.text(
        out,
        3,
        "IndividualReturnFilingStatusCd",
        match input.status {
            FilingStatus::Single => "1",
            FilingStatus::Mfj => "2",
            FilingStatus::Mfs => "3",
            FilingStatus::Hoh => "4",
        },
    );
    // The SpouseNm choice on the form body serves the MFS spouse-name
    // line (and NRA elections); on a joint return the spouse's name
    // lives in the ReturnHeader NameLine1Txt.
    if input.status == FilingStatus::Mfs {
        if let Some(spouse_name) = identity.spouse.as_ref().and_then(full_name) {
            em.text(out, 3, "SpouseNm", &spouse_name);
        }
    }
    // Required by the schema on every 1040 (the digital-assets
    // question). The return records no digital-asset answer and no
    // digital-asset transaction exists anywhere in its modeled
    // scope; emitted false, disclosed in the export notes.
    em.boolean(out, 3, "VirtualCurAcquiredDurTYInd", false);

    // Dependents (the form's Dependents table).
    let mut listed = 0usize;
    let mut lived_with = 0usize;
    let mut any_child_mapping = false;
    let mut any_single_token = false;
    for (idx, dep) in input.dependents.iter().enumerate() {
        let Some(ssn) = dep.ssn.as_deref().and_then(normalize_ssn) else {
            notes.push(format!(
                "dependent #{} is not listed in IRS1040/DependentDetail: no SSN is \
                 stored for them, and the schema requires one (the dependent still \
                 counts in the engine's credit computations)",
                idx + 1
            ));
            continue;
        };
        listed += 1;
        if dep.months_lived_with_taxpayer > 6 {
            lived_with += 1;
        }
        if dep.relationship == DependentRelationship::Child {
            any_child_mapping = true;
        }
        if !dep.name.contains(char::is_whitespace) {
            any_single_token = true;
        }
        let (first, last) = split_name(&dep.name);
        em.open(out, 3, "DependentDetail");
        em.text(out, 4, "DependentFirstNm", &first);
        em.text(out, 4, "DependentLastNm", &last);
        em.text(out, 4, "DependentNameControlTxt", &name_control(&last));
        em.text(out, 4, "DependentSSN", &ssn);
        em.text(
            out,
            4,
            "DependentRelationshipCd",
            relationship_cd_1040(dep.relationship),
        );
        if dep.months_lived_with_taxpayer > 6 {
            em.checkbox(out, 4, "YesLiveWithChildOverHalfYrInd");
        }
        if dep.full_time_student {
            em.checkbox(out, 4, "ChildIsAStudentUnder24Ind");
        }
        if dep.permanently_disabled {
            em.checkbox(out, 4, "ChildPermanentlyDisabledInd");
        }
        match classify_dependent(dep) {
            DependentCreditClass::ChildTaxCredit => {
                em.checkbox(out, 4, "EligibleForChildTaxCreditInd")
            }
            DependentCreditClass::OtherDependent => em.checkbox(out, 4, "EligibleForODCInd"),
            DependentCreditClass::None => {}
        }
        em.close(out, 3, "DependentDetail");
    }
    if any_child_mapping {
        notes.push(
            "DependentDetail/DependentRelationshipCd: the schema's values are \
             gender-specific for a child (SON / DAUGHTER) and the return records the \
             relationship without gender, so a plain child is emitted as OTHER — a \
             disclosed lossy mapping; STEPCHILD and FOSTER CHILD map exactly"
                .to_string(),
        );
    }
    if any_single_token {
        notes.push(
            "a dependent's stored name has a single token; it is used as both the \
             first and last name in DependentDetail (the return stores one name \
             field per dependent)"
                .to_string(),
        );
    }
    if listed > 4 {
        em.checkbox(out, 3, "MoreDependentsInd");
    }
    if listed > 0 {
        em.text(out, 3, "ChldWhoLivedWithYouCnt", &lived_with.to_string());
        em.text(
            out,
            3,
            "OtherDependentsListedCnt",
            &(listed - lived_with).to_string(),
        );
    }

    // Income.
    let wages = l("1z");
    if !input.w2s.is_empty() {
        em.amt(out, 3, "WagesAmt", wages);
    }
    if !input.w2s.is_empty() {
        em.amt(out, 3, "WagesSalariesAndTipsAmt", wages);
    }
    em.amt_nz(out, 3, "TaxExemptInterestAmt", l("2a"));
    em.amt_nz(out, 3, "TaxableInterestAmt", l("2b"));
    em.amt_nz(out, 3, "QualifiedDividendsAmt", l("3a"));
    em.amt_nz(out, 3, "OrdinaryDividendsAmt", l("3b"));
    let r7 = if !input.f1099_bs.is_empty() {
        em.amt(out, 3, "CapitalGainLossAmt", l("7"))
    } else {
        whole_dollars(l("7"))
    };
    let r8 = if l("8").cents() != 0 {
        em.amt(out, 3, "TotalAdditionalIncomeAmt", l("8"))
    } else {
        0
    };
    let r9 = em.amt(out, 3, "TotalIncomeAmt", l("9"));
    cross_foot(
        notes,
        "IRS1040/TotalIncomeAmt",
        r9,
        whole_dollars(wages) + whole_dollars(l("2b")) + whole_dollars(l("3b")) + r7 + r8,
    );
    em.amt_nz(out, 3, "TotalAdjustmentsAmt", l("10"));
    let r11 = em.amt(out, 3, "AdjustedGrossIncomeAmt", l("11"));
    cross_foot(
        notes,
        "IRS1040/AdjustedGrossIncomeAmt",
        r11,
        r9 - whole_dollars(l("10")),
    );

    // Deduction block.
    if identity.can_be_claimed_as_dependent {
        em.checkbox(out, 3, "PrimaryClaimAsDependentInd");
    }
    let mut boxes = 0;
    if identity.person.is_age_65(params) {
        em.checkbox(out, 3, "Primary65OrOlderInd");
        boxes += 1;
    }
    if identity.person.blind {
        em.checkbox(out, 3, "PrimaryBlindInd");
        boxes += 1;
    }
    if input.status == FilingStatus::Mfj {
        if let Some(spouse) = &identity.spouse {
            if spouse.is_age_65(params) {
                em.checkbox(out, 3, "Spouse65OrOlderInd");
                boxes += 1;
            }
            if spouse.blind {
                em.checkbox(out, 3, "SpouseBlindInd");
                boxes += 1;
            }
        }
    }
    if boxes > 0 {
        em.text(out, 3, "TotalBoxesCheckedCnt", &boxes.to_string());
    }
    em.amt(out, 3, "TotalItemizedOrStandardDedAmt", l("12"));
    em.amt_nz(out, 3, "TotalAdditionalDeductionsAmt", l("13b"));
    let r14 = em.amt(out, 3, "TotalDeductionsAmt", l("14"));
    cross_foot(
        notes,
        "IRS1040/TotalDeductionsAmt",
        r14,
        whole_dollars(l("12")) + whole_dollars(l("13b")),
    );
    em.amt(out, 3, "TaxableIncomeAmt", l("15"));

    // Tax and credits. Line 18 = line 16 in this engine: line 17
    // (Schedule 2, lines 1–3) has no modeled inputs, a stated zero.
    em.amt(out, 3, "TaxAmt", l("16"));
    em.amt(out, 3, "TotalTaxBeforeCrAndOthTaxesAmt", l("16"));
    em.amt_nz(out, 3, "CTCODCAmt", l("19"));
    em.amt_nz(out, 3, "TotalCreditsAmt", l("21"));
    em.amt(out, 3, "TaxLessCreditsAmt", l("22"));
    em.amt_nz(out, 3, "TotalOtherTaxesAmt", l("23"));
    let r24 = em.amt(out, 3, "TotalTaxAmt", l("24"));
    cross_foot(
        notes,
        "IRS1040/TotalTaxAmt",
        r24,
        whole_dollars(l("22")) + whole_dollars(l("23")),
    );

    // Payments. Line 25b is summed from the stored 1099s (the engine
    // tracks only the 25d total as a line).
    let withholding_1099: Money = input
        .f1099_ints
        .iter()
        .map(|i| i.federal_withholding)
        .chain(input.f1099_divs.iter().map(|d| d.federal_withholding))
        .chain(input.f1099_bs.iter().map(|b| b.federal_withholding))
        .chain(input.f1099_necs.iter().map(|n| n.federal_withholding))
        .sum();
    em.amt_nz(out, 3, "FormW2WithheldTaxAmt", l("25a"));
    em.amt_nz(out, 3, "Form1099WithheldTaxAmt", withholding_1099);
    em.amt_nz(out, 3, "WithholdingTaxAmt", l("25d"));
    em.amt_nz(out, 3, "EarnedIncomeCreditAmt", l("27"));
    em.amt_nz(out, 3, "AdditionalChildTaxCreditAmt", l("28"));
    em.amt_nz(out, 3, "RefundableCreditsAmt", l("32"));
    let r33 = em.amt(out, 3, "TotalPaymentsAmt", l("33"));
    cross_foot(
        notes,
        "IRS1040/TotalPaymentsAmt",
        r33,
        whole_dollars(l("25d")) + whole_dollars(l("32")),
    );

    // Settlement: the engine's full-refund assumption (no Form 8888
    // split, nothing applied to next year's estimates).
    if l("34").cents() > 0 {
        em.amt(out, 3, "OverpaidAmt", l("34"));
        em.amt(out, 3, "RefundAmt", l("34"));
    }
    if l("37").cents() > 0 {
        em.amt(out, 3, "OwedAmt", l("37"));
    }

    if let Some(occupation) = identity.person.occupation.as_deref() {
        if !occupation.trim().is_empty() {
            em.text(out, 3, "PrimaryOccupationTxt", occupation.trim());
        }
    }
    if input.status == FilingStatus::Mfj {
        if let Some(occupation) = identity
            .spouse
            .as_ref()
            .and_then(|s| s.occupation.as_deref())
        {
            if !occupation.trim().is_empty() {
                em.text(out, 3, "SpouseOccupationTxt", occupation.trim());
            }
        }
    }
    // Required by the schema on every 1040 since TY2022: which
    // refund financial product, if any, the filer bought. This tool
    // sells none.
    em.text(out, 3, "RefundProductCd", "NO FINANCIAL PRODUCT");
}

// ---------------------------------------------------------------------------
// Schedule 1 (additional income / adjustments) — v1 sources: Schedule C
// business income (line 3) and the deductible part of SE tax (line 15),
// mirroring the packet, which leaves printed line 9 blank (no 8a–8z
// other-income items are modeled).
// ---------------------------------------------------------------------------

fn emit_schedule_1(computed: &ComputedReturn, em: &mut Emitter, out: &mut String) {
    let lines = &computed.lines;
    em.amt(
        out,
        3,
        "BusinessIncomeLossAmt",
        fact(lines, "schc_net_profit"),
    );
    em.amt(out, 3, "TotalAdditionalIncomeAmt", fact(lines, "8"));
    em.amt_nz(out, 3, "DeductibleSelfEmploymentTaxAmt", fact(lines, "10"));
    em.amt_nz(out, 3, "TotalAdjustmentsAmt", fact(lines, "10"));
}

// ---------------------------------------------------------------------------
// Schedule 2 (additional taxes) — v1 source: SE tax (line 4) only.
// ---------------------------------------------------------------------------

fn emit_schedule_2(computed: &ComputedReturn, em: &mut Emitter, out: &mut String) {
    let lines = &computed.lines;
    em.amt(out, 3, "SelfEmploymentTaxAmt", fact(lines, "sched2_se_tax"));
    em.amt(out, 3, "TotalOtherAdditionalTaxesAmt", fact(lines, "23"));
    em.amt(out, 3, "TotalOtherTaxesAmt", fact(lines, "23"));
}

// ---------------------------------------------------------------------------
// Schedule C — one document per 1099-NEC engagement, mirroring the
// packet: the payer names the business, expenses are one Part V
// "other expenses" aggregate carried to line 48 and into line 28.
// ---------------------------------------------------------------------------

fn emit_schedule_c(
    nec: &crate::I1099NecInput,
    primary_name: &str,
    primary_ssn: &str,
    em: &mut Emitter,
    out: &mut String,
) {
    let gross = nec.nonemployee_compensation;
    let expenses = nec.expenses;
    let net = gross - expenses;
    em.text(out, 3, "ProprietorNm", primary_name);
    em.text(out, 3, "SSN", primary_ssn);
    if !nec.payer_name.trim().is_empty() {
        em.text(out, 3, "BusinessNameLine1Txt", nec.payer_name.trim());
    }
    if let Some(ein) = nec.payer_ein.as_deref() {
        let digits: String = ein.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() == 9 {
            em.text(out, 3, "EIN", &digits);
        }
    }
    // Required choice in the schema; see the note in build_mef.
    em.checkbox(out, 3, "MethodOfAccountingCashInd");
    em.amt(out, 3, "TotalGrossReceiptsAmt", gross);
    em.amt(out, 3, "NetGrossReceiptsAmt", gross);
    em.amt(out, 3, "GrossProfitAmt", gross);
    em.amt(out, 3, "GrossIncomeAmt", gross);
    em.amt(out, 3, "TotalExpensesAmt", expenses);
    em.amt(out, 3, "TentativeProfitOrLossAmt", net);
    em.amt(out, 3, "NetProfitOrLossAmt", net);
    if expenses.cents() != 0 {
        em.open(out, 3, "OtherExpenseDetail");
        em.text(
            out,
            4,
            "Desc",
            // ASCII hyphen, not the packet PDF's em dash: the
            // schema's LineExplanationType pattern excludes it.
            "Business expenses - single aggregate (not itemized in v1)",
        );
        em.amt(out, 4, "Amt", expenses);
        em.close(out, 3, "OtherExpenseDetail");
        em.amt(out, 3, "TotalOtherExpensesAmt", expenses);
    }
}

// ---------------------------------------------------------------------------
// Schedule SE — the simplified computation, line by line (the packet's
// derivations). Line 8a carries the engine's W-2 Box 1 proxy for Box 3
// wages (noted in build_mef).
// ---------------------------------------------------------------------------

fn emit_schedule_se(
    input: &Form1040Input,
    computed: &ComputedReturn,
    params: &TaxParams,
    identity: &TaxpayerIdentity,
    em: &mut Emitter,
    out: &mut String,
    notes: &mut Vec<String>,
) {
    let lines = &computed.lines;
    let schc_net = fact(lines, "schc_net_profit");
    let wages: Money = input.w2s.iter().map(|w| w.wages).sum();
    let se = schedule_se_tax(schc_net, wages, params);
    let wage_base = Money::from_cents(params.social_security_wage_base_cents);
    let ss_room = Money::from_cents((wage_base - wages).cents().max(0));
    let allowable = Money::from_cents(se.net_earnings.cents().min(ss_room.cents()));
    let deductible = deductible_part_of_se_tax(se.tax);
    // Presence gated in build_mef; re-derive for the form header.
    let primary_name = full_name(&identity.person).unwrap_or_default();
    let primary_ssn = identity
        .person
        .ssn
        .as_deref()
        .and_then(normalize_ssn)
        .unwrap_or_default();

    em.text(out, 3, "PersonNm", &primary_name);
    em.text(out, 3, "SSN", &primary_ssn);
    em.amt(out, 3, "NetNonFarmProfitLossAmt", schc_net);
    em.amt(out, 3, "SETotalNetEarningsOrLossAmt", schc_net);
    em.amt(out, 3, "MinimumProfitForSETaxAmt", se.net_earnings);
    em.amt(out, 3, "CombinedSEAmt", se.net_earnings);
    if wages.cents() != 0 {
        em.amt(out, 3, "SSTWagesRRTCompAmt", wages);
        em.amt(out, 3, "WagesSubjectToSSTAmt", wages);
        em.amt_nz(out, 3, "TotalWagesAndUnreportedTipsAmt", ss_room);
    }
    em.amt(out, 3, "AllowableSEAmt", allowable);
    let r10 = em.amt(out, 3, "TaxBaseAmt", se.ss_part);
    let r11 = em.amt(out, 3, "SEBaseAmt", se.medicare_part);
    let r12 = em.amt(out, 3, "SelfEmploymentTaxAmt", se.tax);
    cross_foot(
        notes,
        "IRS1040ScheduleSE/SelfEmploymentTaxAmt",
        r12,
        r10 + r11,
    );
    em.amt(out, 3, "DeductibleSelfEmploymentTaxAmt", deductible);
}

// ---------------------------------------------------------------------------
// Schedule D — mirroring the packet's categorization: every stored
// 1099-B transaction is treated as basis-reported and listed on Form
// 8949, so the totals land on lines 1b (short-term) and 8b
// (long-term). Prior-year carryovers are not inputs in v1, so lines
// 6/14 stay absent; the 28%-rate and unrecaptured §1250 worksheet
// amounts (lines 18/19) are not figured by the engine, so they stay
// absent and line 20's question is answered on that basis.
// ---------------------------------------------------------------------------

fn emit_schedule_d(
    input: &Form1040Input,
    computed: &ComputedReturn,
    em: &mut Emitter,
    out: &mut String,
    notes: &mut Vec<String>,
) {
    let lines = &computed.lines;
    let (st, lt) = all_txns(input);
    let st_totals = group_totals(&st);
    let lt_totals = group_totals(&lt);
    let net_short = fact(lines, "schd_net_short");
    let net_long = fact(lines, "schd_net_long");
    // Schedule D line 16 (the combination) and Form 1040 line 7 (the
    // combination after the §1211(b) loss limit) are both engine
    // facts; no worksheet recomputation is needed here.
    let combined = net_short + net_long;
    let line7 = fact(lines, "7");

    if !st.is_empty() {
        em.open(out, 3, "TotalSTCGL1099ShowsBasisGrp");
        em.amt(out, 4, "TotalProceedsSalesPriceAmt", st_totals.proceeds);
        em.amt(out, 4, "TotalCostOrOtherBasisAmt", st_totals.basis);
        em.amt_nz(
            out,
            4,
            "TotAdjustmentsToGainOrLossAmt",
            st_totals.adjustments,
        );
        em.amt(out, 4, "TotalGainOrLossAmt", st_totals.gain);
        em.close(out, 3, "TotalSTCGL1099ShowsBasisGrp");
    }
    let r_net_short = em.amt(out, 3, "NetSTCapitalGainOrLossAmt", net_short);
    if !lt.is_empty() {
        em.open(out, 3, "TotalLTCGL1099ShowsBasisGrp");
        em.amt(out, 4, "TotalProceedsSalesPriceAmt", lt_totals.proceeds);
        em.amt(out, 4, "TotalCostOrOtherBasisAmt", lt_totals.basis);
        em.amt_nz(
            out,
            4,
            "TotAdjustmentsToGainOrLossAmt",
            lt_totals.adjustments,
        );
        em.amt(out, 4, "TotalGainOrLossAmt", lt_totals.gain);
        em.close(out, 3, "TotalLTCGL1099ShowsBasisGrp");
    }
    let r_net_long = em.amt(out, 3, "NetLTCapitalGainOrLossAmt", net_long);
    let r_combined = em.amt(out, 3, "NetSTAndLTCapitalGainOrLossAmt", combined);
    cross_foot(
        notes,
        "IRS1040ScheduleD/NetSTAndLTCapitalGainOrLossAmt",
        r_combined,
        r_net_short + r_net_long,
    );
    if computed.line16_method.as_deref() == Some("qdcg") {
        em.boolean(
            out,
            3,
            "STAndLTGainInd",
            net_long.cents() > 0 && combined.cents() > 0,
        );
        // Lines 18/19 are not figured in v1, so both are blank and
        // line 20's question ("is either blank or zero?") is yes.
        em.boolean(out, 3, "ZeroOrBlankInd", true);
    }
    if line7.cents() < 0 {
        // Line 21: the allowable loss, as a positive amount.
        em.amt(out, 3, "AllowableLossAmt", -line7);
    }
    if computed.line16_method.as_deref() == Some("qdcg") {
        em.boolean(
            out,
            3,
            "QualifiedDividendsInd",
            fact(lines, "3a").cents() > 0,
        );
    }
}

/// All stored 1099-B transactions across every 1099-B entry, in
/// stored order, split by holding period (the packet's grouping).
fn all_txns(input: &Form1040Input) -> (Vec<&crate::BrokerTxn>, Vec<&crate::BrokerTxn>) {
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

struct GroupTotals {
    proceeds: Money,
    basis: Money,
    adjustments: Money,
    gain: Money,
}

/// The packet's group totals: only fully reported transactions
/// (proceeds and basis both present) contribute.
fn group_totals(txns: &[&crate::BrokerTxn]) -> GroupTotals {
    let mut t = GroupTotals {
        proceeds: Money::ZERO,
        basis: Money::ZERO,
        adjustments: Money::ZERO,
        gain: Money::ZERO,
    };
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

// ---------------------------------------------------------------------------
// Form 8949 — every stored transaction listed, short-term in Part I
// with Box A checked, long-term in Part II with Box D (the packet's
// categorization: the 1099-B input carries no basis-reported flag).
// ---------------------------------------------------------------------------

fn emit_8949(input: &Form1040Input, em: &mut Emitter, out: &mut String) {
    let (st, lt) = all_txns(input);
    if !st.is_empty() {
        emit_8949_group(&st, true, em, out);
    }
    if !lt.is_empty() {
        emit_8949_group(&lt, false, em, out);
    }
}

fn emit_8949_group(txns: &[&crate::BrokerTxn], short: bool, em: &mut Emitter, out: &mut String) {
    let group = if short {
        "ShortTermCapitalGainAndLossGrp"
    } else {
        "LongTermCapitalGainAndLossGrp"
    };
    em.open(out, 3, group);
    em.checkbox(out, 4, "TransRptOn1099BThatShowBssInd");
    for txn in txns {
        em.open(out, 4, "CapitalGainAndLossAssetGrp");
        em.text(out, 5, "PropertyDesc", &txn.description);
        if let Some(d) = &txn.date_acquired {
            em.text(out, 5, "AcquiredDt", d);
        }
        if let Some(d) = &txn.date_sold {
            em.text(out, 5, "SoldOrDisposedDt", d);
        }
        if let Some(p) = txn.proceeds {
            em.amt(out, 5, "ProceedsSalesPriceAmt", p);
        }
        if let Some(b) = txn.cost_basis {
            em.amt(out, 5, "CostOrOtherBasisAmt", b);
        }
        if let Some(code) = &txn.adjustment_code {
            if !code.trim().is_empty() {
                em.text(out, 5, "AdjustmentsToGainOrLossCd", code.trim());
            }
        }
        em.amt_nz(out, 5, "AdjustmentsToGainOrLossAmt", txn.adjustment);
        if let Some(g) = txn.gain() {
            em.amt(out, 5, "GainOrLossAmt", g);
        }
        em.close(out, 4, "CapitalGainAndLossAssetGrp");
    }
    let totals = group_totals(txns);
    em.amt(out, 4, "TotalProceedsSalesPriceAmt", totals.proceeds);
    em.amt(out, 4, "TotalCostOrOtherBasisAmt", totals.basis);
    em.amt_nz(out, 4, "TotAdjustmentsToGainOrLossAmt", totals.adjustments);
    em.amt(out, 4, "TotalGainOrLossAmt", totals.gain);
    em.close(out, 3, group);
}

// ---------------------------------------------------------------------------
// Schedule 8812 — recomputed from the same worksheet primitive the
// packet fills from. Part II-B (three or more qualifying children) is
// not implemented by the engine; the simplified Part II-A result is
// what the form carries (docs/KNOWN_LIMITS.md).
// ---------------------------------------------------------------------------

fn emit_schedule_8812(
    input: &Form1040Input,
    computed: &ComputedReturn,
    params: &TaxParams,
    em: &mut Emitter,
    out: &mut String,
) {
    let lines = &computed.lines;
    let agi = fact(lines, "11");
    let income_tax = fact(lines, "16");
    let earned_income = fact(lines, "sched8812_earned_income");
    let s = schedule_8812(
        &input.dependents,
        input.status,
        agi,
        income_tax,
        earned_income,
        params,
    );
    let threshold = params.ctc_phaseout_threshold(input.status);
    // Line 10: the MAGI excess over the threshold, rounded UP to the
    // next multiple of $1,000 (the worksheet's own step).
    let excess_cents = (agi - threshold).cents().max(0);
    let line10 = if excess_cents == 0 {
        Money::ZERO
    } else {
        Money::from_cents((excess_cents + 100_000 - 1) / 100_000 * 100_000)
    };

    em.amt(out, 3, "AdjustedGrossIncomeAmt", agi);
    em.amt(out, 3, "ModifiedAGIAmt", agi);
    if s.ctc_children > 0 {
        em.text(
            out,
            3,
            "QlfyChildUnderAgeSSNCnt",
            &s.ctc_children.to_string(),
        );
        em.amt(
            out,
            3,
            "QlfyChildUnderAgeSSNLimtAmt",
            Money::from_cents(s.ctc_children as i64 * params.ctc_per_child_cents),
        );
    }
    if s.odc_dependents > 0 {
        em.text(out, 3, "OtherDependentCnt", &s.odc_dependents.to_string());
        em.amt(
            out,
            3,
            "OtherDependentCreditAmt",
            Money::from_cents(s.odc_dependents as i64 * params.odc_per_dependent_cents),
        );
    }
    if s.tentative_credit.cents() > 0 {
        em.amt(out, 3, "InitialCTCODCAmt", s.tentative_credit);
        em.text(
            out,
            3,
            "FilingStatusThresholdCd",
            &(threshold.cents() / 100).to_string(),
        );
        if line10.cents() > 0 {
            em.amt(out, 3, "ExcessAdjGrossIncomeAmt", line10);
            em.amt(out, 3, "ModifiedAGIPhaseOutAmt", s.phaseout_reduction);
        }
    }
    // Required by the schema whenever Schedule 8812 is filed.
    em.boolean(
        out,
        3,
        "CTCODCOverAGILimitInd",
        s.phaseout_reduction.cents() > 0,
    );
    if s.tentative_credit.cents() > 0 {
        em.amt(out, 3, "CTCODCAfterAGILimitAmt", s.credit_after_phaseout);
        em.amt(out, 3, "ACTCTaxLiabiltyLimitAmt", s.credit_limit);
        em.amt(out, 3, "CTCODCAmt", s.nonrefundable_credit);
    }

    if s.additional_child_tax_credit.cents() > 0 {
        let unused = s.credit_after_phaseout - s.nonrefundable_credit;
        let actc_cap = Money::from_cents(s.ctc_children as i64 * params.actc_cap_per_child_cents);
        let line17 = Money::from_cents(unused.cents().min(actc_cap.cents()));
        let ei_threshold = Money::from_cents(params.actc_earned_income_threshold_cents);
        let ei_excess = Money::from_cents((s.earned_income - ei_threshold).cents().max(0));
        let line20 = ei_excess.mul_bp(params.actc_rate_bp);
        em.open(out, 3, "ClaimACTCAllFilersGrp");
        em.amt(out, 4, "ACTCBeforeLimitAmt", unused);
        em.text(
            out,
            4,
            "QlfyChildUnderAgeSSNCnt",
            &s.ctc_children.to_string(),
        );
        em.amt(out, 4, "QlfyChildUnderAgeSSNLimtAmt", actc_cap);
        em.amt(out, 4, "ACTCAfterLimitAmt", line17);
        em.amt(out, 4, "TotalEarnedIncomeAmt", s.earned_income);
        em.boolean(
            out,
            4,
            "EarnedIncmMoreThanSpecifiedInd",
            s.earned_income.cents() > ei_threshold.cents(),
        );
        em.amt(out, 4, "NetTotalEarnedIncomeAmt", ei_excess);
        em.amt(out, 4, "NetEarnedIncomeCalculatedAmt", line20);
        em.boolean(out, 4, "ThreeOrMoreQlfyChildrenInd", s.ctc_children >= 3);
        em.close(out, 3, "ClaimACTCAllFilersGrp");
        em.amt(
            out,
            3,
            "AdditionalChildTaxCreditAmt",
            s.additional_child_tax_credit,
        );
    }
}

// ---------------------------------------------------------------------------
// Schedule 1-A — Part V (enhanced deduction for seniors) only, the
// packet's derivations. Parts II–IV are not modeled, so the line 38
// total is the Part V line 37 amount.
// ---------------------------------------------------------------------------

fn emit_schedule_1a(
    input: &Form1040Input,
    bonus: &crate::SeniorBonus,
    params: &TaxParams,
    agi: Money,
    em: &mut Emitter,
    out: &mut String,
) {
    let threshold = params.senior_bonus_phaseout_threshold(input.status);
    let excess = Money::from_cents((bonus.magi - threshold).cents().max(0));
    let line34 = excess.mul_bp(params.senior_bonus_phaseout_rate_bp);
    let eligible = |p: &PersonIdentity| p.is_age_65(params) && p.ssn.is_some();
    let (primary_eligible, spouse_eligible) = match &input.identity {
        Some(id) => (
            eligible(&id.person),
            input.status == FilingStatus::Mfj && id.spouse.as_ref().map(eligible).unwrap_or(false),
        ),
        None => (false, false),
    };

    em.amt(out, 3, "AdjustedGrossIncomeAmt", agi);
    em.amt(out, 3, "ModifiedAGIAmt", bonus.magi);
    em.amt(out, 3, "EnhncSrDedFSThrshldAmt", threshold);
    em.amt_nz(out, 3, "EnhncSrDedMAGILessThrshldAmt", excess);
    em.amt_nz(out, 3, "EnhnSrDedMAGILessThrshldRedAmt", line34);
    if bonus.eligible_persons > 0 {
        em.amt(out, 3, "SpecfiedDolLessThrshldRedAmt", bonus.per_person);
    }
    if primary_eligible {
        em.amt(out, 3, "PrimaryEnhancedSeniorDedAmt", bonus.per_person);
    }
    if spouse_eligible {
        em.amt(out, 3, "SpouseEnhancedSeniorDedAmt", bonus.per_person);
    }
    em.amt(out, 3, "EnhancedSeniorDeductionAmt", bonus.deduction);
    em.amt(out, 3, "TotalAdditionalDeductionsAmt", bonus.deduction);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        compute_1040, I1099NecInput, MailingAddress, PersonIdentity, TaxpayerIdentity, W2Input,
    };

    fn params() -> TaxParams {
        TaxParams::load_2025().unwrap()
    }

    fn cents(dollars: i64) -> Money {
        Money::from_cents(dollars * 100)
    }

    fn identity() -> TaxpayerIdentity {
        TaxpayerIdentity {
            person: PersonIdentity {
                first_name: Some("Spike".to_string()),
                last_name: Some("Tester".to_string()),
                ssn: Some("123456789".to_string()),
                date_of_birth: Some("1990-05-01".to_string()),
                occupation: Some("Engineer".to_string()),
                blind: false,
            },
            address: Some(MailingAddress {
                street: Some("1 Main St".to_string()),
                apt: None,
                city: Some("Springfield".to_string()),
                state: Some("IL".to_string()),
                zip: Some("62701".to_string()),
            }),
            spouse: None,
            can_be_claimed_as_dependent: false,
        }
    }

    fn base_input() -> Form1040Input {
        Form1040Input {
            status: FilingStatus::Single,
            w2s: vec![W2Input {
                employer_ein: "12-3456789".to_string(),
                wages: cents(50_000),
                federal_withholding: cents(4_000),
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
            identity: Some(identity()),
            state: None,
        }
    }

    const TS: &str = "2026-10-27T00:00:00Z";

    #[test]
    fn rounding_is_half_away_from_zero() {
        assert_eq!(whole_dollars(Money::from_cents(254_332)), 2543);
        assert_eq!(whole_dollars(Money::from_cents(127_166)), 1272);
        assert_eq!(whole_dollars(Money::from_cents(50)), 1);
        assert_eq!(whole_dollars(Money::from_cents(49)), 0);
        assert_eq!(whole_dollars(Money::from_cents(-150)), -2);
        assert_eq!(whole_dollars(Money::from_cents(-149)), -1);
        assert_eq!(whole_dollars(Money::ZERO), 0);
        assert_eq!(whole_dollars(cents(38_000)), 38_000);
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            esc("Smith & Wesson <LLC>"),
            "Smith &amp; Wesson &lt;LLC&gt;"
        );
        assert_eq!(esc("plain"), "plain");
    }

    #[test]
    fn name_controls() {
        assert_eq!(name_control("Tester"), "TEST");
        assert_eq!(name_control("Li"), "LI");
        assert_eq!(name_control("O'Brien-Smith"), "OBRI");
    }

    #[test]
    fn simple_return_has_only_irs1040() {
        // Case-001 shape: single, one W-2 $50,000 / $4,000 withheld.
        let input = base_input();
        let computed = compute_1040(&input, &params());
        let export = build_mef(&input, &computed, &params(), 2025, TS).unwrap();
        assert_eq!(export.documents, vec!["IRS1040".to_string()]);
        let xml = &export.xml;
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(
            xml.contains("<Return xmlns=\"http://www.irs.gov/efile\" returnVersion=\"2025v5.2\">")
        );
        assert!(xml.contains("<ReturnHeader binaryAttachmentCnt=\"0\">"));
        assert!(xml.contains("<PrimarySSN>123456789</PrimarySSN>"));
        assert!(xml.contains("<NameLine1Txt>Spike Tester</NameLine1Txt>"));
        assert!(xml.contains("<PrimaryNameControlTxt>TEST</PrimaryNameControlTxt>"));
        assert!(xml.contains("<ReturnData documentCnt=\"1\">"));
        assert!(xml.contains("<IRS1040 documentId=\"1\">"));
        assert!(!xml.contains("Schedule"), "no schedule elements: {xml}");
        assert!(xml.contains("<WagesAmt>50000</WagesAmt>"));
        assert!(xml.contains("<AdjustedGrossIncomeAmt>50000</AdjustedGrossIncomeAmt>"));
        assert!(xml.contains("<TaxAmt>3875</TaxAmt>"));
        assert!(xml.contains("<RefundAmt>125</RefundAmt>"));
        assert!(xml.contains("<RefundProductCd>NO FINANCIAL PRODUCT</RefundProductCd>"));
        assert!(xml.contains("<VirtualCurAcquiredDurTYInd>false</VirtualCurAcquiredDurTYInd>"));
    }

    #[test]
    fn missing_identity_is_an_error() {
        let mut input = base_input();
        input.identity = None;
        let computed = compute_1040(&input, &params());
        let e = build_mef(&input, &computed, &params(), 2025, TS).unwrap_err();
        assert_eq!(e.code, "identity_required");
        assert!(e.message.contains("identity required for MeF export"));
    }

    #[test]
    fn identity_without_ssn_is_an_error() {
        let mut input = base_input();
        input.identity.as_mut().unwrap().person.ssn = None;
        let computed = compute_1040(&input, &params());
        let e = build_mef(&input, &computed, &params(), 2025, TS).unwrap_err();
        assert_eq!(e.code, "identity_required");
    }

    #[test]
    fn nec_return_emits_schedules_and_records_rounding() {
        // Case-005 shape: W-2 $50k/$4k + NEC $20,000 comp, $2,000
        // expenses. SE tax is $2,543.32 — a genuine cents value.
        let mut input = base_input();
        input.f1099_necs = vec![I1099NecInput {
            payer_name: "Smith & Wesson LLC".to_string(),
            payer_ein: Some("12-3456789".to_string()),
            nonemployee_compensation: cents(20_000),
            federal_withholding: Money::ZERO,
            expenses: cents(2_000),
            state_withholding: None,
        }];
        let computed = compute_1040(&input, &params());
        let export = build_mef(&input, &computed, &params(), 2025, TS).unwrap();
        assert_eq!(
            export.documents,
            vec![
                "IRS1040".to_string(),
                "IRS1040Schedule1".to_string(),
                "IRS1040Schedule2".to_string(),
                "IRS1040ScheduleC".to_string(),
                "IRS1040ScheduleSE".to_string(),
            ]
        );
        let xml = &export.xml;
        // The payer name is XML-escaped.
        assert!(xml.contains("<BusinessNameLine1Txt>Smith &amp; Wesson LLC</BusinessNameLine1Txt>"));
        assert!(!xml.contains("Smith & Wesson"));
        // SE tax rounds half away from zero: 2543.32 -> 2543; the
        // deductible half, 1271.66, rounds to 1272.
        assert!(xml.contains("<SelfEmploymentTaxAmt>2543</SelfEmploymentTaxAmt>"));
        assert!(
            xml.contains("<DeductibleSelfEmploymentTaxAmt>1272</DeductibleSelfEmploymentTaxAmt>")
        );
        assert!(xml.contains("<NetProfitOrLossAmt>18000</NetProfitOrLossAmt>"));
        assert!(xml.contains("<TotalTaxAmt>8672</TotalTaxAmt>"));
        assert!(xml.contains("<OwedAmt>4672</OwedAmt>"));
        // Rounding was recorded, per field.
        assert!(export
            .rounded_fields
            .contains(&"IRS1040ScheduleSE/SelfEmploymentTaxAmt".to_string()));
        assert!(export
            .rounded_fields
            .contains(&"IRS1040ScheduleSE/DeductibleSelfEmploymentTaxAmt".to_string()));
        assert!(export
            .notes
            .iter()
            .any(|n| n.contains("fields rounded from cents")));
    }

    #[test]
    fn wrong_year_is_an_error() {
        let input = base_input();
        let computed = compute_1040(&input, &params());
        let e = build_mef(&input, &computed, &params(), 2024, TS).unwrap_err();
        assert_eq!(e.code, "unsupported_tax_year");
    }
}
