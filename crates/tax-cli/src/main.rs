//! `tax` — agent-first CLI for opentax.
//!
//! Conventions: JSON only on stdout when piped or `--output json`; human
//! text on a TTY; errors as `{"success":false,"error","code"}` on stderr
//! with exit 1. State is a local directory store (default `.opentax/`).

use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use tax_core::{CaParams, FactResult, FilingStatus, Money, TaxParams, TreatyParams};
use tax_doc::{LopdfTextExtractor, TextExtractor};
use tax_forms::mef::build_mef;
use tax_forms::packet::{
    build_ca_packet, build_nr_packet, build_packet, nr_mapped_incomplete, schedules_view,
};
use tax_forms::{
    additional_standard_deduction, build_fill_values_with_dependents, compute_1040, compute_1040nr,
    compute_ca540, deductible_part_of_se_tax, eic_childless_age_outcome, eic_qualifying_children,
    mask_ssn, normalize_ssn, schedule_c_net_profit, schedule_se_tax, senior_bonus_eligible_persons,
    validate_1099_div, validate_dependent, CaFinding, ComputedReturn, DependentInput, F1042sInput,
    Form1040Input, I1099BInput, I1099DivInput, I1099IntInput, I1099NecInput, NrFinding, NraBlock,
    PersonIdentity, ScheduleOiEntry, StateBlock, TaxpayerIdentity, W2Input,
};
use tax_pdf::{FillReport, LopdfBackend, PdfBackend, PdferBackend};

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutputMode {
    Auto,
    Json,
    Human,
}

#[derive(Parser, Debug)]
#[command(name = "tax", version, about = "Agent-first US tax CLI (opentax)")]
struct Cli {
    /// Output mode: auto = JSON when stdout is not a TTY.
    #[arg(long, value_enum, default_value = "auto", global = true)]
    output: OutputMode,
    /// State store directory.
    #[arg(long, default_value = ".opentax", global = true)]
    store: PathBuf,
    /// Skills directory (for `tax skills list`).
    #[arg(long, default_value = "skills", global = true)]
    skills_dir: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Environment and parameter-pack checks.
    Doctor,
    /// Run the golden benchmark cases and report pass/fail.
    Selftest,
    /// Print the (static, versioned) input schema.
    Schema {
        /// Optional input/command name to describe.
        command: Option<String>,
    },
    /// Registered document/node types and their templates.
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
    /// Return lifecycle.
    Return {
        #[command(subcommand)]
        action: ReturnAction,
    },
    /// Add a document entry (e.g. a W-2) to a return.
    Form {
        #[command(subcommand)]
        action: FormAction,
    },
    /// Document helpers.
    Doc {
        #[command(subcommand)]
        action: DocAction,
    },
    /// PDF form primitives: inspect and fill AcroForm fields.
    Pdf {
        #[command(subcommand)]
        action: PdfAction,
    },
    /// Explain how a line was derived.
    Explain {
        #[arg(long)]
        r#return: String,
        #[arg(long)]
        line: Option<String>,
        #[arg(long)]
        fact: Option<String>,
        /// Explain a STATE line instead of a federal one (e.g.
        /// `--state CA --line 31`). The return must have that state
        /// selected with `tax return set-state`; line numbers are the
        /// state form's printed lines (Form 540 for CA).
        #[arg(long)]
        state: Option<String>,
    },
    /// Bundled skills.
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
}

#[derive(Subcommand, Debug)]
enum NodeAction {
    /// List registered document types.
    List,
    /// Print the exact JSON template `form add` accepts for a document type.
    Template {
        #[arg(long)]
        document: String,
    },
}

#[derive(Subcommand, Debug)]
enum ReturnAction {
    /// Create a return in the store.
    Create {
        #[arg(long, default_value_t = 2025)]
        year: u32,
        /// single | mfj | hoh | mfs
        #[arg(long, default_value = "single")]
        status: String,
        /// Taxable interest, integer cents (e.g. 12500 = $125.00).
        #[arg(long)]
        interest_cents: Option<i64>,
        /// Ordinary dividends, integer cents.
        #[arg(long)]
        dividends_cents: Option<i64>,
    },
    /// Compute and show a stored return (partial returns show incomplete lines).
    Get {
        #[arg(long)]
        r#return: String,
    },
    /// Run deterministic validation over a stored return.
    Validate {
        #[arg(long)]
        r#return: String,
    },
    /// Store the taxpayer identity block from a JSON file (names, SSNs,
    /// DOBs, address, occupations). SSNs are normalized to digits,
    /// stored locally only, and echoed back masked.
    SetIdentity {
        #[arg(long)]
        r#return: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Select the state return to compute alongside the federal
    /// return, from a JSON file: {"state": "CA", "residency":
    /// "full_year"}. v1 computes only California for full-year
    /// residents (Form 540); any other state or residency is an
    /// explicit error. When a state is selected, `return get` gains a
    /// "state" section and `return validate` reports the state
    /// findings. Re-running replaces the selection.
    SetState {
        #[arg(long)]
        r#return: String,
        /// State code; when given it must match the file's "state".
        #[arg(long)]
        state: Option<String>,
        /// JSON file with the state block.
        #[arg(long)]
        file: PathBuf,
    },
    /// Mark the return as a nonresident-alien STUDENT return, from a
    /// JSON file: {"visa_type": "F-1", "citizenship": "CN",
    /// "first_calendar_year_in_status": 2024,
    /// "calendar_years_in_status_including_current": 2,
    /// "form_8843": {...}}. The return is then computed on Form
    /// 1040-NR instead of Form 1040. Students only (F-1 /
    /// J-1), single filing status, at most 5 calendar years in
    /// status — anything else is an explicit error. Incompatible with
    /// a state selection. Re-running replaces the block. The exact
    /// shape is in `tax schema NraBlock`.
    SetNra {
        #[arg(long)]
        r#return: String,
        /// JSON file with the nra block.
        #[arg(long)]
        file: PathBuf,
    },
    /// Record the direct-entry interest/dividend totals on an existing
    /// return (the post-creation counterpart of create's
    /// --interest-cents/--dividends-cents). The return cannot complete
    /// until these are recorded — even as 0 — so an unrecorded amount
    /// is never silently treated as zero. Re-running replaces the
    /// recorded totals.
    SetOtherIncome {
        #[arg(long)]
        r#return: String,
        /// Taxable interest, integer cents (e.g. 12500 = $125.00).
        #[arg(long)]
        interest_cents: i64,
        /// Ordinary dividends, integer cents.
        #[arg(long)]
        dividends_cents: i64,
    },
    /// List stored returns (PII-free: id, year, status, completeness,
    /// whether identity is set).
    List,
    /// Export a stored return. json, pdf, packet, and mef work.
    /// pdf requires --template (a blank TY2025 Form 1040
    /// PDF) and --out. packet requires --forms-dir (a directory holding
    /// the blank TY2025 form PDFs under their canonical names —
    /// f1040.pdf, f1040sd.pdf, f8949.pdf, f1040s1.pdf, f1040s2.pdf,
    /// f1040sc.pdf, f1040sse.pdf, f10408812.pdf, f1040s1a.pdf, plus
    /// f540.pdf when a California state block is selected) and
    /// --out-dir (the packet is written there, with a manifest.json).
    /// mef requires --out (the XML file to write) and a stored taxpayer
    /// identity (the ReturnHeader cannot be populated without one);
    /// it writes MeF-schema-shaped XML (TY2025v5.2 form documents;
    /// whole-dollar amounts; NOT transmissible — no e-file credentials
    /// exist in this tool, see docs/KNOWN_LIMITS.md).
    Export {
        #[arg(long)]
        r#return: String,
        #[arg(long = "type", default_value = "json")]
        export_type: String,
        #[arg(long)]
        template: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        forms_dir: Option<PathBuf>,
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum FormAction {
    /// Add one document entry from a JSON file, `--data`, or stdin.
    Add {
        #[arg(long)]
        r#return: String,
        #[arg(long)]
        node_type: String,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        data: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum DocAction {
    /// Classify a .txt file by its text, or a .pdf via text extraction.
    Classify {
        #[arg(long)]
        file: PathBuf,
    },
    /// Extract fields from a born-digital PDF (W-2, 1099-INT, 1099-DIV,
    /// 1099-NEC) into a `form add`-ready node, deterministically.
    Extract {
        #[arg(long)]
        file: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum PdfAction {
    /// List the AcroForm fields of a PDF (names decoded as PDF text strings).
    Inspect {
        /// PDF file to inspect.
        file: PathBuf,
    },
    /// Fill fields from a flat JSON object {qualified_name: value}.
    Fill {
        /// Input PDF.
        input: PathBuf,
        /// Where to write the filled PDF (never written on audit failure).
        #[arg(long)]
        out: PathBuf,
        /// JSON file with a flat {qualified_name: value} object of strings.
        /// Button/checkbox values are the on-state name, e.g. "1".
        #[arg(long)]
        data: PathBuf,
        /// Fill backend: pdfer (default, regenerates /AP, audited) or lopdf
        /// (/V only, no appearances — see docs/PDF_SPIKE.md).
        #[arg(long, default_value = "pdfer")]
        backend: String,
    },
}

#[derive(Subcommand, Debug)]
enum SkillsAction {
    /// List bundled skills found in the skills directory.
    List,
}

// ---------------------------------------------------------------------------
// Errors and output
// ---------------------------------------------------------------------------

struct CliErr {
    code: String,
    message: String,
    /// Extra JSON fields merged into the error object (e.g.
    /// `incomplete_lines` for `return_incomplete`).
    extra: Option<Value>,
}

impl CliErr {
    fn new(code: &str, message: impl Into<String>) -> CliErr {
        CliErr {
            code: code.to_string(),
            message: message.into(),
            extra: None,
        }
    }

    fn with_extra(code: &str, message: impl Into<String>, extra: Value) -> CliErr {
        CliErr {
            code: code.to_string(),
            message: message.into(),
            extra: Some(extra),
        }
    }
}

type CResult<T> = Result<T, CliErr>;

fn io_err(e: std::io::Error) -> CliErr {
    CliErr::new("io_error", e.to_string())
}
fn json_err(e: serde_json::Error) -> CliErr {
    CliErr::new("invalid_json", e.to_string())
}

struct Out {
    value: Value,
    human: String,
    exit_code: i32,
}

impl Out {
    fn ok(value: Value, human: String) -> Out {
        Out {
            value,
            human,
            exit_code: 0,
        }
    }
}

fn json_mode(cli: &Cli) -> bool {
    match cli.output {
        OutputMode::Json => true,
        OutputMode::Human => false,
        OutputMode::Auto => !std::io::stdout().is_terminal(),
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OtherIncome {
    interest: Money,
    dividends: Money,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEntry {
    entry_id: String,
    node_type: String,
    data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredReturn {
    id: String,
    year: u32,
    status: String,
    #[serde(default)]
    entries: Vec<StoredEntry>,
    #[serde(default)]
    other_income: Option<OtherIncome>,
    /// Taxpayer identity block (`return set-identity`). Optional:
    /// returns stored before identity existed load unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    taxpayer: Option<TaxpayerIdentity>,
    /// State return selection (`return set-state`). Optional:
    /// returns stored before state support existed load unchanged,
    /// and a return with no selection computes federally only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<StateBlock>,
    /// Nonresident-alien (student) selection (`return set-nra`).
    /// Optional: returns stored before NRA support existed load
    /// unchanged; when set, the return computes on Form 1040-NR
    /// instead of Form 1040.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nra: Option<NraBlock>,
}

fn return_path(store: &Path, id: &str) -> PathBuf {
    store.join("returns").join(id).join("return.json")
}

fn save_return(store: &Path, ret: &StoredReturn) -> CResult<()> {
    let path = return_path(store, &ret.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    let text = serde_json::to_string_pretty(ret).map_err(json_err)?;
    std::fs::write(&path, text).map_err(io_err)
}

fn load_return(store: &Path, id: &str) -> CResult<StoredReturn> {
    let path = return_path(store, id);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        CliErr::new(
            "return_not_found",
            format!("no return '{id}' in store {}", store.display()),
        )
    })?;
    serde_json::from_str(&text).map_err(json_err)
}

/// Record (or replace) the direct-entry interest/dividend totals on a
/// stored return — the post-creation path for what `return create`
/// takes as --interest-cents/--dividends-cents. Returns the updated
/// return and whether a previously recorded block was replaced.
/// Negative amounts are rejected: directly entered interest and
/// dividends cannot be negative (a loss is not interest income).
fn set_other_income(
    store: &Path,
    id: &str,
    interest_cents: i64,
    dividends_cents: i64,
) -> CResult<(StoredReturn, bool)> {
    if interest_cents < 0 || dividends_cents < 0 {
        return Err(CliErr::new(
            "invalid_input",
            "interest and dividend amounts cannot be negative",
        ));
    }
    let mut ret = load_return(store, id)?;
    let replaced = ret.other_income.is_some();
    ret.other_income = Some(OtherIncome {
        interest: Money::from_cents(interest_cents),
        dividends: Money::from_cents(dividends_cents),
    });
    save_return(store, &ret)?;
    Ok((ret, replaced))
}

/// Render one person's identity as JSON with the SSN MASKED
/// (`***-**-6789`). This is the only rendering of identity the CLI
/// ever prints: the full SSN appears only inside a filled PDF.
fn masked_person_json(p: &PersonIdentity) -> Value {
    json!({
        "first_name": p.first_name,
        "last_name": p.last_name,
        "ssn": p.ssn.as_deref().map(mask_ssn),
        "date_of_birth": p.date_of_birth,
        "occupation": p.occupation,
        "blind": p.blind,
    })
}

/// Render the stored identity block with every SSN masked.
fn masked_identity_json(identity: &TaxpayerIdentity) -> Value {
    let mut v = masked_person_json(&identity.person);
    v["address"] = match &identity.address {
        Some(a) => json!({
            "street": a.street, "apt": a.apt, "city": a.city,
            "state": a.state, "zip": a.zip,
        }),
        None => Value::Null,
    };
    v["spouse"] = match &identity.spouse {
        Some(s) => masked_person_json(s),
        None => Value::Null,
    };
    v["can_be_claimed_as_dependent"] = json!(identity.can_be_claimed_as_dependent);
    v
}

/// The `return list` rows for every return in the store: PII-free
/// (id, year, status, completeness, identity_set — never names or
/// SSNs). A return that fails to load or compute reports
/// `"complete": "error"` instead of failing the whole listing.
fn list_returns(store: &Path, params: &TaxParams) -> Vec<Value> {
    let dir = store.join("returns");
    let mut ids: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            if entry.path().join("return.json").exists() {
                ids.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    ids.sort();
    ids.iter()
        .map(|id| match load_return(store, id) {
            Ok(ret) => {
                let complete: Value = match compute_view(&ret, params) {
                    Ok(view) => json!(view.complete),
                    Err(_) => json!("error"),
                };
                let mut row = json!({
                    "id": ret.id,
                    "year": ret.year,
                    "status": ret.status,
                    "complete": complete,
                    "identity_set": ret.taxpayer.is_some(),
                    "state": ret.state.as_ref().map(|b| b.state.clone()),
                });
                if ret.nra.is_some() {
                    row["form"] = json!("1040-NR");
                }
                row
            }
            Err(_) => json!({
                "id": id,
                "year": Value::Null,
                "status": Value::Null,
                "complete": "error",
                "identity_set": false,
            }),
        })
        .collect()
}

fn stored_w2s(ret: &StoredReturn) -> CResult<Vec<W2Input>> {
    ret.entries
        .iter()
        .filter(|e| e.node_type == "w2")
        .map(|e| serde_json::from_value::<W2Input>(e.data.clone()).map_err(json_err))
        .collect()
}

fn stored_ints(ret: &StoredReturn) -> CResult<Vec<I1099IntInput>> {
    ret.entries
        .iter()
        .filter(|e| e.node_type == "1099-int")
        .map(|e| serde_json::from_value::<I1099IntInput>(e.data.clone()).map_err(json_err))
        .collect()
}

fn stored_divs(ret: &StoredReturn) -> CResult<Vec<I1099DivInput>> {
    ret.entries
        .iter()
        .filter(|e| e.node_type == "1099-div")
        .map(|e| serde_json::from_value::<I1099DivInput>(e.data.clone()).map_err(json_err))
        .collect()
}

fn stored_bs(ret: &StoredReturn) -> CResult<Vec<I1099BInput>> {
    ret.entries
        .iter()
        .filter(|e| e.node_type == "1099-b")
        .map(|e| serde_json::from_value::<I1099BInput>(e.data.clone()).map_err(json_err))
        .collect()
}

fn stored_necs(ret: &StoredReturn) -> CResult<Vec<I1099NecInput>> {
    ret.entries
        .iter()
        .filter(|e| e.node_type == "1099-nec")
        .map(|e| serde_json::from_value::<I1099NecInput>(e.data.clone()).map_err(json_err))
        .collect()
}

fn stored_1042ss(ret: &StoredReturn) -> CResult<Vec<F1042sInput>> {
    ret.entries
        .iter()
        .filter(|e| e.node_type == "1042-s")
        .map(|e| serde_json::from_value::<F1042sInput>(e.data.clone()).map_err(json_err))
        .collect()
}

fn stored_dependents(ret: &StoredReturn) -> CResult<Vec<DependentInput>> {
    ret.entries
        .iter()
        .filter(|e| e.node_type == "dependent")
        .map(|e| serde_json::from_value::<DependentInput>(e.data.clone()).map_err(json_err))
        .collect()
}

/// Aggregate the 1099-B entries of a partial return the same way the
/// engine does: (net_short, net_long, line_7, schd_net_capital_gain)
/// facts — Incomplete when any transaction is missing proceeds or
/// basis — plus the total 1099-B withholding. Line 7 and the QDCG gain
/// come from the same Schedule D combination the engine uses (loss
/// limit included). The carryover keys are NOT produced here: the
/// carryover worksheet needs taxable income, which a partial return
/// does not determine.
fn partial_b_nets(
    bs: &[I1099BInput],
    status: FilingStatus,
    params: &TaxParams,
) -> (
    FactResult<Money>,
    FactResult<Money>,
    FactResult<Money>,
    FactResult<Money>,
    Money,
) {
    let mut net_short = Money::ZERO;
    let mut net_long = Money::ZERO;
    let mut incomplete = false;
    for form in bs {
        for txn in &form.transactions {
            match txn.gain() {
                Some(g) => match txn.term {
                    tax_forms::TxnTerm::Short => net_short = net_short + g,
                    tax_forms::TxnTerm::Long => net_long = net_long + g,
                },
                None => incomplete = true,
            }
        }
    }
    let withholding: Money = bs.iter().map(|f| f.federal_withholding).sum();
    if incomplete {
        (
            FactResult::Incomplete,
            FactResult::Incomplete,
            FactResult::Incomplete,
            FactResult::Incomplete,
            withholding,
        )
    } else {
        let comb = tax_forms::combine_schedule_d(net_short, net_long, status, params);
        (
            FactResult::Complete(net_short),
            FactResult::Complete(net_long),
            FactResult::Complete(comb.line7),
            FactResult::Complete(comb.net_capital_gain),
            withholding,
        )
    }
}

// ---------------------------------------------------------------------------
// Computed view of a stored return
// ---------------------------------------------------------------------------

fn line_json(fr: &FactResult<Money>) -> Value {
    match fr {
        FactResult::Complete(v) => {
            json!({"state": "complete", "cents": v.cents(), "dollars": v.to_string()})
        }
        FactResult::Placeholder(v) => {
            json!({"state": "placeholder", "cents": v.cents(), "dollars": v.to_string()})
        }
        FactResult::Incomplete => json!({"state": "incomplete", "cents": null, "dollars": null}),
    }
}

struct ComputedView {
    lines: BTreeMap<String, FactResult<Money>>,
    complete: bool,
    incomplete: Vec<String>,
    warnings: Vec<String>,
    line16_method: Option<String>,
    /// The internal `schedules` view (packet derivations, integer
    /// cents). Only the full-engine path can build it; the partial
    /// views leave it `None` (serialized as null).
    schedules: Option<Value>,
    /// The computed state section (California Form 540 in v1),
    /// present only when the return has a state block stored.
    state: Option<StateView>,
    /// The computed Form 1040-NR section, present only when the
    /// return has an nra block stored. When present, `lines` IS the
    /// NR line map (the resident Form 1040 is not computed at all).
    nra: Option<NrView>,
}

/// The computed Form 1040-NR section of a stored return: the NR
/// lines in the same FactResult representation as the federal lines,
/// plus the determination, the Schedule OI data, and the engine's
/// findings for `return validate`.
struct NrView {
    block: NraBlock,
    lines: BTreeMap<String, FactResult<Money>>,
    notes: Vec<String>,
    findings: Vec<NrFinding>,
    tax_method: Option<String>,
    schedule_oi: Vec<ScheduleOiEntry>,
    exempt_individual: bool,
    treaty_modeled: bool,
}

/// The computed state section of a stored return: the state form's
/// lines in the same FactResult representation as the federal lines,
/// plus the engine's findings for `return validate`.
struct StateView {
    block: StateBlock,
    lines: BTreeMap<String, FactResult<Money>>,
    complete: bool,
    incomplete: Vec<String>,
    notes: Vec<String>,
    findings: Vec<CaFinding>,
    line31_method: Option<String>,
}

/// All line keys the current engine/view knows, in display order.
const ALL_LINES: &[&str] = &[
    "1a",
    "1z",
    "2a",
    "2b",
    "3a",
    "3b",
    "7",
    "schd_net_short",
    "schd_net_long",
    "schd_net_capital_gain",
    "schd_carryover_short",
    "schd_carryover_long",
    "8",
    "schc_net_profit",
    "9",
    "10",
    "schedse_net_earnings",
    "11",
    "12",
    "senior_bonus",
    "13b",
    "14",
    "15",
    "16",
    "sched8812_tentative",
    "sched8812_credit_after_phaseout",
    "sched8812_earned_income",
    "19",
    "21",
    "22",
    "23",
    "sched2_se_tax",
    "24",
    "25a",
    "25d",
    "27",
    "eic_earned_income",
    "eic_qualifying_children",
    "eic_investment_income",
    "28",
    "32",
    "33",
    "34",
    "37",
];

fn incomplete_lines(lines: &BTreeMap<String, FactResult<Money>>) -> Vec<String> {
    lines
        .iter()
        .filter(|(_, f)| matches!(f, FactResult::Incomplete))
        .map(|(k, _)| k.clone())
        .collect()
}

/// The full computed view of a stored return: the federal view from
/// [`compute_view_inner`], plus — when the return has a state block
/// stored — the computed state section. The overall `complete` is
/// the conjunction: a return whose federal side is complete but
/// whose California section is not is not complete.
fn compute_view(ret: &StoredReturn, params: &TaxParams) -> CResult<ComputedView> {
    if let Some(block) = &ret.nra {
        return compute_view_nr(ret, block, params);
    }
    let mut view = compute_view_inner(ret, params)?;
    if let Some(block) = &ret.state {
        block
            .validated()
            .map_err(|e| CliErr::new("unsupported_state", e))?;
        let ca_params = CaParams::load_2025().map_err(json_err)?;
        let status =
            FilingStatus::parse(&ret.status).map_err(|e| CliErr::new("invalid_status", e))?;
        // The California engine reads the direct interest/dividend
        // amounts only through the federal facts (line 11 AGI, the
        // EIC investment-income aggregate), never directly, so the
        // assembled input may carry zeros for them: on the partial
        // path those federal facts are Incomplete in the view's line
        // map and every dependent California line follows.
        let input = Form1040Input {
            status,
            w2s: stored_w2s(ret)?,
            interest_income: Money::ZERO,
            ordinary_dividends: Money::ZERO,
            f1099_ints: stored_ints(ret)?,
            f1099_divs: stored_divs(ret)?,
            f1099_bs: stored_bs(ret)?,
            f1099_necs: stored_necs(ret)?,
            f1042ss: stored_1042ss(ret)?,
            dependents: stored_dependents(ret)?,
            identity: ret.taxpayer.clone(),
            state: Some(block.clone()),
            nra: ret.nra.clone(),
        };
        let fed = ComputedReturn {
            lines: view.lines.clone(),
            complete: view.complete,
            notes: Vec::new(),
            line16_method: view.line16_method.clone(),
        };
        let ca = compute_ca540(&input, &fed, params, &ca_params);
        view.complete = view.complete && ca.complete;
        view.state = Some(StateView {
            block: block.clone(),
            lines: ca.lines,
            complete: ca.complete,
            incomplete: ca.incomplete,
            notes: ca.notes,
            findings: ca.findings,
            line31_method: ca.line31_method,
        });
    }
    Ok(view)
}

/// The computed view of a nonresident-alien student return: the
/// Form 1040-NR engine INSTEAD of the resident engine. A state block
/// alongside the nra block is an explicit error (the California
/// selection is a full-year-resident Form 540; California does not
/// honor federal treaties, and Form 540NR is not implemented).
fn compute_view_nr(
    ret: &StoredReturn,
    block: &NraBlock,
    params: &TaxParams,
) -> CResult<ComputedView> {
    if ret.state.is_some() {
        return Err(CliErr::new(
            "nra_state_conflict",
            "this return has both a state selection and an nra block: the California \
             selection is a full-year-resident Form 540, which is incompatible with the \
             Form 1040-NR route (California does not honor federal tax treaties and \
             taxes nonresidents on Form 540NR, which v1 does not implement)",
        ));
    }
    let treaties = TreatyParams::load_2025().map_err(json_err)?;
    let determination = block
        .determination(ret.year, &treaties)
        .map_err(|e| CliErr::new("nra_determination_failed", e))?;
    let status = FilingStatus::parse(&ret.status).map_err(|e| CliErr::new("invalid_status", e))?;
    let (interest, dividends) = match &ret.other_income {
        Some(o) => (o.interest, o.dividends),
        None => (Money::ZERO, Money::ZERO),
    };
    let input = Form1040Input {
        status,
        w2s: stored_w2s(ret)?,
        interest_income: interest,
        ordinary_dividends: dividends,
        f1099_ints: stored_ints(ret)?,
        f1099_divs: stored_divs(ret)?,
        f1099_bs: stored_bs(ret)?,
        f1099_necs: stored_necs(ret)?,
        f1042ss: stored_1042ss(ret)?,
        dependents: stored_dependents(ret)?,
        identity: ret.taxpayer.clone(),
        state: None,
        nra: Some(block.clone()),
    };
    let nr = compute_1040nr(&input, params, &treaties);
    let mut warnings = nr.notes.clone();
    let (lines, complete) = if ret.entries.is_empty() {
        // Mirror the federal view's rule: an untouched return is not
        // "complete with zeros" — no documents have been recorded.
        warnings.push("no documents have been added to this return".to_string());
        (
            nr.lines
                .keys()
                .map(|k| (k.clone(), FactResult::Incomplete))
                .collect::<BTreeMap<_, _>>(),
            false,
        )
    } else {
        (nr.lines.clone(), nr.complete)
    };
    let incomplete = incomplete_lines(&lines);
    Ok(ComputedView {
        lines,
        complete,
        incomplete,
        warnings,
        line16_method: nr.tax_method.clone(),
        schedules: None,
        state: None,
        nra: Some(NrView {
            block: block.clone(),
            lines: nr.lines,
            notes: nr.notes,
            findings: nr.findings,
            tax_method: nr.tax_method,
            schedule_oi: nr.schedule_oi,
            exempt_individual: determination.exempt_individual,
            treaty_modeled: determination.treaty_modeled,
        }),
    })
}

fn compute_view_inner(ret: &StoredReturn, params: &TaxParams) -> CResult<ComputedView> {
    let w2s = stored_w2s(ret)?;
    let ints = stored_ints(ret)?;
    let divs = stored_divs(ret)?;
    let bs = stored_bs(ret)?;
    let necs = stored_necs(ret)?;
    let deps = stored_dependents(ret)?;
    let status = FilingStatus::parse(&ret.status).map_err(|e| CliErr::new("invalid_status", e))?;
    let mut warnings = Vec::new();

    if ret.entries.is_empty() && ret.other_income.is_none() {
        let lines: BTreeMap<String, FactResult<Money>> = ALL_LINES
            .iter()
            .map(|l| (l.to_string(), FactResult::Incomplete))
            .collect();
        return Ok(ComputedView {
            incomplete: incomplete_lines(&lines),
            lines,
            complete: false,
            warnings: vec!["no documents have been added to this return".to_string()],
            line16_method: None,
            schedules: None,
            state: None,
            nra: None,
        });
    }

    match &ret.other_income {
        Some(other) => {
            let input = Form1040Input {
                status,
                w2s,
                interest_income: other.interest,
                ordinary_dividends: other.dividends,
                f1099_ints: ints,
                f1099_divs: divs,
                f1099_bs: bs,
                f1099_necs: necs,
                f1042ss: stored_1042ss(ret)?,
                dependents: deps,
                identity: ret.taxpayer.clone(),
                state: ret.state.clone(),
                nra: ret.nra.clone(),
            };
            let computed = compute_1040(&input, params);
            warnings.extend(computed.notes.iter().cloned());
            let schedules = schedules_view(&input, &computed, params);
            Ok(ComputedView {
                incomplete: incomplete_lines(&computed.lines),
                complete: computed.complete,
                lines: computed.lines,
                warnings,
                line16_method: computed.line16_method.clone(),
                schedules: Some(schedules),
                state: None,
                nra: None,
            })
        }
        None => {
            // Documents are known; the *direct* interest/dividend amounts
            // are NOT recorded, so every line that depends on them is
            // Incomplete — never zero-filled. Lines that depend only on
            // stored documents are computed from them.
            let wages: Money = w2s.iter().map(|w| w.wages).sum();
            let w2_withholding: Money = w2s.iter().map(|w| w.federal_withholding).sum();
            // Line 12 stays computable in the partial view: the base
            // deduction plus the identity's age-65/blind additions.
            let deduction = params.standard_deduction(status)
                + additional_standard_deduction(ret.taxpayer.as_ref(), status, params);
            let mut lines: BTreeMap<String, FactResult<Money>> = BTreeMap::new();
            lines.insert("1a".into(), FactResult::Complete(wages));
            lines.insert("1z".into(), FactResult::Complete(wages));
            lines.insert("12".into(), FactResult::Complete(deduction));
            // Schedule 1-A, Part V (line 13b): eligibility depends
            // only on the stored identity, so with no eligible person
            // the deduction is a stated zero and line 14 = line 12.
            // With an eligible person, the phaseout needs AGI, which
            // the partial view does not determine — 13b/14 stay
            // Incomplete (filled in by the ALL_LINES pass below),
            // exactly as the full engine treats them.
            if senior_bonus_eligible_persons(ret.taxpayer.as_ref(), status, params) == 0 {
                lines.insert("senior_bonus".into(), FactResult::Complete(Money::ZERO));
                lines.insert("13b".into(), FactResult::Complete(Money::ZERO));
                lines.insert("14".into(), FactResult::Complete(deduction));
            }
            lines.insert("25a".into(), FactResult::Complete(w2_withholding));
            // The Schedule 8812 earned-income key depends only on
            // stored W-2 / 1099-NEC entries, so it is computable here
            // exactly as the engine computes it.
            let ei_nec = if necs.is_empty() {
                Money::ZERO
            } else {
                schedule_c_net_profit(&necs)
            };
            let earned_income = wages + Money::from_cents(ei_nec.cents().max(0));
            lines.insert(
                "sched8812_earned_income".into(),
                FactResult::Complete(earned_income),
            );
            // The EIC earned-income and qualifying-children keys also
            // depend only on stored W-2 / 1099-NEC / dependent
            // entries, so they are computable here exactly as the
            // engine computes them (EIC Worksheet B: wages + Schedule
            // C net − the deductible part of SE tax).
            let schc_net_all = schedule_c_net_profit(&necs);
            let se_all = schedule_se_tax(schc_net_all, wages, params);
            let eic_earned = wages + schc_net_all - deductible_part_of_se_tax(se_all.tax);
            lines.insert("eic_earned_income".into(), FactResult::Complete(eic_earned));
            let eic_children = eic_qualifying_children(&deps);
            lines.insert(
                "eic_qualifying_children".into(),
                FactResult::Complete(Money::from_cents(i64::from(eic_children) * 100)),
            );
            // With no EIC qualifying children, the childless credit
            // could still apply when a date of birth on file passes
            // the age-25-to-64 test: lines 27/32 (and 33 through 32)
            // then depend on AGI, which the partial view does not
            // determine — they stay Incomplete rather than stated
            // zeros. Otherwise line 27 is a stated zero (the engine's
            // childless rule: an undeterminable age test takes no
            // credit).
            let childless_possible = deps.is_empty()
                && status != FilingStatus::Mfs
                && eic_childless_age_outcome(ret.taxpayer.as_ref(), status, params) == Some(true);
            if deps.is_empty() {
                // No dependents stored: the tentative credit is zero
                // whatever the income, so these credit lines are stated
                // zeros. Lines 22/24 still need line 16 and stay
                // Incomplete (filled in by the ALL_LINES pass below).
                for l in [
                    "sched8812_tentative",
                    "sched8812_credit_after_phaseout",
                    "19",
                    "21",
                    "28",
                ] {
                    lines.insert(l.into(), FactResult::Complete(Money::ZERO));
                }
                if !childless_possible {
                    lines.insert("27".into(), FactResult::Complete(Money::ZERO));
                    lines.insert("32".into(), FactResult::Complete(Money::ZERO));
                }
            }
            if !ints.is_empty() {
                let exempt: Money = ints.iter().map(|f| f.tax_exempt_interest).sum();
                lines.insert("2a".into(), FactResult::Complete(exempt));
            }
            if !divs.is_empty() {
                let qualified: Money = divs.iter().map(|f| f.qualified_dividends).sum();
                lines.insert("3a".into(), FactResult::Complete(qualified));
            }
            let mut b_withholding = Money::ZERO;
            if !bs.is_empty() {
                let (ns, nl, cap, gain, wh) = partial_b_nets(&bs, status, params);
                lines.insert("schd_net_short".into(), ns);
                lines.insert("schd_net_long".into(), nl);
                lines.insert("7".into(), cap);
                lines.insert("schd_net_capital_gain".into(), gain);
                b_withholding = wh;
            }
            let mut nec_withholding = Money::ZERO;
            if !necs.is_empty() {
                // The self-employment lines depend only on stored NEC
                // and W-2 entries, so they are computable here exactly
                // as the engine computes them. Line 24 stays Incomplete:
                // it also needs line 16, which needs total income.
                let schc_net = schedule_c_net_profit(&necs);
                let se = schedule_se_tax(schc_net, wages, params);
                lines.insert("schc_net_profit".into(), FactResult::Complete(schc_net));
                lines.insert("8".into(), FactResult::Complete(schc_net));
                lines.insert(
                    "schedse_net_earnings".into(),
                    FactResult::Complete(se.net_earnings),
                );
                lines.insert("sched2_se_tax".into(), FactResult::Complete(se.tax));
                lines.insert("23".into(), FactResult::Complete(se.tax));
                lines.insert(
                    "10".into(),
                    FactResult::Complete(deductible_part_of_se_tax(se.tax)),
                );
                nec_withholding = necs.iter().map(|f| f.federal_withholding).sum();
            }
            if !ints.is_empty() || !divs.is_empty() || !bs.is_empty() || !necs.is_empty() {
                // Withholding on every stored document is recorded, so
                // the withholding totals are computable from the stored set.
                let int_wh: Money = ints.iter().map(|f| f.federal_withholding).sum();
                let div_wh: Money = divs.iter().map(|f| f.federal_withholding).sum();
                let total_wh = w2_withholding + int_wh + div_wh + b_withholding + nec_withholding;
                lines.insert("25d".into(), FactResult::Complete(total_wh));
                if !childless_possible {
                    lines.insert("33".into(), FactResult::Complete(total_wh));
                }
            }
            if !deps.is_empty() {
                // Dependents are stored, but the credits need AGI and
                // line 16, which a partial return does not determine:
                // every credit line — and line 33, which adds line 32 —
                // is Incomplete here, never zero-filled.
                for l in [
                    "sched8812_tentative",
                    "sched8812_credit_after_phaseout",
                    "19",
                    "21",
                    "22",
                    "27",
                    "28",
                    "32",
                    "33",
                ] {
                    lines.insert(l.into(), FactResult::Incomplete);
                }
            }
            for l in ALL_LINES {
                lines.entry(l.to_string()).or_insert(FactResult::Incomplete);
            }
            warnings.push(
                "interest_income and ordinary_dividends have not been recorded for this \
                 return (create it with --interest-cents/--dividends-cents, record them \
                 with `return set-other-income`, or supply the full Form1040Input to \
                 the engine); dependent lines are incomplete, not zero"
                    .to_string(),
            );
            Ok(ComputedView {
                incomplete: incomplete_lines(&lines),
                lines,
                complete: false,
                warnings,
                line16_method: None,
                schedules: None,
                state: None,
                nra: None,
            })
        }
    }
}

fn view_json(view: &ComputedView) -> Value {
    let lines: serde_json::Map<String, Value> = view
        .lines
        .iter()
        .map(|(k, v)| (k.clone(), line_json(v)))
        .collect();
    let mut value = json!({
        "lines": lines,
        "complete": view.complete,
        "incomplete": view.incomplete,
        "warnings": view.warnings,
        "line16_method": view.line16_method,
        "schedules": view.schedules,
    });
    if let Some(sv) = &view.state {
        let state_lines: serde_json::Map<String, Value> = sv
            .lines
            .iter()
            .map(|(k, v)| (k.clone(), line_json(v)))
            .collect();
        value["state"] = json!({
            "state": sv.block.state,
            "residency": sv.block.residency,
            "complete": sv.complete,
            "lines": state_lines,
            "incomplete": sv.incomplete,
            "line31_method": sv.line31_method,
            "notes": sv.notes,
        });
    }
    if let Some(nv) = &view.nra {
        // The return computes on Form 1040-NR: "lines" above IS the
        // NR line map, and this object carries the route's metadata.
        // Money in schedule_oi serializes as integer cents strings,
        // like everywhere else in the CLI.
        value["form"] = json!("1040-NR");
        value["nra"] = json!({
            "visa_type": nv.block.visa_type,
            "citizenship": nv.block.citizenship,
            "first_calendar_year_in_status": nv.block.first_calendar_year_in_status,
            "calendar_years_in_status_including_current":
                nv.block.calendar_years_in_status_including_current,
            "form_8843_stored": nv.block.form_8843.is_some(),
            "exempt_individual": nv.exempt_individual,
            "treaty_modeled": nv.treaty_modeled,
            "tax_method": nv.tax_method,
            "schedule_oi": serde_json::to_value(&nv.schedule_oi).unwrap_or(Value::Null),
            "notes": nv.notes,
        });
    }
    value
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn cmd_doctor(cli: &Cli) -> CResult<Out> {
    let mut checks = Vec::new();
    let mut push = |name: &str, ok: bool, required: bool, detail: String, hint: &str| {
        checks.push(
            json!({"name": name, "ok": ok, "required": required, "detail": detail, "hint": hint}),
        );
    };

    push(
        "version",
        true,
        true,
        format!("tax {}", env!("CARGO_PKG_VERSION")),
        "",
    );
    match TaxParams::load_2025() {
        Ok(p) => push(
            "params_2025",
            true,
            true,
            format!("TY{} parameters loaded ({})", p.year, p.source),
            "",
        ),
        Err(e) => push(
            "params_2025",
            false,
            true,
            e.to_string(),
            "reinstall opentax; params/2025.json is embedded at build time",
        ),
    }
    match CaParams::load_2025() {
        Ok(p) => push(
            "ca_params_2025",
            true,
            true,
            format!(
                "TY{} California parameters loaded ({} CA tax-table rows, {} CA EITC-table rows)",
                p.year,
                tax_core::CaTaxTable::load_2025().rows.len(),
                tax_core::CaEitcTable::load_2025().rows.len()
            ),
            "",
        ),
        Err(_) => push(
            "ca_params_2025",
            false,
            true,
            "California params or the extracted CA Tax / EITC tables failed to load".to_string(),
            "reinstall opentax; params/2025-ca*.json are embedded at build time",
        ),
    }
    match TreatyParams::load_2025() {
        Ok(p) => push(
            "treaty_params_2025",
            true,
            true,
            format!(
                "TY{} treaty parameters loaded ({} countries modeled)",
                p.year,
                p.countries.len()
            ),
            "",
        ),
        Err(e) => push(
            "treaty_params_2025",
            false,
            true,
            e.to_string(),
            "reinstall opentax; params/2025-treaties.json is embedded at build time",
        ),
    }
    // Store writable probe.
    let store_ok = (|| -> std::io::Result<()> {
        let dir = cli.store.join("returns");
        std::fs::create_dir_all(&dir)?;
        let probe = dir.join(".probe");
        std::fs::write(&probe, b"ok")?;
        std::fs::remove_file(&probe)?;
        Ok(())
    })();
    match store_ok {
        Ok(()) => push(
            "store_writable",
            true,
            true,
            cli.store.display().to_string(),
            "",
        ),
        Err(e) => push(
            "store_writable",
            false,
            true,
            e.to_string(),
            "pick a writable --store directory",
        ),
    }
    push(
        "pdf_backend",
        true,
        true,
        format!(
            "inspect: {} (lopdf); fill: {} (pdfer_forms, regenerates /AP, audited) — \
             see `tax pdf inspect|fill`",
            LopdfBackend.name(),
            PdferBackend.name()
        ),
        "",
    );
    let ocr = tax_doc::NoOcr;
    push(
        "ocr_backend",
        false,
        false,
        format!(
            "{} — OCR is an optional fallback, not installed in v1",
            tax_doc::OcrBackend::name(&ocr)
        ),
        "scanned documents use the agent-vision path in the tax-ingest skill",
    );

    let ok = checks
        .iter()
        .all(|c| !c["required"].as_bool().unwrap_or(false) || c["ok"].as_bool().unwrap_or(false));
    let human = checks
        .iter()
        .map(|c| {
            format!(
                "{} {} — {}",
                if c["ok"].as_bool().unwrap_or(false) {
                    "ok  "
                } else {
                    "FAIL"
                },
                c["name"].as_str().unwrap_or(""),
                c["detail"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Out::ok(json!({"ok": ok, "checks": checks}), human))
}

const GOLDEN_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/001-single-w2-minimal/input.json");
const GOLDEN_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/001-single-w2-minimal/correct.json");
const CASE002_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/002-w2-1099s/input.json");
const CASE002_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/002-w2-1099s/correct.json");
const CASE003_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/003-capital-loss-limit/input.json");
const CASE003_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/003-capital-loss-limit/correct.json");
const CASE004_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/004-qdcg-showcase/input.json");
const CASE004_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/004-qdcg-showcase/correct.json");
const CASE005_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/005-w2-nec/input.json");
const CASE005_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/005-w2-nec/correct.json");
const CASE006_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/006-family-mfj/input.json");
const CASE006_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/006-family-mfj/correct.json");
const CASE007_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/007-actc-hoh/input.json");
const CASE007_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/007-actc-hoh/correct.json");
const CASE008_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/008-identity-65plus/input.json");
const CASE008_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/008-identity-65plus/correct.json");
const CASE009_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/009-eic-two-kids/input.json");
const CASE009_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/009-eic-two-kids/correct.json");
const CASE010_INPUT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/010-senior-phaseout/input.json");
const CASE010_CORRECT: &str =
    include_str!("../../../benchmark/cases/f1040/2025/010-senior-phaseout/correct.json");

/// (case directory name, embedded input.json, embedded correct.json).
const EMBEDDED_CASES: &[(&str, &str, &str)] = &[
    ("001-single-w2-minimal", GOLDEN_INPUT, GOLDEN_CORRECT),
    ("002-w2-1099s", CASE002_INPUT, CASE002_CORRECT),
    ("003-capital-loss-limit", CASE003_INPUT, CASE003_CORRECT),
    ("004-qdcg-showcase", CASE004_INPUT, CASE004_CORRECT),
    ("005-w2-nec", CASE005_INPUT, CASE005_CORRECT),
    ("006-family-mfj", CASE006_INPUT, CASE006_CORRECT),
    ("007-actc-hoh", CASE007_INPUT, CASE007_CORRECT),
    ("008-identity-65plus", CASE008_INPUT, CASE008_CORRECT),
    ("009-eic-two-kids", CASE009_INPUT, CASE009_CORRECT),
    ("010-senior-phaseout", CASE010_INPUT, CASE010_CORRECT),
];

fn run_golden(input_text: &str, correct_text: &str) -> CResult<(bool, Vec<String>)> {
    let input: Form1040Input = serde_json::from_str(input_text).map_err(json_err)?;
    let correct: Value = serde_json::from_str(correct_text).map_err(json_err)?;
    let params = TaxParams::load_2025().map_err(json_err)?;
    let mut mismatches = Vec::new();
    // Nonresident-alien cases: correct.json carries an "nr_lines"
    // object of Form 1040-NR line -> expected cents, derived from
    // the IRS sources cited in the case. The input carries an "nra"
    // block; the resident engine is not run at all.
    if let Some(nr_lines) = correct.get("nr_lines").and_then(|l| l.as_object()) {
        if input.nra.is_none() {
            mismatches.push(
                "correct.json asserts nr_lines but input.json has no \"nra\" block".to_string(),
            );
        }
        let treaties = TreatyParams::load_2025().map_err(json_err)?;
        let nr = compute_1040nr(&input, &params, &treaties);
        for (line, expected) in nr_lines {
            let Some(expected_cents) = expected.as_i64() else {
                continue;
            };
            match nr.lines.get(line) {
                Some(fr) => match fr.value() {
                    Some(v) if v.cents() == expected_cents => {}
                    Some(v) => mismatches.push(format!(
                        "nr line {line}: expected {expected_cents} cents, got {} cents",
                        v.cents()
                    )),
                    None => mismatches.push(format!(
                        "nr line {line}: expected {expected_cents} cents, got incomplete"
                    )),
                },
                None => mismatches.push(format!("nr line {line}: not computed")),
            }
        }
        if let Some(expected_method) = correct.get("nr_tax_method").and_then(|m| m.as_str()) {
            match nr.tax_method.as_deref() {
                Some(actual) if actual == expected_method => {}
                other => mismatches.push(format!(
                    "nr_tax_method: expected {expected_method}, got {}",
                    other.unwrap_or("<none>")
                )),
            }
        }
        return Ok((mismatches.is_empty(), mismatches));
    }
    let computed = compute_1040(&input, &params);
    if let Some(lines) = correct.get("lines").and_then(|l| l.as_object()) {
        for (line, expected) in lines {
            let Some(expected_cents) = expected.as_i64() else {
                continue;
            };
            match computed.lines.get(line) {
                Some(fr) => match fr.value() {
                    Some(v) if v.cents() == expected_cents => {}
                    Some(v) => mismatches.push(format!(
                        "line {line}: expected {expected_cents} cents, got {} cents",
                        v.cents()
                    )),
                    None => mismatches.push(format!(
                        "line {line}: expected {expected_cents} cents, got incomplete"
                    )),
                },
                None => mismatches.push(format!("line {line}: not computed")),
            }
        }
    } else {
        mismatches.push("correct.json has no \"lines\" object".to_string());
    }
    if let Some(expected_method) = correct.get("line16_method").and_then(|m| m.as_str()) {
        match computed.line16_method.as_deref() {
            Some(actual) if actual == expected_method => {}
            other => mismatches.push(format!(
                "line16_method: expected {expected_method}, got {}",
                other.unwrap_or("<none>")
            )),
        }
    }
    // California cases: correct.json carries a "state_lines" object
    // of Form 540 line -> expected cents, derived from FTB sources.
    // The federal assertions above still run (the state section is
    // computed FROM the federal return).
    if let Some(state_lines) = correct.get("state_lines").and_then(|l| l.as_object()) {
        if input.state.is_none() {
            mismatches.push(
                "correct.json asserts state_lines but input.json has no \"state\" block"
                    .to_string(),
            );
        } else {
            let ca_params = CaParams::load_2025().map_err(json_err)?;
            let ca = compute_ca540(&input, &computed, &params, &ca_params);
            for (line, expected) in state_lines {
                let Some(expected_cents) = expected.as_i64() else {
                    continue;
                };
                match ca.lines.get(line) {
                    Some(fr) => match fr.value() {
                        Some(v) if v.cents() == expected_cents => {}
                        Some(v) => mismatches.push(format!(
                            "ca line {line}: expected {expected_cents} cents, got {} cents",
                            v.cents()
                        )),
                        None => mismatches.push(format!(
                            "ca line {line}: expected {expected_cents} cents, got incomplete"
                        )),
                    },
                    None => mismatches.push(format!("ca line {line}: not computed")),
                }
            }
            if let Some(expected_method) = correct.get("ca_line31_method").and_then(|m| m.as_str())
            {
                match ca.line31_method.as_deref() {
                    Some(actual) if actual == expected_method => {}
                    other => mismatches.push(format!(
                        "ca_line31_method: expected {expected_method}, got {}",
                        other.unwrap_or("<none>")
                    )),
                }
            }
        }
    }
    Ok((mismatches.is_empty(), mismatches))
}

fn cmd_selftest() -> CResult<Out> {
    // Prefer the on-disk benchmark tree (so edited cases are what run):
    // every directory under benchmark/cases/f1040/2025/ containing an
    // input.json + correct.json pair is a case. Fall back to the copies
    // embedded at build time when the tree is absent.
    let base = Path::new("benchmark/cases/f1040/2025");
    let mut cases: Vec<(String, String, String, String)> = Vec::new(); // (name, input, correct, origin)
    if base.is_dir() {
        let mut names: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(base).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let dir = entry.path();
            if dir.is_dir() && dir.join("input.json").exists() && dir.join("correct.json").exists()
            {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        for name in names {
            let dir = base.join(&name);
            cases.push((
                name,
                std::fs::read_to_string(dir.join("input.json")).map_err(io_err)?,
                std::fs::read_to_string(dir.join("correct.json")).map_err(io_err)?,
                dir.display().to_string(),
            ));
        }
    }
    if cases.is_empty() {
        for (name, input, correct) in EMBEDDED_CASES {
            cases.push((
                name.to_string(),
                input.to_string(),
                correct.to_string(),
                format!("embedded benchmark/cases/f1040/2025/{name}"),
            ));
        }
    }

    // California cases live in their own tree (benchmark/cases/
    // ca540/2025/<case>/) with the same input.json + correct.json
    // shape; the input carries a "state" block and correct.json
    // asserts "state_lines". On-disk only (no embedded fallback).
    let ca_base = Path::new("benchmark/cases/ca540/2025");
    if ca_base.is_dir() {
        let mut names: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(ca_base).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let dir = entry.path();
            if dir.is_dir() && dir.join("input.json").exists() && dir.join("correct.json").exists()
            {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        for name in names {
            let dir = ca_base.join(&name);
            cases.push((
                format!("ca540:{name}"),
                std::fs::read_to_string(dir.join("input.json")).map_err(io_err)?,
                std::fs::read_to_string(dir.join("correct.json")).map_err(io_err)?,
                dir.display().to_string(),
            ));
        }
    }

    // Nonresident-alien student cases live in their own tree
    // (benchmark/cases/f1040nr/2025/<case>/); the input carries an
    // "nra" block and correct.json asserts "nr_lines". On-disk only.
    let nr_base = Path::new("benchmark/cases/f1040nr/2025");
    if nr_base.is_dir() {
        let mut names: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(nr_base).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let dir = entry.path();
            if dir.is_dir() && dir.join("input.json").exists() && dir.join("correct.json").exists()
            {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        for name in names {
            let dir = nr_base.join(&name);
            cases.push((
                format!("f1040nr:{name}"),
                std::fs::read_to_string(dir.join("input.json")).map_err(io_err)?,
                std::fs::read_to_string(dir.join("correct.json")).map_err(io_err)?,
                dir.display().to_string(),
            ));
        }
    }

    let mut results: Vec<Value> = Vec::new();
    let mut all_pass = true;
    let mut all_mismatches: Vec<String> = Vec::new();
    for (name, input, correct, origin) in &cases {
        let (pass, mismatches) = run_golden(input, correct)?;
        let label = if let Some(n) = name.strip_prefix("ca540:") {
            format!("ca540/2025/{n}")
        } else if let Some(n) = name.strip_prefix("f1040nr:") {
            format!("f1040nr/2025/{n}")
        } else {
            format!("f1040/2025/{name}")
        };
        if !pass {
            all_pass = false;
            all_mismatches.extend(
                mismatches
                    .iter()
                    .map(|m| format!("{label}: {m}"))
                    .collect::<Vec<_>>(),
            );
        }
        results.push(json!({
            "case": label,
            "origin": origin,
            "pass": pass,
            "mismatches": mismatches,
        }));
    }
    let human = if all_pass {
        format!(
            "selftest: {}/{} golden cases pass",
            results.len(),
            results.len()
        )
    } else {
        format!("selftest: FAIL\n{}", all_mismatches.join("\n"))
    };
    Ok(Out {
        value: json!({"cases": results, "pass": all_pass}),
        human,
        exit_code: if all_pass { 0 } else { 1 },
    })
}

fn schema_json() -> Value {
    json!({
        "schema_version": 1,
        "note": "Static scaffold schema, hand-maintained and versioned; generation from the Rust structs via schemars replaces this later. Money is integer cents as a string (e.g. \"5000000\" = $50,000.00); floats are never accepted for money.",
        "inputs": {
            "Form1040Input": {
                "status": {"type": "enum", "values": ["single", "mfj", "hoh", "mfs"]},
                "w2s": {"type": "array", "items": "W2Input"},
                "interest_income": {"type": "money_cents_string"},
                "ordinary_dividends": {"type": "money_cents_string"},
                "f1099_ints": {"type": "array", "items": "I1099IntInput"},
                "f1099_divs": {"type": "array", "items": "I1099DivInput"},
                "f1099_bs": {"type": "array", "items": "I1099BInput"},
                "f1099_necs": {"type": "array", "items": "I1099NecInput"},
                "dependents": {"type": "array", "items": "DependentInput"},
                "identity": {"type": "object", "items": "TaxpayerIdentity", "optional": true, "note": "the same block `return set-identity` stores; drives the age-65/blind standard-deduction additions, the Schedule 1-A Part V enhanced deduction for seniors (line 13b), and the PDF identity fields"},
                "state": {"type": "object", "items": "StateBlock", "optional": true, "note": "the same block `return set-state` stores; when set, the state section is computed alongside the federal return and `return get` gains a \"state\" object. Absent = federal only, computed exactly as before"},
                "f1042ss": {"type": "array", "items": "F1042sInput", "optional": true, "note": "Form 1042-S entries (foreign person's U.S. source income); consumed by the Form 1040-NR section when an \"nra\" block is set — the resident engine ignores them in v1"},
                "nra": {"type": "object", "items": "NraBlock", "optional": true, "note": "the same block `return set-nra` stores; when set, `return get` computes Form 1040-NR INSTEAD of the resident Form 1040: \"lines\" is the NR line map and the object gains \"form\": \"1040-NR\" and an \"nra\" object (determination, tax method, Schedule OI data, notes). Absent = resident behavior exactly as before. Incompatible with a StateBlock (explicit error)"}
            },
            "NraBlock": {
                "note": "Set with `tax return set-nra --return <id> --file nra.json`. v1 is the STUDENT route: F-1 / J-1 visa holders in their exempt-individual years (at most 5 calendar years in status — any part of a calendar year counts, Pub 519 (2025)), filing status single. More than 5 years, another visa, or another filing status is refused with an explicit error (likely resident alien; dual-status not implemented). Citizenship CN / IN carry treaty modeling (params/2025-treaties.json); any other country computes with NO treaty benefits and a treaty_not_modeled warning.",
                "visa_type": {"type": "string", "values": ["F-1", "J-1"]},
                "citizenship": {"type": "string", "note": "ISO 3166-1 alpha-2 country code (CN, IN modeled); case-insensitive, stored uppercase"},
                "first_calendar_year_in_status": {"type": "integer"},
                "calendar_years_in_status_including_current": {"type": "integer", "minimum": 1, "maximum": 5},
                "form_8843": {"type": "object", "items": "Form8843Data", "optional": true, "note": "facts for Form 8843, stored verbatim for the forms lane; optional individually and as a whole — absent facts are reported incomplete, never invented"}
            },
            "Form8843Data": {
                "school_name": {"type": "string", "optional": true},
                "school_address": {"type": "string", "optional": true},
                "school_phone": {"type": "string", "optional": true},
                "director_name": {"type": "string", "optional": true},
                "director_address": {"type": "string", "optional": true},
                "director_phone": {"type": "string", "optional": true},
                "days_present_current_year": {"type": "integer", "optional": true},
                "days_present_prior_year_1": {"type": "integer", "optional": true},
                "days_present_prior_year_2": {"type": "integer", "optional": true}
            },
            "F1042sInput": {
                "note": "One Form 1042-S as printed. Money is integer cents as strings. Income code 20 (compensation during studying and training) and 15 (scholarship or fellowship grants) are modeled on the Form 1040-NR route; any other code is recorded but makes the NR section incomplete with a loud finding. exemption_code is recorded verbatim (\"04\" = exempt under a tax treaty, Instructions for Form 1042-S); an exemption_code of \"04\" or a treaty_article marks the amount as already treaty-excluded by the payer.",
                "recipient_name": {"type": "string", "optional": true},
                "payer_name": {"type": "string"},
                "income_code": {"type": "integer", "note": "Form 1042-S box 1 income code (Instructions for Form 1042-S, Appendix A)"},
                "gross_income": {"type": "money_cents_string", "form": "1042-S Box 2"},
                "federal_withholding": {"type": "money_cents_string", "form": "1042-S withholding (box 7a)", "note": "flows to Form 1040-NR line 25g"},
                "exemption_code": {"type": "string", "optional": true, "form": "1042-S Box 3a"},
                "treaty_country": {"type": "string", "optional": true},
                "treaty_article": {"type": "string", "optional": true}
            },
            "StateBlock": {
                "note": "Set with `tax return set-state --return <id> --state CA --file state.json`. v1 supports exactly one combination; anything else is an explicit error (other states, Form 540NR, and part-year residency are not implemented).",
                "state": {"type": "string", "values": ["CA"]},
                "residency": {"type": "string", "values": ["full_year"]}
            },
            "TaxpayerIdentity": {
                "note": "Set with `tax return set-identity --return <id> --file identity.json`. SSNs accept \"123-45-6789\" or 9 digits and are stored normalized digits-only, locally only, and shown masked (***-**-6789) in every output; the full SSN appears only inside the filled PDF.",
                "first_name": {"type": "string", "optional": true},
                "last_name": {"type": "string", "optional": true},
                "ssn": {"type": "string", "optional": true},
                "date_of_birth": {"type": "string", "format": "YYYY-MM-DD", "optional": true, "note": "born before 1961-01-02 counts as age 65 for TY2025"},
                "occupation": {"type": "string", "optional": true},
                "blind": {"type": "boolean", "default": false},
                "address": {"type": "object", "items": "MailingAddress", "optional": true},
                "spouse": {"type": "object", "items": "PersonIdentity", "optional": true, "note": "meaningful for mfj/mfs; validation warns when present for single/hoh"},
                "can_be_claimed_as_dependent": {"type": "boolean", "default": false, "note": "fills the Form 1040 line 12a checkbox; the dependent-filer standard-deduction limitation itself is NOT computed in v1 (validation warns)"}
            },
            "PersonIdentity": {
                "first_name": {"type": "string", "optional": true},
                "last_name": {"type": "string", "optional": true},
                "ssn": {"type": "string", "optional": true},
                "date_of_birth": {"type": "string", "format": "YYYY-MM-DD", "optional": true},
                "occupation": {"type": "string", "optional": true},
                "blind": {"type": "boolean", "default": false}
            },
            "MailingAddress": {
                "street": {"type": "string", "optional": true},
                "apt": {"type": "string", "optional": true},
                "city": {"type": "string", "optional": true},
                "state": {"type": "string", "optional": true},
                "zip": {"type": "string", "optional": true}
            },
            "DependentInput": {
                "name": {"type": "string", "note": "full name as it should print in the Form 1040 Dependents table"},
                "ssn": {"type": "string", "optional": true, "note": "9 digits, dashed or not; stored normalized, shown masked; the child tax credit requires the child to have an SSN (v1 warns, does not deny); the earned income credit requires one too — in v1 a child without a stored SSN does not count as an EIC qualifying child at all"},
                "relationship": {"type": "enum", "values": ["child", "stepchild", "foster_child", "other"], "note": "child/stepchild/foster_child can qualify for the child tax credit and the earned income credit; other takes the credit-for-other-dependents path only (v1 does not classify 'other' for the EIC, even for siblings/descendants)"},
                "age": {"type": "integer", "note": "age on December 31 of the tax year; the child tax credit requires under 17; the earned income credit requires under 19 (or under 24 if a full-time student, any age if permanently and totally disabled)"},
                "months_lived_with_taxpayer": {"type": "integer", "minimum": 0, "maximum": 12, "default": 12, "note": "v1 models the residency test as more than half the year (> 6 months)"},
                "full_time_student": {"type": "boolean", "default": false, "note": "full-time student during some part of each of 5 calendar months; used only by the earned income credit age test (counts until age 24)"},
                "permanently_disabled": {"type": "boolean", "default": false, "note": "permanently and totally disabled at any time during the year; used only by the earned income credit age test (counts at any age)"}
            },
            "W2Input": {
                "employer_ein": {"type": "string", "example": "12-3456789"},
                "wages": {"type": "money_cents_string", "form": "W-2 Box 1"},
                "federal_withholding": {"type": "money_cents_string", "form": "W-2 Box 2"},
                "social_security_wages": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 3"},
                "social_security_tax": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 4"},
                "medicare_wages": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 5"},
                "medicare_tax": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 6"},
                "state_wages": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 16", "note": "needed by the California section: FTB 3514 line 13 reads California earned income from box 16, not box 1. Omit only when untranscribed — with a CA state block set, a missing box 16 makes the CalEITC incomplete, never zero"},
                "state_withholding": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 17", "note": "summed into Form 540 line 71 when a CA state block is set. Omit only when untranscribed; record \"0\" explicitly when the W-2 reports no state withholding — a missing box makes line 71 incomplete, never zero"}
            },
            "I1099IntInput": {
                "payer_name": {"type": "string"},
                "payer_ein": {"type": "string", "optional": true},
                "interest_income": {"type": "money_cents_string", "form": "1099-INT Box 1"},
                "tax_exempt_interest": {"type": "money_cents_string", "form": "1099-INT Box 8", "default": "0"},
                "federal_withholding": {"type": "money_cents_string", "form": "1099-INT Box 4", "default": "0"}
            },
            "I1099DivInput": {
                "payer_name": {"type": "string"},
                "payer_ein": {"type": "string", "optional": true},
                "total_ordinary_dividends": {"type": "money_cents_string", "form": "1099-DIV Box 1a"},
                "qualified_dividends": {"type": "money_cents_string", "form": "1099-DIV Box 1b", "default": "0"},
                "federal_withholding": {"type": "money_cents_string", "form": "1099-DIV Box 4", "default": "0"}
            },
            "I1099BInput": {
                "payer_name": {"type": "string"},
                "payer_ein": {"type": "string", "optional": true},
                "transactions": {"type": "array", "items": "BrokerTxn"},
                "federal_withholding": {"type": "money_cents_string", "form": "1099-B Box 4", "default": "0"}
            },
            "BrokerTxn": {
                "description": {"type": "string", "form": "1099-B Box 1a"},
                "proceeds": {"type": "money_cents_string", "form": "1099-B Box 1d", "note": "omit only when unreported — the return's capital-gain lines become incomplete"},
                "cost_basis": {"type": "money_cents_string", "form": "1099-B Box 1e", "note": "omit only when unreported — the return's capital-gain lines become incomplete"},
                "adjustment": {"type": "money_cents_string", "form": "1099-B Box 1g", "default": "0"},
                "term": {"type": "enum", "values": ["short", "long"]}
            },
            "I1099NecInput": {
                "payer_name": {"type": "string"},
                "payer_ein": {"type": "string", "optional": true},
                "nonemployee_compensation": {"type": "money_cents_string", "form": "1099-NEC Box 1"},
                "federal_withholding": {"type": "money_cents_string", "form": "1099-NEC Box 4", "default": "0"},
                "expenses": {"type": "money_cents_string", "default": "0", "note": "total Schedule C expenses for this engagement, from the filer's records — not a 1099-NEC box; v1 takes one total per engagement (docs/KNOWN_LIMITS.md)"},
                "state_withholding": {"type": "money_cents_string", "optional": true, "form": "1099-NEC Box 5", "note": "summed into Form 540 line 71 when a CA state block is set. Omit only when untranscribed; record \"0\" explicitly when the 1099-NEC reports no state tax withheld — a missing box makes line 71 incomplete, never zero"}
            }
        }
    })
}

fn node_templates() -> Vec<(String, bool, Value)> {
    vec![
        (
            "w2".to_string(),
            true,
            json!({"employer_ein": "12-3456789", "wages": "0", "federal_withholding": "0",
                   "social_security_wages": "0", "social_security_tax": "0",
                   "medicare_wages": "0", "medicare_tax": "0",
                   "state_wages": "0", "state_withholding": "0"}),
        ),
        (
            "1099-int".to_string(),
            true,
            json!({"payer_name": "", "interest_income": "0", "tax_exempt_interest": "0", "federal_withholding": "0"}),
        ),
        (
            "1099-div".to_string(),
            true,
            json!({"payer_name": "", "total_ordinary_dividends": "0", "qualified_dividends": "0", "federal_withholding": "0"}),
        ),
        (
            "1099-b".to_string(),
            true,
            json!({"payer_name": "", "transactions": [{"description": "", "proceeds": "0", "cost_basis": "0", "adjustment": "0", "term": "short", "date_acquired": null, "date_sold": null, "adjustment_code": null}], "federal_withholding": "0"}),
        ),
        (
            "1099-nec".to_string(),
            true,
            json!({"payer_name": "", "nonemployee_compensation": "0", "federal_withholding": "0", "expenses": "0", "state_withholding": "0"}),
        ),
        (
            "1042-s".to_string(),
            true,
            json!({"payer_name": "", "income_code": 20, "gross_income": "0", "federal_withholding": "0", "exemption_code": null, "treaty_country": null, "treaty_article": null, "recipient_name": null}),
        ),
        (
            "dependent".to_string(),
            true,
            json!({"name": "", "ssn": null, "relationship": "child", "age": 0, "months_lived_with_taxpayer": 12, "full_time_student": false, "permanently_disabled": false}),
        ),
    ]
}

fn cmd_node(action: NodeAction) -> CResult<Out> {
    match action {
        NodeAction::List => {
            let nodes: Vec<Value> = node_templates()
                .into_iter()
                .map(|(name, compute, template)| {
                    json!({"node_type": name, "compute": compute, "template": template})
                })
                .collect();
            let human = nodes
                .iter()
                .map(|n| {
                    format!(
                        "{} (compute: {})",
                        n["node_type"].as_str().unwrap_or(""),
                        n["compute"].as_bool().unwrap_or(false)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Out::ok(json!({"nodes": nodes}), human))
        }
        NodeAction::Template { document } => {
            let doc = document.to_lowercase();
            match node_templates().into_iter().find(|(n, _, _)| *n == doc) {
                Some((name, compute, template)) => Ok(Out::ok(
                    json!({"node_type": name, "compute": compute, "template": template}),
                    serde_json::to_string_pretty(&template).map_err(json_err)?,
                )),
                None => Err(CliErr::new(
                    "unknown_document",
                    format!("no template for document '{document}'; see `tax node list`"),
                )),
            }
        }
    }
}

fn new_return_id(year: u32) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("r{year}-{secs}-{}", std::process::id())
}

/// `return validate` for a nonresident-alien student return: the
/// Form 1040-NR engine's findings (determination, treaty, FICA,
/// unmodeled documents), the W-2 transcription cross-checks, and the
/// Form 8843 data check. The resident findings (EIC, senior bonus,
/// Schedule SE proxies, state findings) do not apply to this route
/// and are not emitted.
fn validate_nr(ret: &StoredReturn, params: &TaxParams) -> CResult<Out> {
    let mut findings: Vec<Value> = Vec::new();
    if ret.entries.is_empty() {
        findings.push(json!({"code": "return_empty", "severity": "error",
            "message": "no documents have been added to this return", "field": null, "entry": null}));
    }
    if ret.state.is_some() {
        findings.push(json!({"code": "nra_state_conflict", "severity": "error",
            "message": "this return has both a state selection and an nra block: the California selection is a full-year-resident Form 540, incompatible with the Form 1040-NR route (California does not honor federal tax treaties; Form 540NR is not implemented)", "field": null, "entry": null}));
    }
    if ret.taxpayer.is_none() {
        findings.push(json!({"code": "identity_not_set", "severity": "info",
            "message": "identity not set — the filled Form 1040-NR and Form 8843 will lack the filer's name and identifying number (set it with `tax return set-identity`)", "field": null, "entry": null}));
    }
    match compute_view(ret, params) {
        Ok(view) => {
            if let Some(nv) = &view.nra {
                for f in &nv.findings {
                    findings.push(json!({"code": f.code, "severity": f.severity,
                        "message": f.message, "field": null, "entry": f.entry}));
                }
            }
        }
        Err(e) => {
            findings.push(json!({"code": e.code, "severity": "error",
                "message": e.message, "field": null, "entry": null}));
        }
    }
    for (i, w2) in stored_w2s(ret)?.iter().enumerate() {
        for f in tax_doc::validate_w2(w2, params) {
            // The statutory-rate cross-checks assume FICA-covered
            // wages; an exempt individual's campus wages are NOT
            // FICA-covered, so a zero Social Security / Medicare tax
            // is expected, not a discrepancy. The
            // fica_withheld_while_exempt finding covers the reverse
            // error on this route.
            if matches!(f.code.as_str(), "w2_ss_rate" | "w2_medicare_rate") {
                continue;
            }
            findings.push(json!({"code": f.code, "severity": f.severity,
                "message": f.message, "field": f.field, "entry": format!("w2_{:02}", i + 1)}));
        }
    }
    // Form 8843 data: the form must accompany the return (and is
    // filed even with no income). v1 stores the facts; the forms
    // lane fills it. Missing facts are reported, never invented.
    let block = ret
        .nra
        .as_ref()
        .expect("validate_nr routes on the nra block");
    match &block.form_8843 {
        None => {
            findings.push(json!({"code": "form_8843_data_missing", "severity": "info",
                "message": "no Form 8843 data is stored: an exempt individual must file a fully completed Form 8843 for the year (Pub 519 (2025), chapter 1) — store the facts in the nra block's form_8843 object (school and program-director name/address/phone, days present in the current and two prior years)", "field": null, "entry": null}));
        }
        Some(d) => {
            let mut missing: Vec<&str> = Vec::new();
            if d.school_name.is_none() {
                missing.push("school_name");
            }
            if d.school_address.is_none() {
                missing.push("school_address");
            }
            if d.director_name.is_none() {
                missing.push("director_name");
            }
            if d.director_address.is_none() {
                missing.push("director_address");
            }
            if d.director_phone.is_none() {
                missing.push("director_phone");
            }
            if d.days_present_current_year.is_none() {
                missing.push("days_present_current_year");
            }
            if d.days_present_prior_year_1.is_none() {
                missing.push("days_present_prior_year_1");
            }
            if d.days_present_prior_year_2.is_none() {
                missing.push("days_present_prior_year_2");
            }
            if !missing.is_empty() {
                findings.push(json!({"code": "form_8843_data_incomplete", "severity": "info",
                    "message": format!("the stored Form 8843 data is missing: {} — the form cannot be completed until these facts are recorded (they are never invented)", missing.join(", ")), "field": null, "entry": null}));
            }
        }
    }
    let valid = !findings.iter().any(|f| f["severity"] == "error");
    let human = findings
        .iter()
        .map(|f| {
            format!(
                "[{}] {}: {}",
                f["severity"].as_str().unwrap_or(""),
                f["code"].as_str().unwrap_or(""),
                f["message"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Out::ok(
        json!({"returnId": ret.id, "form": "1040-NR", "valid": valid, "findings": findings}),
        if human.is_empty() {
            "no findings".to_string()
        } else {
            human
        },
    ))
}

fn cmd_return(cli: &Cli, action: ReturnAction) -> CResult<Out> {
    let params = TaxParams::load_2025().map_err(json_err)?;
    match action {
        ReturnAction::Create {
            year,
            status,
            interest_cents,
            dividends_cents,
        } => {
            let status_parsed =
                FilingStatus::parse(&status).map_err(|e| CliErr::new("invalid_status", e))?;
            if year != 2025 {
                return Err(CliErr::new(
                    "unsupported_year",
                    format!("only TY2025 parameter data ships in v1 (got {year})"),
                ));
            }
            if matches!((interest_cents, dividends_cents), (Some(i), _) | (_, Some(i)) if i < 0) {
                return Err(CliErr::new(
                    "invalid_input",
                    "interest and dividend amounts cannot be negative",
                ));
            }
            let other_income = match (interest_cents, dividends_cents) {
                (Some(i), Some(d)) => Some(OtherIncome {
                    interest: Money::from_cents(i),
                    dividends: Money::from_cents(d),
                }),
                (None, None) => None,
                _ => {
                    return Err(CliErr::new(
                        "invalid_input",
                        "provide both --interest-cents and --dividends-cents, or neither",
                    ))
                }
            };
            let ret = StoredReturn {
                id: new_return_id(year),
                year,
                status: status_parsed.as_str().to_string(),
                entries: Vec::new(),
                other_income,
                taxpayer: None,
                state: None,
                nra: None,
            };
            save_return(&cli.store, &ret)?;
            let value = json!({"returnId": ret.id, "year": ret.year, "status": ret.status});
            let human = format!("created return {} (TY{}, {})", ret.id, ret.year, ret.status);
            Ok(Out::ok(value, human))
        }
        ReturnAction::Get { r#return } => {
            let ret = load_return(&cli.store, &r#return)?;
            let view = compute_view(&ret, &params)?;
            let mut value = view_json(&view);
            value["returnId"] = json!(ret.id);
            value["status"] = json!(ret.status);
            value["year"] = json!(ret.year);
            // Identity is shown with SSNs masked — never in full.
            value["taxpayer"] = match &ret.taxpayer {
                Some(t) => masked_identity_json(t),
                None => Value::Null,
            };
            let human = {
                let mut s = format!("return {} — complete: {}\n", ret.id, view.complete);
                if ret.taxpayer.is_some() {
                    s.push_str("  identity: set (SSNs masked in output)\n");
                }
                for (line, fr) in &view.lines {
                    s.push_str(&format!("  line {:>3}: {}\n", line, line_json(fr)));
                }
                if let Some(sv) = &view.state {
                    s.push_str(&format!(
                        "  state {} ({}) — complete: {}\n",
                        sv.block.state, sv.block.residency, sv.complete
                    ));
                    for (line, fr) in &sv.lines {
                        s.push_str(&format!("  CA line {:>3}: {}\n", line, line_json(fr)));
                    }
                }
                for w in &view.warnings {
                    s.push_str(&format!("  warning: {w}\n"));
                }
                s.trim_end().to_string()
            };
            Ok(Out::ok(value, human))
        }
        ReturnAction::Validate { r#return } => {
            let ret = load_return(&cli.store, &r#return)?;
            if ret.nra.is_some() {
                return validate_nr(&ret, &params);
            }
            let mut findings: Vec<Value> = Vec::new();
            if ret.entries.is_empty() {
                findings.push(json!({"code": "return_empty", "severity": "error",
                    "message": "no documents have been added to this return", "field": null}));
            }
            if ret.other_income.is_none() {
                findings.push(json!({"code": "other_income_not_recorded", "severity": "warning",
                    "message": "interest/dividend income not recorded; totals cannot be validated as complete", "field": null}));
            }
            match &ret.taxpayer {
                None => {
                    findings.push(json!({"code": "identity_not_set", "severity": "info",
                        "message": "identity not set — age/blind standard-deduction additions not checked (set it with `tax return set-identity`); line 12 is the base standard deduction only",
                        "field": null, "entry": null}));
                }
                Some(identity) => {
                    if identity.spouse.is_some() && matches!(ret.status.as_str(), "single" | "hoh")
                    {
                        findings.push(json!({"code": "identity_spouse_unexpected", "severity": "warning",
                            "message": format!("a spouse identity block is stored but the filing status is {}; the spouse block is meaningful only for mfj/mfs and its standard-deduction conditions are not counted", ret.status),
                            "field": null, "entry": null}));
                    }
                    if identity.can_be_claimed_as_dependent {
                        findings.push(json!({"code": "dependent_filer_std_ded_not_computed", "severity": "warning",
                            "message": "can_be_claimed_as_dependent is set: the dependent-filer standard deduction limitation (greater of $1,350 or earned income + $450, capped at the regular standard deduction) is not computed in v1 — line 12 does not apply it",
                            "field": null, "entry": null}));
                    }
                }
            }
            // Earned income credit findings (v1 scope). The per-
            // dependent EIC SSN warning flows in below via
            // validate_dependent.
            let vdeps = stored_dependents(&ret)?;
            if ret.status == "mfs" && !vdeps.is_empty() {
                findings.push(json!({"code": "eic_mfs", "severity": "warning",
                    "message": "filing status is married filing separately: v1 allows no earned income credit for MFS (the separated-spouse exception is not modeled) — line 27 is 0",
                    "field": null, "entry": null}));
            }
            if vdeps.is_empty()
                && ret
                    .taxpayer
                    .as_ref()
                    .and_then(|t| t.person.date_of_birth.as_deref())
                    .is_none()
            {
                findings.push(json!({"code": "eic_childless_age_unknown", "severity": "info",
                    "message": "no dependents are stored and no taxpayer date of birth is set: the no-children earned income credit (age 25-64 test) cannot be determined, so line 27 is 0 — set the taxpayer date of birth with `tax return set-identity` if the childless credit may apply",
                    "field": null, "entry": null}));
            }
            // Schedule 1-A, Part V findings (enhanced deduction for
            // seniors, Form 1040 line 13b). Same judgment-call pattern
            // as the childless EIC: the deduction is a stated zero
            // when no date of birth is on file.
            let taxpayer_dob = ret
                .taxpayer
                .as_ref()
                .and_then(|t| t.person.date_of_birth.as_deref());
            if ret.status == "mfs" {
                let someone_65 = ret.taxpayer.as_ref().is_some_and(|t| {
                    t.person.is_age_65(&params)
                        || t.spouse.as_ref().is_some_and(|s| s.is_age_65(&params))
                });
                if someone_65 {
                    findings.push(json!({"code": "senior_bonus_mfs", "severity": "warning",
                        "message": "filing status is married filing separately: the Schedule 1-A Part V enhanced deduction for seniors requires married taxpayers to file jointly (Part V caution) — line 13b takes no senior deduction",
                        "field": null, "entry": null}));
                }
            } else if taxpayer_dob.is_none() {
                findings.push(json!({"code": "senior_bonus_age_unknown", "severity": "info",
                    "message": "no taxpayer date of birth is set: the Schedule 1-A Part V enhanced deduction for seniors (born before January 2, 1961, with a valid SSN) cannot be determined, so line 13b takes no senior deduction — set the identity with `tax return set-identity` if it may apply",
                    "field": null, "entry": null}));
            }
            if let Some(other) = &ret.other_income {
                // The investment-income total is exact only when the
                // full engine path runs (other_income recorded).
                let vbs = stored_bs(&ret)?;
                let vints = stored_ints(&ret)?;
                let vdivs = stored_divs(&ret)?;
                let vstatus = FilingStatus::parse(&ret.status).unwrap_or(FilingStatus::Single);
                let exempt: i64 = vints.iter().map(|f| f.tax_exempt_interest.cents()).sum();
                let line2b = other.interest.cents()
                    + vints.iter().map(|f| f.interest_income.cents()).sum::<i64>();
                let line3b = other.dividends.cents()
                    + vdivs
                        .iter()
                        .map(|f| f.total_ordinary_dividends.cents())
                        .sum::<i64>();
                let (_, _, line7f, _, _) = partial_b_nets(&vbs, vstatus, &params);
                // Skip the finding when line 7 is not even computable.
                if let Some(gain) = line7f.value() {
                    let inv = exempt + line2b + line3b + gain.cents().max(0);
                    if inv > params.eic_investment_income_limit_cents {
                        findings.push(json!({"code": "eic_investment_income_limit", "severity": "info",
                            "message": "investment income (lines 2a + 2b + 3b + the positive part of line 7) exceeds the TY2025 limit of $11,950 (IRC 32(i)): no earned income credit is allowed — line 27 is 0",
                            "field": null, "entry": null}));
                    }
                }
            }
            for (i, w2) in stored_w2s(&ret)?.iter().enumerate() {
                for f in tax_doc::validate_w2(w2, &params) {
                    findings.push(json!({"code": f.code, "severity": f.severity,
                        "message": f.message, "field": f.field, "entry": format!("w2_{:02}", i + 1)}));
                }
            }
            for (i, div) in stored_divs(&ret)?.iter().enumerate() {
                for f in validate_1099_div(div) {
                    findings.push(json!({"code": f.code, "severity": f.severity,
                        "message": f.message, "field": null, "entry": format!("1099div_{:02}", i + 1)}));
                }
            }
            for form in stored_bs(&ret)? {
                for txn in &form.transactions {
                    if txn.gain().is_none() {
                        findings.push(json!({"code": "1099b_txn_incomplete", "severity": "warning",
                            "message": format!("1099-B from {}: transaction '{}' is missing proceeds or cost basis; capital-gain lines are incomplete", form.payer_name, txn.description),
                            "field": null, "entry": null}));
                    }
                }
            }
            for (i, dep) in stored_dependents(&ret)?.iter().enumerate() {
                for f in validate_dependent(dep) {
                    findings.push(json!({"code": f.code, "severity": f.severity,
                        "message": f.message, "field": null, "entry": format!("dependent_{:02}", i + 1)}));
                }
            }
            if !stored_w2s(&ret)?.is_empty() && !stored_necs(&ret)?.is_empty() {
                findings.push(json!({"code": "nec_ss_wage_proxy", "severity": "info",
                    "message": "return has both W-2 and 1099-NEC entries: the Schedule SE Social Security wage-base room is figured from total W-2 Box 1 wages as a proxy for Box 3 Social Security wages (Schedule SE line 8a). The two differ when Box 1 is reduced by pre-tax amounts that remain Social Security wages (e.g. 401(k) deferrals), in which case the SE tax's Social Security part can be overstated; Box 3, even when transcribed, is not used for this in v1 (docs/KNOWN_LIMITS.md)",
                    "field": null, "entry": null}));
            }
            // State findings (California in v1): the engine's
            // findings over the computed state section, plus the
            // selection check itself.
            if let Some(block) = &ret.state {
                match block.validated() {
                    Err(e) => {
                        findings.push(json!({"code": "ca_state_unsupported", "severity": "error",
                            "message": e, "field": null, "entry": null}));
                    }
                    Ok(()) => {
                        let view = compute_view(&ret, &params)?;
                        if let Some(sv) = &view.state {
                            for f in &sv.findings {
                                findings.push(json!({"code": f.code, "severity": f.severity,
                                    "message": f.message, "field": null, "entry": f.entry}));
                            }
                        }
                    }
                }
            }
            let valid = !findings.iter().any(|f| f["severity"] == "error");
            let human = findings
                .iter()
                .map(|f| {
                    format!(
                        "[{}] {}: {}",
                        f["severity"].as_str().unwrap_or(""),
                        f["code"].as_str().unwrap_or(""),
                        f["message"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Out::ok(
                json!({"returnId": ret.id, "valid": valid, "findings": findings}),
                if human.is_empty() {
                    "no findings".to_string()
                } else {
                    human
                },
            ))
        }
        ReturnAction::SetIdentity { r#return, file } => {
            let text = std::fs::read_to_string(&file).map_err(io_err)?;
            let raw: TaxpayerIdentity = serde_json::from_str(&text).map_err(json_err)?;
            let identity = raw
                .normalized()
                .map_err(|e| CliErr::new("invalid_input", e))?;
            let mut ret = load_return(&cli.store, &r#return)?;
            ret.taxpayer = Some(identity.clone());
            save_return(&cli.store, &ret)?;
            let masked = masked_identity_json(&identity);
            let human = format!(
                "identity stored on return {} (SSNs masked: taxpayer {}, spouse {})",
                ret.id,
                identity
                    .person
                    .ssn
                    .as_deref()
                    .map(mask_ssn)
                    .unwrap_or_else(|| "not set".to_string()),
                identity
                    .spouse
                    .as_ref()
                    .and_then(|s| s.ssn.as_deref())
                    .map(mask_ssn)
                    .unwrap_or_else(|| "not set".to_string()),
            );
            Ok(Out::ok(
                json!({"returnId": ret.id, "taxpayer": masked}),
                human,
            ))
        }
        ReturnAction::SetState {
            r#return,
            state,
            file,
        } => {
            let text = std::fs::read_to_string(&file).map_err(io_err)?;
            let block: StateBlock = serde_json::from_str(&text).map_err(json_err)?;
            if let Some(s) = &state {
                if s != &block.state {
                    return Err(CliErr::new(
                        "invalid_input",
                        format!(
                            "--state {s} does not match the state \"{}\" in {}",
                            block.state,
                            file.display()
                        ),
                    ));
                }
            }
            block
                .validated()
                .map_err(|e| CliErr::new("unsupported_state", e))?;
            let mut ret = load_return(&cli.store, &r#return)?;
            if ret.nra.is_some() {
                return Err(CliErr::new(
                    "nra_state_conflict",
                    "this return already has an nra block (Form 1040-NR route): the \
                     California selection is a full-year-resident Form 540, which is \
                     incompatible with the NR route (California does not honor federal \
                     tax treaties and taxes nonresidents on Form 540NR, which v1 does \
                     not implement). The state selection was NOT stored",
                ));
            }
            let replaced = ret.state.is_some();
            ret.state = Some(block.clone());
            save_return(&cli.store, &ret)?;
            let mut value = json!({
                "returnId": ret.id,
                "state": {"state": block.state, "residency": block.residency},
            });
            if replaced {
                value["replaced"] = json!(true);
            }
            let human = format!(
                "state selection stored on return {}: {} ({}){}",
                ret.id,
                block.state,
                block.residency,
                if replaced {
                    " (replaced previous selection)"
                } else {
                    ""
                },
            );
            Ok(Out::ok(value, human))
        }
        ReturnAction::SetNra { r#return, file } => {
            let text = std::fs::read_to_string(&file).map_err(io_err)?;
            let block: NraBlock = serde_json::from_str(&text).map_err(json_err)?;
            let mut ret = load_return(&cli.store, &r#return)?;
            if ret.state.is_some() {
                return Err(CliErr::new(
                    "nra_state_conflict",
                    "this return already has a state selection: the California selection \
                     is a full-year-resident Form 540, which is incompatible with the \
                     Form 1040-NR route (California does not honor federal tax treaties \
                     and taxes nonresidents on Form 540NR, which v1 does not implement). \
                     The nra block was NOT stored",
                ));
            }
            if ret.status != "single" {
                return Err(CliErr::new(
                    "nra_status_not_supported",
                    format!(
                        "the Form 1040-NR student route supports filing status single \
                         only in v1; this return is {} — the nra block was NOT stored",
                        ret.status
                    ),
                ));
            }
            let treaties = TreatyParams::load_2025().map_err(json_err)?;
            let determination = block
                .determination(ret.year, &treaties)
                .map_err(|e| CliErr::new("nra_determination_failed", e))?;
            // Store the block with the citizenship normalized to the
            // uppercase ISO code the determination validated.
            let mut block = block;
            block.citizenship = block
                .normalized_citizenship()
                .expect("determination validated the citizenship");
            let replaced = ret.nra.is_some();
            ret.nra = Some(block.clone());
            save_return(&cli.store, &ret)?;
            let mut value = json!({
                "returnId": ret.id,
                "form": "1040-NR",
                "nra": serde_json::to_value(&block).map_err(json_err)?,
                "determination": {
                    "exempt_individual": determination.exempt_individual,
                    "treaty_modeled": determination.treaty_modeled,
                },
            });
            if replaced {
                value["replaced"] = json!(true);
            }
            let human = format!(
                "nonresident-alien student block stored on return {}: {} visa, \
                 citizenship {}, calendar year {} of {} in status — the return now \
                 computes on Form 1040-NR (exempt individual: {}; treaty modeled: \
                 {}){}",
                ret.id,
                block.visa_type,
                block.citizenship,
                block.calendar_years_in_status_including_current,
                tax_forms::NraBlock::MAX_EXEMPT_CALENDAR_YEARS,
                determination.exempt_individual,
                determination.treaty_modeled,
                if replaced {
                    " (replaced previous block)"
                } else {
                    ""
                },
            );
            Ok(Out::ok(value, human))
        }
        ReturnAction::SetOtherIncome {
            r#return,
            interest_cents,
            dividends_cents,
        } => {
            let (ret, replaced) =
                set_other_income(&cli.store, &r#return, interest_cents, dividends_cents)?;
            let other = ret.other_income.as_ref().expect("just stored");
            let mut value = json!({
                "returnId": ret.id,
                "other_income": serde_json::to_value(other).map_err(json_err)?,
            });
            if replaced {
                value["replaced"] = json!(true);
            }
            let human = format!(
                "recorded direct interest {} / dividends {} on return {}{}",
                other.interest,
                other.dividends,
                ret.id,
                if replaced {
                    " (replaced previous totals)"
                } else {
                    ""
                },
            );
            Ok(Out::ok(value, human))
        }
        ReturnAction::List => {
            let rows = list_returns(&cli.store, &params);
            let human = if rows.is_empty() {
                "no returns in store".to_string()
            } else {
                rows.iter()
                    .map(|r| {
                        format!(
                            "{} TY{} {} complete={} identity_set={}",
                            r["id"].as_str().unwrap_or("?"),
                            r["year"],
                            r["status"].as_str().unwrap_or("?"),
                            r["complete"],
                            r["identity_set"]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(Out::ok(Value::Array(rows), human))
        }
        ReturnAction::Export {
            r#return,
            export_type,
            template,
            out,
            forms_dir,
            out_dir,
        } => {
            let ret = load_return(&cli.store, &r#return)?;
            if ret.nra.is_some() && export_type == "mef" {
                return Err(CliErr::new(
                    "not_implemented",
                    "export --type mef is not available for a Form 1040-NR return in \
                     v1: the MeF exporter emits the resident Form 1040 package, and \
                     the Form 1040-NR MeF schema package is separate and not \
                     implemented. `return export --type json`, `--type pdf`, and \
                     `--type packet` work for Form 1040-NR returns"
                        .to_string(),
                ));
            }
            match export_type.as_str() {
                "json" => {
                    let view = compute_view(&ret, &params)?;
                    let mut value = view_json(&view);
                    value["returnId"] = json!(ret.id);
                    value["status_filing"] = json!(ret.status);
                    value["year"] = json!(ret.year);
                    value["entries"] = serde_json::to_value(&ret.entries).map_err(json_err)?;
                    let dir = cli.store.join("returns").join(&ret.id);
                    std::fs::create_dir_all(&dir).map_err(io_err)?;
                    let path = dir.join("export.json");
                    std::fs::write(
                        &path,
                        serde_json::to_string_pretty(&value).map_err(json_err)?,
                    )
                    .map_err(io_err)?;
                    Ok(Out::ok(
                        json!({"status": "exported", "format": "json", "path": path.display().to_string(), "complete": view.complete}),
                        format!("exported JSON to {}", path.display()),
                    ))
                }
                "mef" => mef_export(&ret, &params, out.as_ref()),
                "pdf" => {
                    let template = template.as_ref().ok_or_else(|| {
                        CliErr::new(
                            "missing_template",
                            "pdf export requires --template <blank TY2025 Form 1040 PDF>",
                        )
                    })?;
                    let out_path = out.as_ref().ok_or_else(|| {
                        CliErr::new("missing_out", "pdf export requires --out <output PDF path>")
                    })?;
                    if ret.nra.is_some() {
                        return nr_pdf_export(&ret, &params, template, out_path);
                    }
                    let view = compute_view(&ret, &params)?;
                    let status = FilingStatus::parse(&ret.status)
                        .map_err(|e| CliErr::new("invalid_status", e))?;
                    let dependents = stored_dependents(&ret)?;
                    let plan = build_fill_values_with_dependents(
                        &view.lines,
                        status,
                        &dependents,
                        ret.taxpayer.as_ref(),
                        &params,
                    )
                    .map_err(|incomplete| {
                        CliErr::with_extra(
                            "return_incomplete",
                            format!(
                                "return {} is incomplete; mapped lines missing or incomplete: {}",
                                ret.id,
                                incomplete.join(", ")
                            ),
                            json!({"incomplete_lines": incomplete}),
                        )
                    })?;
                    let values: std::collections::HashMap<String, String> = plan
                        .values
                        .iter()
                        .map(|v| (v.field.clone(), v.value.clone()))
                        .collect();
                    let report: FillReport = PdferBackend
                        .fill_fields(template, &values, out_path)
                        .map_err(|e| CliErr::new("pdf_fill_failed", e.to_string()))?;
                    let mut all_warnings = report.warnings.clone();
                    all_warnings.extend(plan.warnings.iter().cloned());
                    // Identity-section values report with "line": null
                    // and a "section" tag; SSN values report MASKED
                    // (the plan's display_value) — the raw digits go
                    // only into the PDF itself.
                    let filled: Vec<Value> = plan
                        .values
                        .iter()
                        .map(|v| {
                            let shown = v.display_value.clone().unwrap_or_else(|| v.value.clone());
                            match &v.section {
                                Some(section) => json!({"line": Value::Null, "section": section,
                                    "field": v.field, "value": shown}),
                                None => json!({"line": v.line, "field": v.field, "value": shown}),
                            }
                        })
                        .collect();
                    let skipped: Vec<Value> = plan
                        .skipped
                        .iter()
                        .map(|s| json!({"field": s.field, "reason": s.reason}))
                        .collect();
                    Ok(Out::ok(
                        json!({"success": true, "type": "pdf",
                                "output": out_path.display().to_string(),
                                "backend": report.backend,
                                "filled": filled,
                                "skipped": skipped,
                                "warnings": all_warnings}),
                        format!(
                            "exported filled PDF to {} ({} fields, backend {})",
                            out_path.display(),
                            report.changed_fields.len(),
                            report.backend
                        ),
                    ))
                }
                "packet" => packet_export(&ret, &params, forms_dir.as_ref(), out_dir.as_ref()),
                other => Err(CliErr::new(
                    "invalid_export_type",
                    format!("unknown export type '{other}' (use json|mef|pdf|packet)"),
                )),
            }
        }
    }
}

/// Rebuild the engine input for a stored return (the same construction
/// the full compute view uses). Requires the direct interest/dividend
/// amounts to have been recorded — at creation time, or afterwards
/// with `return set-other-income`.
fn form_input(ret: &StoredReturn) -> CResult<Form1040Input> {
    let other = ret.other_income.as_ref().ok_or_else(|| {
        CliErr::new(
            "return_incomplete",
            "interest_income and ordinary_dividends have not been recorded for this return \
             (create it with --interest-cents/--dividends-cents, or record them with \
             `return set-other-income`); the packet cannot be built",
        )
    })?;
    let status = FilingStatus::parse(&ret.status).map_err(|e| CliErr::new("invalid_status", e))?;
    Ok(Form1040Input {
        status,
        w2s: stored_w2s(ret)?,
        interest_income: other.interest,
        ordinary_dividends: other.dividends,
        f1099_ints: stored_ints(ret)?,
        f1099_divs: stored_divs(ret)?,
        f1099_bs: stored_bs(ret)?,
        f1099_necs: stored_necs(ret)?,
        f1042ss: stored_1042ss(ret)?,
        dependents: stored_dependents(ret)?,
        identity: ret.taxpayer.clone(),
        state: ret.state.clone(),
        nra: ret.nra.clone(),
    })
}

/// `return export --type packet`: the full filing packet — the filled
/// Form 1040 plus every applicable schedule, each filled from its
/// geometrically verified field map, plus a `manifest.json` in the
/// output directory reporting every form as filled / not_applicable /
/// missing_template. A form whose inputs exist but whose template is
/// absent from `--forms-dir` is a LOUD manifest entry, never a silent
/// skip; an incomplete return is an error, like the pdf export.
fn packet_export(
    ret: &StoredReturn,
    params: &TaxParams,
    forms_dir: Option<&PathBuf>,
    out_dir: Option<&PathBuf>,
) -> CResult<Out> {
    let forms_dir = forms_dir.ok_or_else(|| {
        CliErr::new(
            "missing_forms_dir",
            "packet export requires --forms-dir <dir holding the blank TY2025 form PDFs>",
        )
    })?;
    let out_dir = out_dir.ok_or_else(|| {
        CliErr::new(
            "missing_out_dir",
            "packet export requires --out-dir <dir to write the packet into>",
        )
    })?;
    let view = compute_view(ret, params)?;
    if !view.complete {
        // The overall completeness conjoins the federal and (when a
        // state block is stored) California sections; name the
        // California lines explicitly when they are the gap, so a
        // CA-incomplete return does not report federal-only lines.
        let mut message = if view.incomplete.is_empty() {
            format!("return {} is incomplete", ret.id)
        } else {
            format!(
                "return {} is incomplete; lines missing or incomplete: {}",
                ret.id,
                view.incomplete.join(", ")
            )
        };
        let mut extra = json!({"incomplete_lines": view.incomplete});
        if let Some(state) = &view.state {
            if !state.complete {
                message.push_str(&format!(
                    "; California (Form 540) lines incomplete: {}",
                    state.incomplete.join(", ")
                ));
                extra["state_incomplete_lines"] = json!(state.incomplete);
            }
        }
        if view.nra.is_some() {
            message.push_str(&format!(
                "; Form 1040-NR lines incomplete: {}",
                view.incomplete.join(", ")
            ));
            extra["nra_incomplete_lines"] = json!(view.incomplete);
        }
        return Err(CliErr::with_extra("return_incomplete", message, extra));
    }
    if ret.nra.is_some() {
        return packet_export_nr(ret, params, forms_dir, out_dir);
    }
    let input = form_input(ret)?;
    let status = input.status;
    let computed = compute_1040(&input, params);
    std::fs::create_dir_all(out_dir).map_err(io_err)?;

    // One manifest entry per form, in filing order. The JSON output
    // carries the same array plus per-form fill detail.
    let mut manifest_forms: Vec<Value> = Vec::new();
    let mut detail_forms: Vec<Value> = Vec::new();
    let mut human_lines: Vec<String> = Vec::new();

    let mut record = |manifest_entry: Value, detail_entry: Value, human: String| {
        manifest_forms.push(manifest_entry);
        detail_forms.push(detail_entry);
        human_lines.push(human);
    };

    // Form 1040 — the existing fill path, with the packet's template
    // convention (forms-dir/f1040.pdf).
    {
        let plan = build_fill_values_with_dependents(
            &computed.lines,
            status,
            &input.dependents,
            ret.taxpayer.as_ref(),
            params,
        )
        .map_err(|incomplete| {
            CliErr::with_extra(
                "return_incomplete",
                format!(
                    "return {} is incomplete; mapped lines missing or incomplete: {}",
                    ret.id,
                    incomplete.join(", ")
                ),
                json!({"incomplete_lines": incomplete}),
            )
        })?;
        let template = forms_dir.join("f1040.pdf");
        if !template.exists() {
            record(
                json!({"form": "f1040", "label": "Form 1040", "status": "missing_template",
                        "file": Value::Null,
                        "notes": [format!("template not found: {} — supply the blank TY2025 Form 1040 PDF under this name in --forms-dir", template.display())]}),
                json!({"form": "f1040", "status": "missing_template"}),
                "f1040: MISSING TEMPLATE (f1040.pdf not in --forms-dir)".to_string(),
            );
        } else {
            let values: std::collections::HashMap<String, String> = plan
                .values
                .iter()
                .map(|v| (v.field.clone(), v.value.clone()))
                .collect();
            let out_path = out_dir.join("f1040.pdf");
            let report: FillReport = PdferBackend
                .fill_fields(&template, &values, &out_path)
                .map_err(|e| CliErr::new("pdf_fill_failed", e.to_string()))?;
            let filled: Vec<Value> = plan
                .values
                .iter()
                .map(|v| {
                    let shown = v.display_value.clone().unwrap_or_else(|| v.value.clone());
                    json!({"line": v.line, "field": v.field, "value": shown})
                })
                .collect();
            let skipped: Vec<Value> = plan
                .skipped
                .iter()
                .map(|s| json!({"field": s.field, "reason": s.reason}))
                .collect();
            record(
                json!({"form": "f1040", "label": "Form 1040", "status": "filled",
                        "file": "f1040.pdf", "notes": plan.warnings}),
                json!({"form": "f1040", "status": "filled", "file": "f1040.pdf",
                        "backend": report.backend, "filled": filled, "skipped": skipped,
                        "warnings": report.warnings}),
                format!(
                    "f1040: filled -> f1040.pdf ({} fields)",
                    report.changed_fields.len()
                ),
            );
        }
    }

    // The schedules, then the California forms (Form 540 filled when
    // a state block is stored; both CA forms report not_applicable on
    // a federal-only return).
    let mut packet_forms = build_packet(&input, &computed, params);
    {
        let ca = match &input.state {
            Some(_) => {
                let ca_params = CaParams::load_2025().map_err(json_err)?;
                Some(compute_ca540(&input, &computed, params, &ca_params))
            }
            None => None,
        };
        packet_forms.extend(build_ca_packet(&input, ca.as_ref(), params));
    }
    for form in packet_forms {
        if !form.applicable {
            let reason = form.not_applicable_reason.clone().unwrap_or_default();
            record(
                json!({"form": form.form, "label": form.label, "status": "not_applicable",
                        "file": Value::Null, "notes": [reason.clone()]}),
                json!({"form": form.form, "status": "not_applicable"}),
                format!("{}: not applicable ({})", form.form, reason),
            );
            continue;
        }
        // Template convention: the canonical packet file name — a
        // single f1040sc.pdf template serves every Schedule C.
        let template = forms_dir.join(format!("{}.pdf", form.form));
        if !template.exists() {
            let mut notes = form.notes.clone();
            notes.insert(
                0,
                format!(
                    "template not found: {} — supply the blank TY2025 {} PDF under this name in --forms-dir",
                    template.display(),
                    form.label
                ),
            );
            record(
                json!({"form": form.form, "label": form.label, "status": "missing_template",
                        "file": Value::Null, "notes": notes}),
                json!({"form": form.form, "status": "missing_template"}),
                format!(
                    "{}: MISSING TEMPLATE ({}.pdf not in --forms-dir)",
                    form.form, form.form
                ),
            );
            continue;
        }
        let values: std::collections::HashMap<String, String> = form
            .values
            .iter()
            .map(|v| (v.field.clone(), v.value.clone()))
            .collect();
        let out_path = out_dir.join(&form.file_name);
        let report: FillReport = PdferBackend
            .fill_fields(&template, &values, &out_path)
            .map_err(|e| CliErr::new("pdf_fill_failed", e.to_string()))?;
        let filled: Vec<Value> = form
            .values
            .iter()
            .map(|v| {
                let shown = v.display_value.clone().unwrap_or_else(|| v.value.clone());
                json!({"line": v.line, "field": v.field, "value": shown})
            })
            .collect();
        record(
            json!({"form": form.form, "label": form.label, "status": "filled",
                    "file": form.file_name, "notes": form.notes}),
            json!({"form": form.form, "status": "filled", "file": form.file_name,
                    "backend": report.backend, "filled": filled,
                    "warnings": report.warnings}),
            format!(
                "{}: filled -> {} ({} fields)",
                form.form,
                form.file_name,
                report.changed_fields.len()
            ),
        );
    }

    let manifest = json!({
        "tax_year": ret.year,
        "return_id": ret.id,
        "forms": manifest_forms,
    });
    let manifest_path = out_dir.join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).map_err(json_err)?,
    )
    .map_err(io_err)?;
    human_lines.push(format!("manifest: {}", manifest_path.display()));
    Ok(Out::ok(
        json!({"success": true, "type": "packet",
                "out_dir": out_dir.display().to_string(),
                "manifest": manifest_path.display().to_string(),
                "forms": detail_forms,
                "schedules": schedules_view(&input, &computed, params)}),
        human_lines.join("\n"),
    ))
}

/// Rebuild the engine input for a stored Form 1040-NR return (the
/// same construction [`compute_view_nr`] uses; no recorded
/// other-income is required — on the NR route those amounts only
/// feed findings).
fn nr_form_input(ret: &StoredReturn) -> CResult<Form1040Input> {
    let status = FilingStatus::parse(&ret.status).map_err(|e| CliErr::new("invalid_status", e))?;
    let (interest, dividends) = match &ret.other_income {
        Some(o) => (o.interest, o.dividends),
        None => (Money::ZERO, Money::ZERO),
    };
    Ok(Form1040Input {
        status,
        w2s: stored_w2s(ret)?,
        interest_income: interest,
        ordinary_dividends: dividends,
        f1099_ints: stored_ints(ret)?,
        f1099_divs: stored_divs(ret)?,
        f1099_bs: stored_bs(ret)?,
        f1099_necs: stored_necs(ret)?,
        f1042ss: stored_1042ss(ret)?,
        dependents: stored_dependents(ret)?,
        identity: ret.taxpayer.clone(),
        state: None,
        nra: ret.nra.clone(),
    })
}

/// `return export --type pdf` for a Form 1040-NR return: fill the
/// 1040-NR alone from its geometrically verified field map (the
/// resident path's single-form behavior, mirrored). Refuses when any
/// MAPPED NR line is incomplete — an incomplete return is never
/// exported, and never zero-filled.
fn nr_pdf_export(
    ret: &StoredReturn,
    params: &TaxParams,
    template: &Path,
    out_path: &Path,
) -> CResult<Out> {
    let view = compute_view(ret, params)?;
    let treaties = TreatyParams::load_2025().map_err(json_err)?;
    let input = nr_form_input(ret)?;
    let nr = compute_1040nr(&input, params, &treaties);
    let incomplete = nr_mapped_incomplete(&nr);
    if !incomplete.is_empty() {
        return Err(CliErr::with_extra(
            "return_incomplete",
            format!(
                "return {} is incomplete; mapped Form 1040-NR lines missing or incomplete: {}",
                ret.id,
                incomplete.join(", ")
            ),
            json!({"incomplete_lines": incomplete}),
        ));
    }
    let _ = &view;
    let forms = build_nr_packet(&input, &nr);
    let main = forms
        .iter()
        .find(|f| f.form == "f1040nr")
        .expect("build_nr_packet always yields the f1040nr form");
    let values: std::collections::HashMap<String, String> = main
        .values
        .iter()
        .map(|v| (v.field.clone(), v.value.clone()))
        .collect();
    let report: FillReport = PdferBackend
        .fill_fields(template, &values, out_path)
        .map_err(|e| CliErr::new("pdf_fill_failed", e.to_string()))?;
    let mut all_warnings = report.warnings.clone();
    all_warnings.extend(main.notes.iter().cloned());
    let filled: Vec<Value> = main
        .values
        .iter()
        .map(|v| {
            let shown = v.display_value.clone().unwrap_or_else(|| v.value.clone());
            match &v.section {
                Some(section) => json!({"line": Value::Null, "section": section,
                    "field": v.field, "value": shown}),
                None => json!({"line": v.line, "field": v.field, "value": shown}),
            }
        })
        .collect();
    Ok(Out::ok(
        json!({"success": true, "type": "pdf", "form": "1040-NR",
                "output": out_path.display().to_string(),
                "backend": report.backend,
                "filled": filled,
                "skipped": [],
                "warnings": all_warnings}),
        format!(
            "exported filled Form 1040-NR PDF to {} ({} fields, backend {})",
            out_path.display(),
            report.changed_fields.len(),
            report.backend
        ),
    ))
}

/// `return export --type packet` for a Form 1040-NR return: Form
/// 1040-NR + Schedule OI (when it has something to report) + Form
/// 8843, each filled from its geometrically verified field map, plus
/// the manifest. The caller has already verified overall
/// completeness (an incomplete NR return errors there, naming the NR
/// lines). The resident and California forms are never part of an NR
/// packet.
fn packet_export_nr(
    ret: &StoredReturn,
    params: &TaxParams,
    forms_dir: &Path,
    out_dir: &Path,
) -> CResult<Out> {
    let treaties = TreatyParams::load_2025().map_err(json_err)?;
    let input = nr_form_input(ret)?;
    let nr = compute_1040nr(&input, params, &treaties);
    std::fs::create_dir_all(out_dir).map_err(io_err)?;

    let mut manifest_forms: Vec<Value> = Vec::new();
    let mut detail_forms: Vec<Value> = Vec::new();
    let mut human_lines: Vec<String> = Vec::new();
    let mut record = |manifest_entry: Value, detail_entry: Value, human: String| {
        manifest_forms.push(manifest_entry);
        detail_forms.push(detail_entry);
        human_lines.push(human);
    };

    // The manifest states the route once, up front: an NR packet
    // contains no resident or California forms.
    record(
        json!({"form": "route", "label": "Filing route", "status": "not_applicable",
                "file": Value::Null,
                "notes": ["Form 1040-NR route: the resident Form 1040, its schedules, and the California forms are not part of this packet (California does not honor federal tax treaties and Form 540NR is not implemented)"]}),
        json!({"form": "route", "status": "not_applicable"}),
        "route: Form 1040-NR packet (no resident/CA forms)".to_string(),
    );

    for form in build_nr_packet(&input, &nr) {
        if !form.applicable {
            let reason = form.not_applicable_reason.clone().unwrap_or_default();
            record(
                json!({"form": form.form, "label": form.label, "status": "not_applicable",
                        "file": Value::Null, "notes": [reason.clone()]}),
                json!({"form": form.form, "status": "not_applicable"}),
                format!("{}: not applicable ({})", form.form, reason),
            );
            continue;
        }
        let template = forms_dir.join(format!("{}.pdf", form.form));
        if !template.exists() {
            let mut notes = form.notes.clone();
            notes.insert(
                0,
                format!(
                    "template not found: {} — supply the blank TY2025 {} PDF under this name in --forms-dir",
                    template.display(),
                    form.label
                ),
            );
            record(
                json!({"form": form.form, "label": form.label, "status": "missing_template",
                        "file": Value::Null, "notes": notes}),
                json!({"form": form.form, "status": "missing_template"}),
                format!(
                    "{}: MISSING TEMPLATE ({}.pdf not in --forms-dir)",
                    form.form, form.form
                ),
            );
            continue;
        }
        let values: std::collections::HashMap<String, String> = form
            .values
            .iter()
            .map(|v| (v.field.clone(), v.value.clone()))
            .collect();
        let out_path = out_dir.join(&form.file_name);
        let report: FillReport = PdferBackend
            .fill_fields(&template, &values, &out_path)
            .map_err(|e| CliErr::new("pdf_fill_failed", e.to_string()))?;
        let filled: Vec<Value> = form
            .values
            .iter()
            .map(|v| {
                let shown = v.display_value.clone().unwrap_or_else(|| v.value.clone());
                json!({"line": v.line, "field": v.field, "value": shown})
            })
            .collect();
        record(
            json!({"form": form.form, "label": form.label, "status": "filled",
                    "file": form.file_name, "notes": form.notes}),
            json!({"form": form.form, "status": "filled", "file": form.file_name,
                    "backend": report.backend, "filled": filled,
                    "warnings": report.warnings}),
            format!(
                "{}: filled -> {} ({} fields)",
                form.form,
                form.file_name,
                report.changed_fields.len()
            ),
        );
    }

    let manifest = json!({
        "tax_year": ret.year,
        "return_id": ret.id,
        "form": "1040-NR",
        "forms": manifest_forms,
    });
    let manifest_path = out_dir.join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).map_err(json_err)?,
    )
    .map_err(io_err)?;
    human_lines.push(format!("manifest: {}", manifest_path.display()));
    Ok(Out::ok(
        json!({"success": true, "type": "packet", "form": "1040-NR",
                "out_dir": out_dir.display().to_string(),
                "manifest": manifest_path.display().to_string(),
                "forms": detail_forms,
                "nra": {"tax_method": nr.tax_method, "schedule_oi": nr.schedule_oi}}),
        human_lines.join("\n"),
    ))
}

/// Current UTC time as an ISO 8601 timestamp (`YYYY-MM-DDTHH:MM:SSZ`)
/// for the MeF `ReturnTs`. Civil-date conversion from epoch days
/// (Howard Hinnant's algorithm); no clock crate in the workspace.
fn utc_now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let (hh, mm, ss) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// `return export --type mef`: the return as MeF-shaped XML
/// (TY2025v5.2 form documents in a Pub 4164 Return/ReturnHeader/
/// ReturnData envelope), written to `--out`.
///
/// Stricter than the PDF export in two ways, both by design: the
/// return must be complete (a filing artifact cannot carry
/// placeholders), and a stored taxpayer identity is REQUIRED (the
/// ReturnHeader's Filer block needs the taxpayer's name and SSN —
/// see tax_forms::mef). The full SSN lands in the XML file only; the
/// stdout report below carries no name or SSN. The report's
/// `validation.xsd_validated` is false: the binary performs no XSD
/// validation — `scripts/mef_acceptance.py` validates the emitted
/// form documents against the official TY2025v5.2 XSDs.
fn mef_export(ret: &StoredReturn, params: &TaxParams, out: Option<&PathBuf>) -> CResult<Out> {
    let out_path = out
        .ok_or_else(|| CliErr::new("missing_out", "mef export requires --out <output XML path>"))?;
    let view = compute_view(ret, params)?;
    if !view.complete {
        return Err(CliErr::with_extra(
            "return_incomplete",
            format!(
                "return {} is incomplete; lines missing or incomplete: {}",
                ret.id,
                view.incomplete.join(", ")
            ),
            json!({"incomplete_lines": view.incomplete}),
        ));
    }
    let input = form_input(ret)?;
    let computed = compute_1040(&input, params);
    let export = build_mef(&input, &computed, params, ret.year, &utc_now_iso())
        .map_err(|e| CliErr::new(e.code, e.message))?;
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
    }
    std::fs::write(out_path, &export.xml).map_err(io_err)?;
    let human = format!(
        "mef export: wrote {} ({} documents: {}; schema {}, whole-dollar amounts, \
         NOT transmissible — see validation notes)",
        out_path.display(),
        export.documents.len(),
        export.documents.join(", "),
        tax_forms::mef::MEF_SCHEMA_VERSION,
    );
    Ok(Out::ok(
        json!({"status": "generated", "type": "mef", "format": "mef",
        "output": out_path.display().to_string(),
        "documents": export.documents,
        "document_count": export.documents.len(),
        "validation": {
            "xsd_validated": false,
            "schema_version": tax_forms::mef::MEF_SCHEMA_VERSION,
            "notes": export.notes,
        }}),
        human,
    ))
}

fn cmd_form(cli: &Cli, action: FormAction) -> CResult<Out> {
    match action {
        FormAction::Add {
            r#return,
            node_type,
            file,
            data,
        } => {
            // Entry-id prefix per node type; unknown types stay an honest
            // not_implemented, exactly as before.
            let prefix = match node_type.as_str() {
                "w2" => "w2",
                "1099-int" => "1099int",
                "1099-div" => "1099div",
                "1099-b" => "1099b",
                "1099-nec" => "1099nec",
                "1042-s" => "f1042s",
                "dependent" => "dependent",
                _ => {
                    return Ok(Out::ok(
                        json!({"status": "not_implemented", "node_type": node_type,
                                "detail": "v1 stores and computes w2, 1099-int, 1099-div, 1099-b, 1099-nec, 1042-s, and dependent entries (see `tax node list`)"}),
                        format!("adding {node_type}: not implemented in v1"),
                    ));
                }
            };
            let text = if let Some(f) = &file {
                std::fs::read_to_string(f).map_err(io_err)?
            } else if let Some(d) = &data {
                d.clone()
            } else {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf).map_err(io_err)?;
                buf
            };
            let value: Value = serde_json::from_str(&text).map_err(json_err)?;
            // Validate against the typed struct before storing anything.
            // A dependent's SSN, when present, is validated and stored
            // normalized (digits only); the stored JSON is rewritten
            // from the typed value so the normalization sticks.
            let mut value = value;
            match node_type.as_str() {
                "w2" => {
                    let _: W2Input = serde_json::from_value(value.clone()).map_err(json_err)?;
                }
                "1099-int" => {
                    let _: I1099IntInput =
                        serde_json::from_value(value.clone()).map_err(json_err)?;
                }
                "1099-div" => {
                    let _: I1099DivInput =
                        serde_json::from_value(value.clone()).map_err(json_err)?;
                }
                "1099-b" => {
                    let _: I1099BInput = serde_json::from_value(value.clone()).map_err(json_err)?;
                }
                "1099-nec" => {
                    let _: I1099NecInput =
                        serde_json::from_value(value.clone()).map_err(json_err)?;
                }
                "1042-s" => {
                    let _: F1042sInput = serde_json::from_value(value.clone()).map_err(json_err)?;
                }
                "dependent" => {
                    let mut dep: DependentInput =
                        serde_json::from_value(value.clone()).map_err(json_err)?;
                    if dep.months_lived_with_taxpayer > 12 {
                        return Err(CliErr::new(
                            "invalid_input",
                            format!(
                                "dependent {}: months_lived_with_taxpayer must be 0-12 (got {})",
                                dep.name, dep.months_lived_with_taxpayer
                            ),
                        ));
                    }
                    if let Some(ssn) = &dep.ssn {
                        dep.ssn = Some(normalize_ssn(ssn).ok_or_else(|| {
                            CliErr::new(
                                "invalid_input",
                                format!(
                                    "dependent {}: ssn must be 9 digits (\"123-45-6789\" or \"123456789\")",
                                    dep.name
                                ),
                            )
                        })?);
                        value = serde_json::to_value(&dep).map_err(json_err)?;
                    }
                }
                _ => unreachable!(),
            }
            let mut ret = load_return(&cli.store, &r#return)?;
            let n = ret
                .entries
                .iter()
                .filter(|e| e.node_type == node_type)
                .count()
                + 1;
            let entry_id = format!("{prefix}_{n:02}");
            ret.entries.push(StoredEntry {
                entry_id: entry_id.clone(),
                node_type: node_type.clone(),
                data: value,
            });
            save_return(&cli.store, &ret)?;
            Ok(Out::ok(
                json!({"returnId": ret.id, "entryId": entry_id, "node_type": node_type}),
                format!("added {entry_id} to return {}", ret.id),
            ))
        }
    }
}

fn cmd_doc(action: DocAction) -> CResult<Out> {
    match action {
        DocAction::Classify { file } => {
            let ext = file
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let text = if ext == "pdf" {
                LopdfTextExtractor
                    .extract_text(&file)
                    .map_err(|e| CliErr::new("extract_error", e))?
            } else {
                std::fs::read_to_string(&file).map_err(io_err)?
            };
            match tax_doc::classify_text(&text) {
                Some(kind) => Ok(Out::ok(
                    json!({"file": file.display().to_string(), "kind": kind.as_str(), "status": "classified"}),
                    format!("{}: {}", file.display(), kind.as_str()),
                )),
                None => Ok(Out::ok(
                    json!({"file": file.display().to_string(), "kind": null, "status": "unknown",
                            "detail": "no known form title found in extracted text; route to the agent-vision path (tax-ingest skill), do not guess"}),
                    format!("{}: unknown document type", file.display()),
                )),
            }
        }
        DocAction::Extract { file } => {
            let params = TaxParams::load_2025().map_err(json_err)?;
            let report = tax_doc::extract_document(&file, &params)
                .map_err(|e| CliErr::new("extract_error", e))?;
            let mut value = serde_json::to_value(&report).map_err(json_err)?;
            value["file"] = json!(file.display().to_string());
            let status = value["status"].as_str().unwrap_or("").to_string();
            let form = report.form.as_deref().unwrap_or("unrecognized document");
            let human = match report.status {
                tax_doc::ExtractStatus::Extracted | tax_doc::ExtractStatus::Partial => format!(
                    "{}: {} — {} ({} fields extracted)",
                    file.display(),
                    form,
                    status,
                    report.fields.len()
                ),
                tax_doc::ExtractStatus::NeedsOcr => format!(
                    "{}: needs_ocr — no usable text layer; route to the agent-vision path",
                    file.display()
                ),
                tax_doc::ExtractStatus::Unrecognized => format!(
                    "{}: unrecognized — not an extractable document type",
                    file.display()
                ),
            };
            Ok(Out::ok(value, human))
        }
    }
}

fn fill_report_json(report: &FillReport, output: &Path) -> Value {
    json!({
        "backend": report.backend,
        "changed_fields": report.changed_fields,
        "unintended_changes": report.unintended_changes,
        "appearance_verified": report.appearance_verified,
        "warnings": report.warnings,
        "output": output.display().to_string(),
        "output_written": true,
    })
}

fn cmd_pdf(action: PdfAction) -> CResult<Out> {
    match action {
        PdfAction::Inspect { file } => {
            let fields = LopdfBackend
                .inspect_fields(&file)
                .map_err(|e| CliErr::new("pdf_error", e.to_string()))?;
            let field_json: Vec<Value> = fields
                .iter()
                .map(|f| json!({"name": f.name, "field_type": f.field_type, "value": f.value}))
                .collect();
            let human = {
                let mut s = format!("{}: {} fields\n", file.display(), fields.len());
                for f in &fields {
                    s.push_str(&format!(
                        "  {} [{}] = {}\n",
                        f.name,
                        f.field_type.as_deref().unwrap_or("?"),
                        f.value.as_deref().unwrap_or("")
                    ));
                }
                s.trim_end().to_string()
            };
            Ok(Out::ok(
                json!({"file": file.display().to_string(), "count": fields.len(), "fields": field_json}),
                human,
            ))
        }
        PdfAction::Fill {
            input,
            out,
            data,
            backend,
        } => {
            let text = std::fs::read_to_string(&data).map_err(io_err)?;
            let parsed: Value = serde_json::from_str(&text).map_err(json_err)?;
            let Some(obj) = parsed.as_object() else {
                return Err(CliErr::new(
                    "invalid_json",
                    "values file must be a flat JSON object {qualified_name: value}",
                ));
            };
            let mut values: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for (k, v) in obj {
                let Some(s) = v.as_str() else {
                    return Err(CliErr::new(
                        "invalid_json",
                        format!("value for '{k}' must be a string"),
                    ));
                };
                values.insert(k.clone(), s.to_string());
            }
            let report = match backend.as_str() {
                "pdfer" | "pdfer_forms" => PdferBackend
                    .fill_fields(&input, &values, &out)
                    .map_err(|e| CliErr::new("pdf_fill_error", e.to_string())),
                "lopdf" => LopdfBackend
                    .fill_fields(&input, &values, &out)
                    .map_err(|e| CliErr::new("pdf_fill_error", e.to_string())),
                other => Err(CliErr::new(
                    "invalid_backend",
                    format!("unknown pdf backend '{other}' (use pdfer|lopdf)"),
                )),
            }?;
            let human = format!(
                "filled {} via {}: {} field(s) changed, appearance verified on {}, \
                 output written to {}",
                input.display(),
                report.backend,
                report.changed_fields.len(),
                report.appearance_verified,
                out.display()
            );
            Ok(Out::ok(fill_report_json(&report, &out), human))
        }
    }
}

/// How line 16 was figured, for `tax explain --line 16`.
fn line16_derivation(method: Option<&str>) -> String {
    match method {
        Some("qdcg") => "Qualified Dividends and Capital Gain Tax Worksheet (Instructions for Form 1040, line 16): qualified dividends (line 3a) plus the Schedule D capital gain (smaller of Schedule D lines 15/16, floored at 0) are taxed at 0%/15%/20% (thresholds per Rev. Proc. 2024-40, section 3.03); the rest is taxed by the Tax Table / Tax Computation Worksheet, and the result is capped at the ordinary tax on all of line 15".to_string(),
        Some("tax_table") => "TY2025 Tax Table on line 15 (taxable income under $100,000); table extracted from Publication 1040, Tax and Earned Income Credit Tables (params/2025-tax-table.json)".to_string(),
        Some("tax_computation") => "Tax Computation Worksheet on line 15 (ordinary brackets, params/2025.json); applies at $100,000 or more of taxable income".to_string(),
        _ => "not figured yet (line 16 is incomplete for this return); when computed it is the QDCG worksheet, the Tax Table, or the Tax Computation Worksheet, per the line 16 instructions".to_string(),
    }
}

/// `tax explain --state CA --line <n>`: explain a line of the
/// computed state section (Form 540 printed line numbers, plus the
/// internal `caleitc_*` keys). The state counterpart of
/// [`cmd_explain`]; the return must have the state selected.
fn cmd_explain_nr(cli: &Cli, id: &str, line: Option<String>, fact: Option<String>) -> CResult<Out> {
    let fact_map: &[(&str, &str)] = &[
        ("wages", "1a"),
        ("treaty_exempt_income", "1k"),
        ("total_income", "9"),
        ("agi", "11a"),
        ("standard_deduction", "12"),
        ("taxable_income", "15"),
        ("tax", "16"),
        ("total_tax", "24"),
        ("w2_withholding", "25a"),
        ("f1042s_withholding", "25g"),
        ("total_payments", "33"),
        ("overpaid", "34"),
        ("refund", "34"),
        ("amount_owed", "37"),
        ("eci_wages_gross", "eci_wages_gross"),
        ("treaty_wage_exclusion", "treaty_wage_exclusion"),
        ("treaty_claim_on_return", "treaty_claim_on_return"),
        ("taxable_scholarship", "taxable_scholarship"),
        ("scholarship", "taxable_scholarship"),
    ];
    let line_no = if let Some(l) = &line {
        l.clone()
    } else if let Some(f) = &fact {
        fact_map
            .iter()
            .find(|(name, _)| *name == f.as_str())
            .map(|(_, l)| l.to_string())
            .ok_or_else(|| CliErr::new("unknown_fact", format!("unknown NR fact '{f}'")))?
    } else {
        return Err(CliErr::new("invalid_input", "provide --line or --fact"));
    };

    let params = TaxParams::load_2025().map_err(json_err)?;
    let ret = load_return(&cli.store, id)?;
    let view = compute_view(&ret, &params)?;
    let nv = view.nra.as_ref().ok_or_else(|| {
        CliErr::new(
            "nra_not_selected",
            "the return has no computed Form 1040-NR section",
        )
    })?;
    let fr = nv.lines.get(&line_no).cloned().ok_or_else(|| {
        CliErr::new(
            "unsupported_line",
            format!("Form 1040-NR line {line_no} is not computed in v1"),
        )
    })?;

    let (derivation, citation) = match line_no.as_str() {
        "1a" => ("taxable wages: total W-2 box 1 plus the taxable part of Form 1042-S income code 20, MINUS the treaty personal-services exemption claimed on this return. Wages exempt under a treaty are never reported on line 1a (they are on line 1k); when the employer split the pay with Form 8233, the exempt part is on the 1042-S and the W-2 already excludes it".to_string(), "Form 1040-NR (2025), line 1a; Instructions for Form 1040-NR (2025), Line 1a and 'Amounts Exempt Under a Treaty' (worked examples 1 and 2)".to_string()),
        "1k" => ("total income exempt by a treaty: the wage exemption (Schedule OI, item L) plus any treaty-exempt scholarship, from the computed Schedule OI data".to_string(), "Form 1040-NR (2025), line 1k; Instructions for Form 1040-NR (2025), Line 1k".to_string()),
        "1z" => ("total of lines 1a through 1h; in v1 only line 1a is nonzero (lines 1b-1h are not modeled)".to_string(), "Form 1040-NR (2025), line 1z".to_string()),
        "8" => ("additional income from Schedule 1 (Form 1040), line 10: in v1 the taxable scholarship or fellowship grants of Schedule 1, line 8r (Form 1042-S income code 15 amounts the treaty does not exempt)".to_string(), "Form 1040-NR (2025), line 8; Instructions for Form 1040-NR (2025), Line 1a TIP (Schedule 1, line 8r)".to_string()),
        "9" => ("total effectively connected income: lines 1z + 2b + 3b + 4b + 5b + 7a + 8; in v1 only 1z and 8 can be nonzero (bank interest is excluded for nonresident aliens; dividends and capital gains belong on Schedule NEC, not modeled)".to_string(), "Form 1040-NR (2025), line 9".to_string()),
        "11a" | "11b" => ("adjusted gross income: line 9 minus line 10 (adjustments from Schedule 1); no Schedule 1 adjustments are modeled in v1, so AGI equals line 9".to_string(), "Form 1040-NR (2025), lines 11a/11b".to_string()),
        "12" => ("deductions: ZERO for most nonresident aliens (no standard deduction). For a student from India eligible under Article 21(2) of the U.S.-India treaty, the Form 1040 standard deduction amount for the filing status instead (itemized deductions on Schedule A are not computed in v1)".to_string(), "Form 1040-NR (2025), line 12; Instructions for Form 1040-NR (2025), Line 12".to_string()),
        "14" => ("total deductions: line 12 (lines 13a-13c are zero in v1)".to_string(), "Form 1040-NR (2025), line 14".to_string()),
        "15" => ("taxable income: line 11b minus line 14, floored at zero".to_string(), "Form 1040-NR (2025), line 15".to_string()),
        "16" => (match nv.tax_method.as_deref() {
            Some("tax_computation") => "tax from the Tax Computation Worksheet (single brackets in params/2025.json): line 15 is $100,000 or more".to_string(),
            _ => "tax from the TY2025 Tax Table (params/2025-tax-table.json): line 15 is under $100,000, so the table applies. Form 1040-NR uses the same table as Form 1040".to_string(),
        }, "Form 1040-NR (2025), line 16; Instructions for Form 1040-NR (2025), Line 16 (referring to the Instructions for Form 1040, Line 16)".to_string()),
        "18" => ("line 16 plus line 17 (Schedule 2, line 3 — zero in v1)".to_string(), "Form 1040-NR (2025), line 18".to_string()),
        "22" => ("line 18 minus line 21 (nonrefundable credits): zero in v1 — no credits are computed on the student route, so line 22 equals line 18".to_string(), "Form 1040-NR (2025), line 22".to_string()),
        "24" => ("total tax: line 22 plus line 23d (Schedule NEC tax and other taxes — zero in v1; Schedule NEC is not modeled)".to_string(), "Form 1040-NR (2025), line 24".to_string()),
        "25a" => ("federal income tax withheld on Forms W-2 (box 2), summed over all stored W-2 entries".to_string(), "Form 1040-NR (2025), line 25a".to_string()),
        "25d" => ("total of lines 25a-25c; in v1 only W-2 withholding (25a) is nonzero".to_string(), "Form 1040-NR (2025), line 25d".to_string()),
        "25g" => ("federal income tax withheld on Forms 1042-S, summed over all stored 1042-S entries".to_string(), "Form 1040-NR (2025), line 25g".to_string()),
        "32" => ("total other payments and refundable credits (lines 28-31): zero in v1 — the additional child tax credit is not computed on the student route".to_string(), "Form 1040-NR (2025), line 32".to_string()),
        "33" => ("total payments: lines 25d + 25e + 25f + 25g + 26 + 32; in v1, W-2 withholding (25a, via 25d) plus Form 1042-S withholding (25g)".to_string(), "Form 1040-NR (2025), line 33".to_string()),
        "34" => ("overpaid: line 33 minus line 24 when payments exceed total tax, else 0".to_string(), "Form 1040-NR (2025), line 34".to_string()),
        "37" => ("amount owed: line 24 minus line 33 when total tax exceeds payments, else 0".to_string(), "Form 1040-NR (2025), line 37".to_string()),
        "eci_wages_gross" => ("gross effectively connected wages before any treaty exemption: total W-2 box 1 plus total Form 1042-S income code 20 gross (internal key)".to_string(), "engine internal key (Form 1040-NR (2025), line 1 components)".to_string()),
        "treaty_wage_exclusion" => ("the total personal-services treaty exemption: the part the payer already excluded on a treaty-marked Form 1042-S plus the part claimed on this return against W-2 wages, capped by the treaty amount (internal key)".to_string(), "engine internal key (treaty params in params/2025-treaties.json)".to_string()),
        "treaty_claim_on_return" => ("the part of the treaty wage exemption claimed on this return: the treaty cap minus the already-excluded Form 1042-S part, floored at zero and capped by the W-2 wages (internal key)".to_string(), "engine internal key; Instructions for Form 1040-NR (2025), 'Amounts Exempt Under a Treaty', example 2".to_string()),
        "taxable_scholarship" => ("the taxable scholarship or fellowship grants: Form 1042-S income code 15 gross minus the part exempt under the treaty (internal key; flows to line 8 via Schedule 1, line 8r)".to_string(), "engine internal key (Schedule 1 (Form 1040), line 8r)".to_string()),
        _ => ("computed directly from stored inputs".to_string(), "Form 1040-NR (2025) and its instructions".to_string()),
    };

    let mut value = json!({
        "returnId": ret.id,
        "form": "1040-NR",
        "line": line_no,
        "result": line_json(&fr),
        "derivation": derivation,
        "citation": citation,
    });
    if line_no == "16" {
        value["tax_method"] = json!(nv.tax_method);
    }
    let human = format!(
        "Form 1040-NR line {line_no}: {} — {derivation} [{citation}]",
        line_json(&fr)
    );
    Ok(Out::ok(value, human))
}

fn cmd_explain_state(
    cli: &Cli,
    id: &str,
    line: Option<String>,
    fact: Option<String>,
    state: &str,
) -> CResult<Out> {
    let fact_map: &[(&str, &str)] = &[
        ("ca_personal_exemptions", "7"),
        ("ca_dependent_exemptions", "10"),
        ("ca_total_exemptions", "11"),
        ("federal_agi", "13"),
        ("ca_subtractions", "14"),
        ("ca_additions", "16"),
        ("ca_agi", "17"),
        ("ca_standard_deduction", "18"),
        ("ca_taxable_income", "19"),
        ("ca_tax", "31"),
        ("exemption_credits", "32"),
        ("ca_tax_after_credits", "33"),
        ("ca_total_tax", "64"),
        ("ca_withholding", "71"),
        ("caleitc", "75"),
        ("ca_eitc", "75"),
        ("caleitc_earned_income", "caleitc_earned_income"),
        ("caleitc_qualifying_children", "caleitc_qualifying_children"),
        ("young_child_tax_credit", "76"),
        ("yctc", "76"),
        ("ca_total_payments", "78"),
        ("ca_refund", "97"),
        ("ca_overpaid", "97"),
        ("ca_amount_owed", "100"),
        ("ca_tax_due", "100"),
    ];
    let line_no = if let Some(l) = &line {
        l.clone()
    } else if let Some(f) = &fact {
        fact_map
            .iter()
            .find(|(name, _)| *name == f.as_str())
            .map(|(_, l)| l.to_string())
            .ok_or_else(|| CliErr::new("unknown_fact", format!("unknown state fact '{f}'")))?
    } else {
        return Err(CliErr::new("invalid_input", "provide --line or --fact"));
    };

    let params = TaxParams::load_2025().map_err(json_err)?;
    let ret = load_return(&cli.store, id)?;
    let block = ret.state.as_ref().ok_or_else(|| {
        CliErr::new(
            "state_not_selected",
            format!(
                "return {} has no state selection; select one with \
                 `tax return set-state --return {} --state {state} --file state.json`",
                ret.id, ret.id
            ),
        )
    })?;
    if !block.state.eq_ignore_ascii_case(state) {
        return Err(CliErr::new(
            "state_not_selected",
            format!(
                "return {} has state {} selected, not {state}",
                ret.id, block.state
            ),
        ));
    }
    block
        .validated()
        .map_err(|e| CliErr::new("unsupported_state", e))?;
    let view = compute_view(&ret, &params)?;
    let sv = view.state.as_ref().ok_or_else(|| {
        CliErr::new(
            "state_not_selected",
            "the return has no computed state section",
        )
    })?;
    let fr = sv.lines.get(&line_no).cloned().ok_or_else(|| {
        CliErr::new(
            "unsupported_line",
            format!("California line {line_no} is not computed in v1"),
        )
    })?;

    let (derivation, citation) = match line_no.as_str() {
        "7" => ("personal exemption credits: the exemption count for the filing status (1, or 2 if married filing jointly — reduced by the line 6 rule when the taxpayer can be claimed as a dependent) times $153".to_string(), "Form 540 (2025), line 7; 2025 Form 540 booklet, line 7 instructions".to_string()),
        "8" => ("blind exemption credits: the number of blind filers (taxpayer, plus spouse for MFJ/MFS) in the stored identity times $153; zero without an identity".to_string(), "Form 540 (2025), line 8".to_string()),
        "9" => ("senior exemption credits: the number of filers (taxpayer, plus spouse for MFJ/MFS) who are 65 or older (born before January 2, 1961) in the stored identity times $153; zero without an identity".to_string(), "Form 540 (2025), line 9".to_string()),
        "10" => ("dependent exemption credits: the number of stored dependent entries times $475".to_string(), "Form 540 (2025), line 10".to_string()),
        "11" => ("total exemption credits before the AGI limitation: lines 7 + 8 + 9 + 10".to_string(), "Form 540 (2025), line 11".to_string()),
        "13" => ("federal adjusted gross income: the computed federal Form 1040 line 11, carried over as the California starting point".to_string(), "Form 540 (2025), line 13".to_string()),
        "14" => ("California subtractions: the total of Schedule CA (540), Part I, line 27, column B. In v1 the adjustments are scoped to the facts the federal engine produces, which are all California-conforming for the v1 persona, so this is a stated zero (docs/KNOWN_LIMITS.md)".to_string(), "Form 540 (2025), line 14; Schedule CA (540), Part I".to_string()),
        "15" => ("line 13 minus line 14".to_string(), "Form 540 (2025), line 15".to_string()),
        "16" => ("California additions: the total of Schedule CA (540), Part I, line 27, column C. A stated zero in v1 for the same scoping as line 14 — in particular the federal deductible part of self-employment tax is NOT added back, because California conforms to it for a taxpayer who is self-employed for both federal and California purposes (the Schedule CA Section C line 15 adjustment applies only to a federal-contractor / California-employee classification difference, which v1 does not model)".to_string(), "Form 540 (2025), line 16; 2025 Instructions for Schedule CA (540), Part I, Section C, line 15".to_string()),
        "17" => ("California adjusted gross income: line 15 plus line 16".to_string(), "Form 540 (2025), line 17".to_string()),
        "18" => ("the California standard deduction for the filing status from params/2025-ca.json (v1 always takes the standard deduction; California itemized deductions via Schedule CA Part II are not computed). California gives no addition for age 65 or blindness here — those facts earn exemption credits on lines 8/9 instead".to_string(), "Form 540 (2025), line 18; 2025 Form 540 booklet, California Standard Deduction Chart for Most People".to_string()),
        "19" => ("line 17 minus line 18, floored at zero".to_string(), "Form 540 (2025), line 19".to_string()),
        "31" => (match sv.line31_method.as_deref() {
            Some("ca_rate_schedule") => "tax from the 2025 California Tax Rate Schedules (params/2025-ca.json): line 19 is over $100,000, so the schedules apply — Schedule X (single / married filing separately), Y (married filing jointly), or Z (head of household)".to_string(),
            _ => "tax from the 2025 California Tax Table (params/2025-ca-tax-table.json, extracted from the official booklet): line 19 is $100,000 or less, so the table applies".to_string(),
        }, "Form 540 (2025), line 31; 2025 Form 540 booklet, 2025 California Tax Table / Tax Rate Schedules".to_string()),
        "32" => ("exemption credits after the AGI Limitation Worksheet: line 11, reduced when federal AGI (line 13) exceeds the status threshold ($252,203 single/MFS, $504,411 MFJ, $378,310 HoH) — the excess divided by $2,500 ($1,250 MFS), rounded up, times $6 per exemption, applied to the lines 7-9 group and the line 10 dependents separately, each floored at zero".to_string(), "Form 540 (2025), line 32; 2025 Form 540 booklet, AGI Limitation Worksheet".to_string()),
        "33" => ("line 31 minus line 32, floored at zero (exemption credits are nonrefundable)".to_string(), "Form 540 (2025), line 33".to_string()),
        "34" => ("tax from Schedule G-1 / form FTB 5870A (lump-sum distributions): not computed in v1, a stated zero".to_string(), "Form 540 (2025), line 34".to_string()),
        "35" => ("line 33 plus line 34".to_string(), "Form 540 (2025), line 35".to_string()),
        "48" => ("line 35 minus the total nonrefundable credits of lines 40-47 (child/dependent care, other named credits, the Nonrefundable Renter's Credit) — none are computed in v1, so line 48 equals line 35".to_string(), "Form 540 (2025), line 48".to_string()),
        "62" => ("Behavioral Health Services Tax: 1% of line 19 over $1,000,000 (figured in whole dollars on the printed worksheet; the engine keeps cents)".to_string(), "Form 540 (2025), line 62; 2025 Form 540 booklet, line 62 instructions".to_string()),
        "64" => ("total tax: line 48 plus line 62 (lines 61 AMT and 63 other taxes are not computed in v1)".to_string(), "Form 540 (2025), line 64".to_string()),
        "71" => ("California income tax withheld: the sum of W-2 box 17 over all stored W-2 entries plus 1099-NEC box 5 over all stored 1099-NEC entries. Incomplete — never zero-filled — when any entry's state withholding box was not transcribed".to_string(), "Form 540 (2025), line 71".to_string()),
        "75" => ("the California earned income tax credit (form FTB 3514): gates in booklet order — no credit for married filing separately in v1, federal AGI under $32,901, investment income at most $4,814, and with no qualifying children the filer must be at least 18 (born on or before January 1, 2008). Earned income (internal key caleitc_earned_income) is W-2 box 16 California wages plus business income (Schedule C net minus the deductible part of SE tax, FTB 3514 Worksheet 3); the credit is the California EITC Table amount for earned income, or the smaller of the earned-income and AGI table amounts when AGI differs and is at or above the Part II threshold ($4,661 / $6,998 / $9,823 by child count)".to_string(), "Form 540 (2025), line 75; 2025 FTB 3514 booklet, Steps 1-6 and the California EITC Worksheet".to_string()),
        "caleitc_earned_income" => ("California earned income for the California EITC (FTB 3514 line 19): W-2 box 16 California wages (line 13) plus business income (line 18, from Worksheet 3: federal Schedule 1 line 3 business income minus the deductible part of self-employment tax). Incomplete when any W-2's box 16 was not transcribed (internal key)".to_string(), "FTB 3514 (2025), Step 5, lines 13-19 and Worksheet 3 (internal key)".to_string()),
        "caleitc_qualifying_children" => ("the number of stored dependents who are qualifying children for the California EITC: the federal EIC child tests (relationship, age, residency — lived with the taxpayer more than half the year, which the full-year-resident selection stands in for as lived in California — and an SSN or ITIN stored); expressed in the lines map as count x $1.00, not an amount of money (internal key)".to_string(), "FTB 3514 (2025), Step 3 (internal key)".to_string()),
        "76" => ("the Young Child Tax Credit (form FTB 3514 Part VII): requires having been allowed the California EITC and a qualifying child younger than six years old as of the last day of the taxable year. $1,189 per return (not per child), reduced by $21.71 for every $100 (two decimal places, not rounded) of California earned income over $27,425; a result between $0 and $1 is entered as $1, over $1 rounded to the nearest dollar. The earned-income-of-zero-or-less expansion (Step 8) is not computed in v1 — line 76 is Incomplete when it may apply".to_string(), "Form 540 (2025), line 76; 2025 FTB 3514 booklet, Step 8 and Part VII, lines 23-28".to_string()),
        "78" => ("total payments: line 71 (withholding) + line 75 (California EITC) + line 76 (Young Child Tax Credit) in v1; lines 72-74 (estimated payments, real-estate withholding, film credit) and 77 (Foster Youth Tax Credit) are not computed".to_string(), "Form 540 (2025), line 78".to_string()),
        "97" => ("overpaid tax: line 78 minus line 64 when payments exceed total tax, else 0. Lines 91-95 (use tax, Individual Shared Responsibility) are not modeled in v1 and are treated as zero".to_string(), "Form 540 (2025), line 97".to_string()),
        "100" => ("tax due: line 64 minus line 78 when total tax exceeds payments, else 0. Lines 91-95 (use tax, Individual Shared Responsibility) are not modeled in v1 and are treated as zero".to_string(), "Form 540 (2025), line 100".to_string()),
        _ => ("computed directly from stored inputs".to_string(), "Form 540 (2025) and its instructions".to_string()),
    };

    let mut value = json!({
        "returnId": ret.id,
        "state": block.state,
        "residency": block.residency,
        "line": line_no,
        "result": line_json(&fr),
        "derivation": derivation,
        "citation": citation,
    });
    if line_no == "31" {
        value["line31_method"] = json!(sv.line31_method);
    }
    let human = format!(
        "{} line {line_no}: {} — {derivation} [{citation}]",
        block.state,
        line_json(&fr)
    );
    Ok(Out::ok(value, human))
}

fn cmd_explain(
    cli: &Cli,
    id: &str,
    line: Option<String>,
    fact: Option<String>,
    state: Option<String>,
) -> CResult<Out> {
    if let Some(state) = &state {
        return cmd_explain_state(cli, id, line, fact, state);
    }
    if load_return(&cli.store, id)?.nra.is_some() {
        return cmd_explain_nr(cli, id, line, fact);
    }
    let fact_map: &[(&str, &str)] = &[
        ("wages", "1a"),
        ("tax_exempt_interest", "2a"),
        ("taxable_interest", "2b"),
        ("qualified_dividends", "3a"),
        ("ordinary_dividends", "3b"),
        ("capital_gain", "7"),
        ("net_capital_gain", "schd_net_capital_gain"),
        ("capital_loss_carryover_short", "schd_carryover_short"),
        ("capital_loss_carryover_long", "schd_carryover_long"),
        ("additional_income", "8"),
        ("schedule_c_net_profit", "schc_net_profit"),
        ("total_income", "9"),
        ("adjustments", "10"),
        ("agi", "11"),
        ("standard_deduction", "12"),
        ("senior_bonus", "13b"),
        ("senior_deduction", "senior_bonus"),
        ("additional_deductions", "13b"),
        ("total_deductions", "14"),
        ("taxable_income", "15"),
        ("tax", "16"),
        ("child_tax_credit", "19"),
        ("nonrefundable_credits", "21"),
        ("tax_after_credits", "22"),
        ("se_tax", "23"),
        ("net_se_earnings", "schedse_net_earnings"),
        ("total_tax", "24"),
        ("earned_income_credit", "27"),
        ("eic", "27"),
        ("eic_earned_income", "eic_earned_income"),
        ("eic_qualifying_children", "eic_qualifying_children"),
        ("eic_investment_income", "eic_investment_income"),
        ("additional_child_tax_credit", "28"),
        ("actc", "28"),
        ("refundable_credits", "32"),
        ("total_payments", "33"),
        ("ctc_tentative_credit", "sched8812_tentative"),
        (
            "ctc_credit_after_phaseout",
            "sched8812_credit_after_phaseout",
        ),
        ("earned_income", "sched8812_earned_income"),
        ("overpaid", "34"),
        ("refund", "34"),
        ("amount_owed", "37"),
    ];
    let line_no = if let Some(l) = &line {
        l.clone()
    } else if let Some(f) = &fact {
        fact_map
            .iter()
            .find(|(name, _)| *name == f.as_str())
            .map(|(_, l)| l.to_string())
            .ok_or_else(|| CliErr::new("unknown_fact", format!("unknown fact '{f}'")))?
    } else {
        return Err(CliErr::new("invalid_input", "provide --line or --fact"));
    };

    let params = TaxParams::load_2025().map_err(json_err)?;
    let ret = load_return(&cli.store, id)?;
    let view = compute_view(&ret, &params)?;
    let fr = view.lines.get(&line_no).cloned().ok_or_else(|| {
        CliErr::new(
            "unsupported_line",
            format!("line {line_no} is not computed in v1"),
        )
    })?;

    let (derivation, citation) = match line_no.as_str() {
        "1a" | "1z" => ("sum of wages (Box 1) over all stored W-2 entries".to_string(), "Form 1040 instructions, line 1".to_string()),
        "2a" => ("sum of tax-exempt interest (Box 8) over all stored 1099-INT entries".to_string(), "Form 1040, line 2a".to_string()),
        "2b" => ("taxable interest recorded directly plus sum of interest income (Box 1) over all stored 1099-INT entries".to_string(), "Form 1040, line 2b".to_string()),
        "3a" => ("sum of qualified dividends (Box 1b) over all stored 1099-DIV entries".to_string(), "Form 1040, line 3a".to_string()),
        "3b" => ("ordinary dividends recorded directly plus sum of total ordinary dividends (Box 1a) over all stored 1099-DIV entries".to_string(), "Form 1040, line 3b".to_string()),
        "7" => ("Schedule D combination of the 1099-B nets (internal keys schd_net_short / schd_net_long): the combined gain in full, or a loss limited to $3,000 ($1,500 MFS) by IRC 1211(b), with the rest carried over".to_string(), "Form 1040, line 7 (capital gain or loss, via Schedule D lines 16/21)".to_string()),
        "schd_net_short" => ("sum of (proceeds - basis + adjustment) over short-term 1099-B transactions".to_string(), "Form 8949 / Schedule D aggregation (internal key)".to_string()),
        "schd_net_long" => ("sum of (proceeds - basis + adjustment) over long-term 1099-B transactions".to_string(), "Form 8949 / Schedule D aggregation (internal key)".to_string()),
        "8" => ("Schedule C net profit from 1099-NEC engagements (internal key schc_net_profit), via Schedule 1 line 3 — in v1 the only additional income on Schedule 1".to_string(), "Form 1040, line 8 (additional income from Schedule 1)".to_string()),
        "schc_net_profit" => ("sum over 1099-NEC entries of (Box 1 nonemployee compensation minus the engagement's total Schedule C expenses); a net loss is allowed".to_string(), "Schedule C / Schedule 1, line 3 (internal key)".to_string()),
        "9" => ("line 1z + line 2b + line 3b + line 7 + line 8".to_string(), "Form 1040, line 9".to_string()),
        "10" => ("the deductible part of self-employment tax: one-half of line 23 (Schedule 1 line 15, from Schedule SE line 13)".to_string(), "Form 1040, line 10; IRC 164(f)".to_string()),
        "11" => ("line 9 minus line 10 (adjustments to income)".to_string(), "Form 1040, line 11".to_string()),
        "12" => (match &ret.taxpayer {
            Some(identity) => {
                let addition = additional_standard_deduction(
                    Some(identity),
                    FilingStatus::parse(&ret.status).unwrap_or(FilingStatus::Single),
                    &params,
                );
                if addition.cents() > 0 {
                    format!("standard deduction for filing status '{}' from params/2025.json, plus {addition} additional for age 65 / blindness conditions from the stored identity (per-condition amount from params/2025.json)", ret.status)
                } else {
                    format!("standard deduction for filing status '{}' from params/2025.json (identity stored; no age-65/blind additions apply)", ret.status)
                }
            }
            None => format!("standard deduction for filing status '{}' from params/2025.json", ret.status),
        }, params.source.clone()),
        "13b" => ("additional deductions from Schedule 1-A, line 38 — in v1 the Schedule 1-A total is its Part V enhanced deduction for seniors alone (internal key senior_bonus): $6,000 per eligible person (valid SSN stored, born before January 2, 1961; the spouse counts only on a joint return), reduced per person by 6% of modified AGI (AGI in v1) over $75,000 ($150,000 if married filing jointly) on Schedule 1-A lines 31-37. Married filing separately takes none (Part V caution: married taxpayers must file jointly). Parts II (tips), III (overtime), and IV (car loan interest) of Schedule 1-A are not implemented in v1".to_string(), "Form 1040, line 13b; Schedule 1-A (Form 1040) TY2025, Part V and line 38".to_string()),
        "senior_bonus" => ("the Schedule 1-A line 37 enhanced deduction for seniors: the per-person amount of Schedule 1-A line 35 ($6,000 minus 6% of the modified AGI over the filing-status threshold, floored at zero) times the number of eligible people (lines 36a/36b — valid SSN stored and born before January 2, 1961; spouse only on a joint return); equal to line 13b in v1 (internal key)".to_string(), "Schedule 1-A (Form 1040) TY2025, Part V, lines 31-37 (internal key)".to_string()),
        "14" => ("total deductions: line 12 plus line 13b (printed form: lines 12e + 13a + 13b; line 13a, the qualified business income deduction from Form 8995/8995-A, is not computed in v1)".to_string(), "Form 1040, line 14".to_string()),
        "15" => ("line 11 minus line 14 (total deductions: the line 12 standard deduction plus line 13b additional deductions), floored at zero".to_string(), "Form 1040, line 15".to_string()),
        "16" => (line16_derivation(view.line16_method.as_deref()), params.source.clone()),
        "23" => ("self-employment tax from Schedule SE, via Schedule 2 line 4: 12.4% Social Security on net SE earnings (internal key schedse_net_earnings) up to the wage-base room left after W-2 wages, plus 2.9% Medicare on all net SE earnings; rates and wage base from params/2025.json".to_string(), "Form 1040, line 23; Schedule SE; IRC 1401".to_string()),
        "schedse_net_earnings" => ("Schedule SE line 4a: Schedule C net profit times 92.35%; zero when the net is a loss".to_string(), "Schedule SE, line 4a; IRC 1402(a) (internal key)".to_string()),
        "sched2_se_tax" => ("the Schedule SE tax as it enters Schedule 2 line 4; equal to line 23".to_string(), "Schedule 2, line 4 (internal key)".to_string()),
        "19" => ("the nonrefundable child tax credit and credit for other dependents from Schedule 8812 (line 14): the smaller of the credit after the modified-AGI phaseout (Schedule 8812 line 12 — tentative credit of $2,200 per qualifying child under 17 plus $500 per other dependent, reduced by 5% of the AGI excess over $400,000 MFJ / $200,000 other, the excess rounded up to a $1,000 multiple) or the Credit Limit Worksheet A amount, which is line 16 in v1 (Form 1040 line 18 minus Schedule 3 nonrefundable credits, both zero here beyond line 16)".to_string(), "Form 1040, line 19; Schedule 8812, Part I and Credit Limit Worksheet A".to_string()),
        "sched8812_tentative" => ("Schedule 8812 line 8: $2,200 times the number of qualifying children (child-type relationship, under 17 at year-end, lived with the taxpayer more than half the year) plus $500 times the number of other dependents meeting the residency test (internal key)".to_string(), "Schedule 8812, lines 4-8 (internal key)".to_string()),
        "sched8812_credit_after_phaseout" => ("Schedule 8812 line 12: the tentative credit (internal key sched8812_tentative) minus the phaseout reduction of Schedule 8812 line 11 — 5% of the modified AGI (AGI in v1) over the filing-status threshold, rounded up to the next multiple of $1,000 (internal key)".to_string(), "Schedule 8812, lines 9-12 (internal key)".to_string()),
        "sched8812_earned_income" => ("earned income for the additional child tax credit (Schedule 8812 line 18a), v1 definition: total W-2 wages plus the Schedule C net profit when it is positive (internal key)".to_string(), "Schedule 8812, Part II-A, line 18a (internal key)".to_string()),
        "21" => ("line 19 plus Schedule 3, line 14 (other nonrefundable credits), which is zero in v1, so line 21 equals line 19".to_string(), "Form 1040, line 21".to_string()),
        "22" => ("line 16 minus line 21, floored at zero (nonrefundable credits cannot make the tax negative or offset line 23 taxes)".to_string(), "Form 1040, line 22".to_string()),
        "24" => ("line 22 plus line 23".to_string(), "Form 1040, line 24".to_string()),
        "28" => ("the additional child tax credit from Schedule 8812 Part II-A (line 27): the smallest of the credit left unused by line 19 (Schedule 8812 line 12 minus line 14), $1,700 per qualifying child, or 15% of earned income (internal key sched8812_earned_income) over $2,500; the Part II-B formula for three or more qualifying children is not implemented in v1".to_string(), "Form 1040, line 28; Schedule 8812, Part II-A".to_string()),
        "27" => ("the earned income credit (EIC), figured by EIC Worksheets A and B: count the qualifying children (internal key eic_qualifying_children; child/stepchild/foster child with an SSN stored, lived with the taxpayer more than half the year, under 19 — or under 24 if a full-time student — or permanently and totally disabled), take the TY2025 EIC Table credit for earned income (internal key eic_earned_income: wages plus Schedule SE line 3 net earnings minus the deductible part of SE tax); when AGI differs from earned income and is at or above the phaseout threshold, take the smaller of the earned-income and AGI table amounts. Zero when: filing status is MFS (the separated-spouse exception is not modeled), investment income (internal key eic_investment_income: lines 2a + 2b + 3b + the positive part of line 7) exceeds $11,950 (IRC 32(i)), AGI or earned income reaches the completed phaseout amount, earned income is not positive, or — with no qualifying children — no date of birth on file passes the age-25-to-64 test (v1 takes no credit rather than guessing)".to_string(), "Form 1040, line 27 (printed as 27a); Instructions for Form 1040 (2025), line 27, EIC Worksheets A and B; Rev. Proc. 2024-40, section 3.01".to_string()),
        "eic_earned_income" => ("earned income for the earned income credit (EIC Worksheets A and B): Form 1040 line 1z wages plus, for a self-employed filer, Schedule SE line 3 net earnings (the Schedule C net profit, NOT reduced by the 92.35% Schedule SE factor, which applies only to the SE tax) minus Schedule SE line 13, the deductible part of SE tax (line 10)".to_string(), "EIC Worksheets A and B (Instructions for Form 1040 (2025), line 27); Pub 596 (2025), chapter 3".to_string()),
        "eic_qualifying_children" => ("the number of stored dependents who are qualifying children for the earned income credit (relationship child/stepchild/foster child, SSN stored, lived with the taxpayer more than half the year, age under 19 — or under 24 if a full-time student — or permanently and totally disabled); expressed in the lines map as count x $1.00, not an amount of money".to_string(), "Instructions for Form 1040 (2025), line 27, Step 3".to_string()),
        "eic_investment_income" => ("investment income for the IRC 32(i) limit, scoped to the lines this engine computes: line 2a (tax-exempt interest counts) + line 2b + line 3b + the positive part of line 7 (capital gain net income); rental, royalty and passive-activity amounts do not exist among v1 inputs. Over $11,950 in TY2025, no earned income credit is allowed".to_string(), "IRC 32(i); Pub 596 (2025), chapter 2; Rev. Proc. 2024-40, section 3.01".to_string()),
        "32" => ("total other payments and refundable credits: line 27 (earned income credit) + line 28 (additional child tax credit) in v1 (lines 29/30/31 — American opportunity, premium tax credit, Schedule 3 line 15 — are not implemented)".to_string(), "Form 1040, line 32".to_string()),
        "33" => ("line 25d (total withheld) plus line 32 (refundable credits)".to_string(), "Form 1040, line 33".to_string()),
        "schd_net_capital_gain" => ("the QDCG worksheet's capital-gain input: the smaller of Schedule D lines 15 and 16 when both are gains, else 0".to_string(), "Qualified Dividends and Capital Gain Tax Worksheet, line 3 (Instructions for Form 1040)".to_string()),
        "schd_carryover_short" => ("short-term capital loss carried to next year per the Capital Loss Carryover Worksheet (the $3,000/$1,500 allowance is absorbed against short-term losses first)".to_string(), "Instructions for Schedule D, Capital Loss Carryover Worksheet".to_string()),
        "schd_carryover_long" => ("long-term capital loss carried to next year per the Capital Loss Carryover Worksheet".to_string(), "Instructions for Schedule D, Capital Loss Carryover Worksheet".to_string()),
        "34" => ("line 33 minus line 24 when payments exceed total tax, else 0".to_string(), "Form 1040, line 34".to_string()),
        "37" => ("line 24 minus line 33 when total tax exceeds payments, else 0".to_string(), "Form 1040, line 37".to_string()),
        _ => ("computed directly from stored inputs".to_string(), "Form 1040 instructions".to_string()),
    };

    let mut value = json!({
        "returnId": ret.id,
        "line": line_no,
        "result": line_json(&fr),
        "derivation": derivation,
        "citation": citation,
    });
    if line_no == "16" {
        value["line16_method"] = json!(view.line16_method);
    }
    let human = format!(
        "line {line_no}: {} — {derivation} [{citation}]",
        line_json(&fr)
    );
    Ok(Out::ok(value, human))
}

fn cmd_skills(cli: &Cli, action: SkillsAction) -> CResult<Out> {
    match action {
        SkillsAction::List => {
            let dir = &cli.skills_dir;
            if !dir.is_dir() {
                return Err(CliErr::new(
                    "skills_dir_missing",
                    format!("skills directory {} not found; run from the repo root or pass --skills-dir", dir.display()),
                ));
            }
            let mut names = Vec::new();
            for entry in std::fs::read_dir(dir).map_err(io_err)? {
                let entry = entry.map_err(io_err)?;
                if entry.path().join("SKILL.md").exists() {
                    names.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
            names.sort();
            let human = names.join("\n");
            Ok(Out::ok(json!({"skills": names}), human))
        }
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn run(cli: &Cli) -> CResult<Out> {
    match &cli.command {
        Commands::Doctor => cmd_doctor(cli),
        Commands::Selftest => cmd_selftest(),
        Commands::Schema { command } => {
            let full = schema_json();
            if let Some(name) = command {
                match full["inputs"].get(name) {
                    Some(v) => Ok(Out::ok(
                        json!({"input": name, "schema": v}),
                        serde_json::to_string_pretty(v).map_err(json_err)?,
                    )),
                    None => Err(CliErr::new(
                        "unknown_input",
                        format!("no input named '{name}'"),
                    )),
                }
            } else {
                let human = serde_json::to_string_pretty(&full).map_err(json_err)?;
                Ok(Out::ok(full, human))
            }
        }
        Commands::Node { action } => match action {
            NodeAction::List => cmd_node(NodeAction::List),
            NodeAction::Template { document } => cmd_node(NodeAction::Template {
                document: document.clone(),
            }),
        },
        Commands::Return { action } => match action {
            ReturnAction::Create { .. }
            | ReturnAction::Get { .. }
            | ReturnAction::Validate { .. }
            | ReturnAction::SetIdentity { .. }
            | ReturnAction::SetState { .. }
            | ReturnAction::SetNra { .. }
            | ReturnAction::SetOtherIncome { .. }
            | ReturnAction::List
            | ReturnAction::Export { .. } => {
                // Re-dispatch with ownership-free copies: simplest is to call a
                // helper that matches on the same enum by reference patterns
                // already handled here — instead, rebuild via a shim:
                cmd_return_ref(cli, action)
            }
        },
        Commands::Form { action } => match action {
            FormAction::Add { .. } => cmd_form_ref(cli, action),
        },
        Commands::Doc { action } => match action {
            DocAction::Classify { file } => cmd_doc(DocAction::Classify { file: file.clone() }),
            DocAction::Extract { file } => cmd_doc(DocAction::Extract { file: file.clone() }),
        },
        Commands::Pdf { action } => match action {
            PdfAction::Inspect { file } => cmd_pdf(PdfAction::Inspect { file: file.clone() }),
            PdfAction::Fill {
                input,
                out,
                data,
                backend,
            } => cmd_pdf(PdfAction::Fill {
                input: input.clone(),
                out: out.clone(),
                data: data.clone(),
                backend: backend.clone(),
            }),
        },
        Commands::Explain {
            r#return,
            line,
            fact,
            state,
        } => cmd_explain(cli, r#return, line.clone(), fact.clone(), state.clone()),
        Commands::Skills { action } => match action {
            SkillsAction::List => cmd_skills(cli, SkillsAction::List),
        },
    }
}

// The action enums are matched by reference at the top level; these shims
// keep handler signatures simple by re-matching with cloned fields.
fn cmd_return_ref(cli: &Cli, action: &ReturnAction) -> CResult<Out> {
    match action {
        ReturnAction::Create {
            year,
            status,
            interest_cents,
            dividends_cents,
        } => cmd_return(
            cli,
            ReturnAction::Create {
                year: *year,
                status: status.clone(),
                interest_cents: *interest_cents,
                dividends_cents: *dividends_cents,
            },
        ),
        ReturnAction::Get { r#return } => cmd_return(
            cli,
            ReturnAction::Get {
                r#return: r#return.clone(),
            },
        ),
        ReturnAction::Validate { r#return } => cmd_return(
            cli,
            ReturnAction::Validate {
                r#return: r#return.clone(),
            },
        ),
        ReturnAction::SetIdentity { r#return, file } => cmd_return(
            cli,
            ReturnAction::SetIdentity {
                r#return: r#return.clone(),
                file: file.clone(),
            },
        ),
        ReturnAction::SetState {
            r#return,
            state,
            file,
        } => cmd_return(
            cli,
            ReturnAction::SetState {
                r#return: r#return.clone(),
                state: state.clone(),
                file: file.clone(),
            },
        ),
        ReturnAction::SetNra { r#return, file } => cmd_return(
            cli,
            ReturnAction::SetNra {
                r#return: r#return.clone(),
                file: file.clone(),
            },
        ),
        ReturnAction::SetOtherIncome {
            r#return,
            interest_cents,
            dividends_cents,
        } => cmd_return(
            cli,
            ReturnAction::SetOtherIncome {
                r#return: r#return.clone(),
                interest_cents: *interest_cents,
                dividends_cents: *dividends_cents,
            },
        ),
        ReturnAction::List => cmd_return(cli, ReturnAction::List),
        ReturnAction::Export {
            r#return,
            export_type,
            template,
            out,
            forms_dir,
            out_dir,
        } => cmd_return(
            cli,
            ReturnAction::Export {
                r#return: r#return.clone(),
                export_type: export_type.clone(),
                template: template.clone(),
                out: out.clone(),
                forms_dir: forms_dir.clone(),
                out_dir: out_dir.clone(),
            },
        ),
    }
}

fn cmd_form_ref(cli: &Cli, action: &FormAction) -> CResult<Out> {
    match action {
        FormAction::Add {
            r#return,
            node_type,
            file,
            data,
        } => cmd_form(
            cli,
            FormAction::Add {
                r#return: r#return.clone(),
                node_type: node_type.clone(),
                file: file.clone(),
                data: data.clone(),
            },
        ),
    }
}

fn main() {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(out) => {
            if json_mode(&cli) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&out.value).unwrap_or_else(|_| "{}".to_string())
                );
            } else {
                println!("{}", out.human);
            }
            std::process::exit(out.exit_code);
        }
        Err(e) => {
            let mut obj = json!({"success": false, "error": e.message, "code": e.code});
            if let (Some(extra), Some(map)) = (e.extra, obj.as_object_mut()) {
                if let Some(extra_map) = extra.as_object() {
                    for (k, v) in extra_map {
                        map.insert(k.clone(), v.clone());
                    }
                }
            }
            eprintln!("{}", obj);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spike_identity() -> TaxpayerIdentity {
        TaxpayerIdentity {
            person: PersonIdentity {
                first_name: Some("Spike".to_string()),
                last_name: Some("Tester".to_string()),
                ssn: Some("123456789".to_string()),
                date_of_birth: Some("1958-05-01".to_string()),
                occupation: Some("Engineer".to_string()),
                blind: false,
            },
            address: None,
            spouse: Some(PersonIdentity {
                first_name: Some("Partner".to_string()),
                last_name: Some("Tester".to_string()),
                ssn: Some("987654321".to_string()),
                date_of_birth: Some("1960-11-15".to_string()),
                occupation: None,
                blind: true,
            }),
            can_be_claimed_as_dependent: false,
        }
    }

    #[test]
    fn masked_identity_output_never_shows_full_ssn() {
        let rendered = serde_json::to_string(&masked_identity_json(&spike_identity())).unwrap();
        assert!(
            !rendered.contains("123456789"),
            "taxpayer SSN leaked: {rendered}"
        );
        assert!(
            !rendered.contains("987654321"),
            "spouse SSN leaked: {rendered}"
        );
        assert!(rendered.contains("***-**-6789"), "{rendered}");
        assert!(rendered.contains("***-**-4321"), "{rendered}");
    }

    #[test]
    fn set_identity_normalizes_dashed_ssn() {
        let raw: TaxpayerIdentity = serde_json::from_str(
            r#"{"first_name":"Spike","ssn":"123-45-6789","spouse":{"ssn":"987-65-4321"}}"#,
        )
        .unwrap();
        let id = raw.normalized().unwrap();
        assert_eq!(id.person.ssn.as_deref(), Some("123456789"));
        assert_eq!(id.spouse.unwrap().ssn.as_deref(), Some("987654321"));
        let bad: TaxpayerIdentity = serde_json::from_str(r#"{"ssn":"123-45-678"}"#).unwrap();
        assert!(bad.normalized().is_err());
    }

    #[test]
    fn old_return_without_taxpayer_block_still_loads() {
        // A return.json written before the identity block existed.
        let text = r#"{
            "id": "r2025-1-1", "year": 2025, "status": "single",
            "entries": [],
            "other_income": {"interest": "0", "dividends": "0"}
        }"#;
        let ret: StoredReturn = serde_json::from_str(text).unwrap();
        assert!(ret.taxpayer.is_none());
        // And it reserializes without inventing the block.
        let out = serde_json::to_string(&ret).unwrap();
        assert!(!out.contains("taxpayer"), "{out}");
    }

    #[test]
    fn return_list_is_pii_free_with_two_returns() {
        let store = std::env::temp_dir().join(format!("opentax-list-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        let params = TaxParams::load_2025().unwrap();
        let complete = StoredReturn {
            id: "r2025-100-1".to_string(),
            year: 2025,
            status: "mfj".to_string(),
            entries: Vec::new(),
            other_income: Some(OtherIncome {
                interest: Money::ZERO,
                dividends: Money::ZERO,
            }),
            taxpayer: Some(spike_identity()),
            state: None,
            nra: None,
        };
        let partial = StoredReturn {
            id: "r2025-200-2".to_string(),
            year: 2025,
            status: "single".to_string(),
            entries: Vec::new(),
            other_income: None,
            taxpayer: None,
            state: None,
            nra: None,
        };
        assert!(save_return(&store, &complete).is_ok());
        assert!(save_return(&store, &partial).is_ok());
        let rows = list_returns(&store, &params);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], "r2025-100-1");
        assert_eq!(rows[0]["complete"], true);
        assert_eq!(rows[0]["identity_set"], true);
        assert_eq!(rows[1]["id"], "r2025-200-2");
        assert_eq!(rows[1]["complete"], false);
        assert_eq!(rows[1]["identity_set"], false);
        let rendered = serde_json::to_string(&rows).unwrap();
        assert!(!rendered.contains("Tester"), "last name leaked: {rendered}");
        assert!(!rendered.contains("123456789"), "SSN leaked: {rendered}");
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    fn set_other_income_completes_return_created_without_flags() {
        let store =
            std::env::temp_dir().join(format!("opentax-otherinc-done-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        let params = TaxParams::load_2025().unwrap();
        // A return as `return create` stores it when neither
        // --interest-cents nor --dividends-cents was passed.
        let ret = StoredReturn {
            id: "r2025-300-3".to_string(),
            year: 2025,
            status: "single".to_string(),
            entries: Vec::new(),
            other_income: None,
            taxpayer: None,
            state: None,
            nra: None,
        };
        assert!(save_return(&store, &ret).is_ok());
        // Before: the direct amounts are unrecorded, so the view stays
        // on the partial path — lines 2b/3b Incomplete, never zero.
        // (`.ok().expect(..)` because CliErr has no Debug for unwrap.)
        let before = compute_view(&ret, &params).ok().expect("compute before");
        assert!(!before.complete);
        assert!(matches!(
            before.lines.get("2b"),
            Some(FactResult::Incomplete)
        ));
        assert!(matches!(
            before.lines.get("3b"),
            Some(FactResult::Incomplete)
        ));
        // Record the amounts (0/0) after creation:
        let (stored, replaced) = set_other_income(&store, &ret.id, 0, 0)
            .ok()
            .expect("set other income");
        assert!(!replaced);
        // After: the same stored return computes complete.
        let after = compute_view(&stored, &params).ok().expect("compute after");
        assert!(after.complete, "still incomplete: {:?}", after.incomplete);
        assert!(matches!(
            after.lines.get("2b"),
            Some(FactResult::Complete(m)) if *m == Money::ZERO
        ));
        // And the recording persisted to disk.
        assert!(load_return(&store, &ret.id)
            .ok()
            .expect("reload")
            .other_income
            .is_some());
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    fn set_other_income_overwrite_reports_replaced() {
        let store =
            std::env::temp_dir().join(format!("opentax-otherinc-over-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        let ret = StoredReturn {
            id: "r2025-400-4".to_string(),
            year: 2025,
            status: "single".to_string(),
            entries: Vec::new(),
            other_income: None,
            taxpayer: None,
            state: None,
            nra: None,
        };
        assert!(save_return(&store, &ret).is_ok());
        let (_, replaced) = set_other_income(&store, &ret.id, 10_000, 20_000)
            .ok()
            .expect("first set");
        assert!(!replaced);
        let (stored, replaced) = set_other_income(&store, &ret.id, 30_000, 40_000)
            .ok()
            .expect("overwrite set");
        assert!(replaced);
        let other = stored.other_income.unwrap();
        assert_eq!(other.interest, Money::from_cents(30_000));
        assert_eq!(other.dividends, Money::from_cents(40_000));
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    fn set_other_income_rejects_negative_amounts() {
        let store =
            std::env::temp_dir().join(format!("opentax-otherinc-neg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        let ret = StoredReturn {
            id: "r2025-500-5".to_string(),
            year: 2025,
            status: "single".to_string(),
            entries: Vec::new(),
            other_income: None,
            taxpayer: None,
            state: None,
            nra: None,
        };
        assert!(save_return(&store, &ret).is_ok());
        let err = set_other_income(&store, &ret.id, -1, 0).unwrap_err();
        assert_eq!(err.code, "invalid_input");
        assert!(set_other_income(&store, &ret.id, 0, -10_000).is_err());
        // The rejected calls recorded nothing.
        assert!(load_return(&store, &ret.id)
            .ok()
            .expect("reload")
            .other_income
            .is_none());
        let _ = std::fs::remove_dir_all(&store);
    }
}
