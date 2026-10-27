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
use tax_core::{FactResult, FilingStatus, Money, TaxParams};
use tax_doc::{LopdfTextExtractor, TextExtractor};
use tax_forms::{
    compute_1040, validate_1099_div, Form1040Input, I1099BInput, I1099DivInput, I1099IntInput,
    W2Input,
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
    /// Export a stored return. json works; mef/pdf report not_implemented.
    Export {
        #[arg(long)]
        r#return: String,
        #[arg(long = "type", default_value = "json")]
        export_type: String,
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
}

impl CliErr {
    fn new(code: &str, message: impl Into<String>) -> CliErr {
        CliErr {
            code: code.to_string(),
            message: message.into(),
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

/// Aggregate the 1099-B entries of a partial return the same way the
/// engine does: (net_short, net_long, capital_gain) facts — Incomplete
/// when any transaction is missing proceeds or basis — plus the total
/// 1099-B withholding.
fn partial_b_nets(
    bs: &[I1099BInput],
) -> (
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
            withholding,
        )
    } else {
        (
            FactResult::Complete(net_short),
            FactResult::Complete(net_long),
            FactResult::Complete(net_short + net_long),
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
    "9",
    "11",
    "12",
    "15",
    "16",
    "25a",
    "25d",
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

fn compute_view(ret: &StoredReturn, params: &TaxParams) -> CResult<ComputedView> {
    let w2s = stored_w2s(ret)?;
    let ints = stored_ints(ret)?;
    let divs = stored_divs(ret)?;
    let bs = stored_bs(ret)?;
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
            };
            let computed = compute_1040(&input, params);
            warnings.extend(computed.notes.iter().cloned());
            Ok(ComputedView {
                incomplete: incomplete_lines(&computed.lines),
                complete: computed.complete,
                lines: computed.lines,
                warnings,
            })
        }
        None => {
            // Documents are known; the *direct* interest/dividend amounts
            // are NOT recorded, so every line that depends on them is
            // Incomplete — never zero-filled. Lines that depend only on
            // stored documents are computed from them.
            let wages: Money = w2s.iter().map(|w| w.wages).sum();
            let w2_withholding: Money = w2s.iter().map(|w| w.federal_withholding).sum();
            let deduction = params.standard_deduction(status);
            let mut lines: BTreeMap<String, FactResult<Money>> = BTreeMap::new();
            lines.insert("1a".into(), FactResult::Complete(wages));
            lines.insert("1z".into(), FactResult::Complete(wages));
            lines.insert("12".into(), FactResult::Complete(deduction));
            lines.insert("25a".into(), FactResult::Complete(w2_withholding));
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
                let (ns, nl, cap, wh) = partial_b_nets(&bs);
                lines.insert("schd_net_short".into(), ns);
                lines.insert("schd_net_long".into(), nl);
                lines.insert("7".into(), cap);
                b_withholding = wh;
            }
            if !ints.is_empty() || !divs.is_empty() || !bs.is_empty() {
                // Withholding on every stored document is recorded, so
                // the withholding totals are computable from the stored set.
                let int_wh: Money = ints.iter().map(|f| f.federal_withholding).sum();
                let div_wh: Money = divs.iter().map(|f| f.federal_withholding).sum();
                let total_wh = w2_withholding + int_wh + div_wh + b_withholding;
                lines.insert("25d".into(), FactResult::Complete(total_wh));
                lines.insert("33".into(), FactResult::Complete(total_wh));
            }
            for l in ALL_LINES {
                lines.entry(l.to_string()).or_insert(FactResult::Incomplete);
            }
            warnings.push(
                "interest_income and ordinary_dividends have not been recorded for this \
                 return (create it with --interest-cents/--dividends-cents, or supply the \
                 full Form1040Input to the engine); dependent lines are incomplete, not zero"
                    .to_string(),
            );
            Ok(ComputedView {
                incomplete: incomplete_lines(&lines),
                lines,
                complete: false,
                warnings,
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
    json!({
        "lines": lines,
        "complete": view.complete,
        "incomplete": view.incomplete,
        "warnings": view.warnings,
    })
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

/// (case directory name, embedded input.json, embedded correct.json).
const EMBEDDED_CASES: &[(&str, &str, &str)] = &[
    ("001-single-w2-minimal", GOLDEN_INPUT, GOLDEN_CORRECT),
    ("002-w2-1099s", CASE002_INPUT, CASE002_CORRECT),
];

fn run_golden(input_text: &str, correct_text: &str) -> CResult<(bool, Vec<String>)> {
    let input: Form1040Input = serde_json::from_str(input_text).map_err(json_err)?;
    let correct: Value = serde_json::from_str(correct_text).map_err(json_err)?;
    let params = TaxParams::load_2025().map_err(json_err)?;
    let computed = compute_1040(&input, &params);
    let mut mismatches = Vec::new();
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

    let mut results: Vec<Value> = Vec::new();
    let mut all_pass = true;
    let mut all_mismatches: Vec<String> = Vec::new();
    for (name, input, correct, origin) in &cases {
        let (pass, mismatches) = run_golden(input, correct)?;
        if !pass {
            all_pass = false;
            all_mismatches.extend(
                mismatches
                    .iter()
                    .map(|m| format!("{name}: {m}"))
                    .collect::<Vec<_>>(),
            );
        }
        results.push(json!({
            "case": format!("f1040/2025/{name}"),
            "origin": origin,
            "pass": pass,
            "mismatches": mismatches,
        }));
    }
    let human = if all_pass {
        format!(
            "selftest: {}/{} golden cases pass",
            results.len(),
            cases.len()
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
                "f1099_bs": {"type": "array", "items": "I1099BInput"}
            },
            "W2Input": {
                "employer_ein": {"type": "string", "example": "12-3456789"},
                "wages": {"type": "money_cents_string", "form": "W-2 Box 1"},
                "federal_withholding": {"type": "money_cents_string", "form": "W-2 Box 2"},
                "social_security_wages": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 3"},
                "social_security_tax": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 4"},
                "medicare_wages": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 5"},
                "medicare_tax": {"type": "money_cents_string", "optional": true, "form": "W-2 Box 6"}
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
                   "medicare_wages": "0", "medicare_tax": "0"}),
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
            json!({"payer_name": "", "transactions": [{"description": "", "proceeds": "0", "cost_basis": "0", "adjustment": "0", "term": "short"}], "federal_withholding": "0"}),
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
            let human = {
                let mut s = format!("return {} — complete: {}\n", ret.id, view.complete);
                for (line, fr) in &view.lines {
                    s.push_str(&format!("  line {:>3}: {}\n", line, line_json(fr)));
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
            let mut findings: Vec<Value> = Vec::new();
            if ret.entries.is_empty() {
                findings.push(json!({"code": "return_empty", "severity": "error",
                    "message": "no documents have been added to this return", "field": null}));
            }
            if ret.other_income.is_none() {
                findings.push(json!({"code": "other_income_not_recorded", "severity": "warning",
                    "message": "interest/dividend income not recorded; totals cannot be validated as complete", "field": null}));
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
        ReturnAction::Export {
            r#return,
            export_type,
        } => {
            let ret = load_return(&cli.store, &r#return)?;
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
                "mef" => Ok(Out::ok(
                    json!({"status": "not_implemented", "format": export_type,
                            "detail": "MeF XML export is not implemented in v1; no file was produced. JSON export is the supported artifact."}),
                    "mef export: not implemented in v1 (no file produced)".to_string(),
                )),
                "pdf" => Ok(Out::ok(
                    json!({"status": "not_implemented", "format": export_type,
                            "detail": "Filled-PDF export of a computed return is not implemented in v1: the 1040 line-to-field mapping is the next step. The fill primitive itself works — see `tax pdf fill` (pdfer_forms backend). No file was produced. JSON export is the supported artifact."}),
                    "pdf export: not implemented in v1 (line-to-field mapping is the next step; no file produced)".to_string(),
                )),
                other => Err(CliErr::new(
                    "invalid_export_type",
                    format!("unknown export type '{other}' (use json|mef|pdf)"),
                )),
            }
        }
    }
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
                _ => {
                    return Ok(Out::ok(
                        json!({"status": "not_implemented", "node_type": node_type,
                                "detail": "v1 stores and computes w2, 1099-int, 1099-div, and 1099-b entries (see `tax node list`)"}),
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

fn cmd_explain(cli: &Cli, id: &str, line: Option<String>, fact: Option<String>) -> CResult<Out> {
    let fact_map: &[(&str, &str)] = &[
        ("wages", "1a"),
        ("tax_exempt_interest", "2a"),
        ("taxable_interest", "2b"),
        ("qualified_dividends", "3a"),
        ("ordinary_dividends", "3b"),
        ("capital_gain", "7"),
        ("total_income", "9"),
        ("agi", "11"),
        ("standard_deduction", "12"),
        ("taxable_income", "15"),
        ("tax", "16"),
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
        "7" => ("net short-term gain/loss plus net long-term gain/loss from stored 1099-B transactions (internal keys schd_net_short / schd_net_long)".to_string(), "Form 1040, line 7 (capital gain or loss, via Schedule D)".to_string()),
        "schd_net_short" => ("sum of (proceeds - basis + adjustment) over short-term 1099-B transactions".to_string(), "Form 8949 / Schedule D aggregation (internal key)".to_string()),
        "schd_net_long" => ("sum of (proceeds - basis + adjustment) over long-term 1099-B transactions".to_string(), "Form 8949 / Schedule D aggregation (internal key)".to_string()),
        "9" => ("line 1z + line 2b + line 3b + line 7".to_string(), "Form 1040, line 9".to_string()),
        "11" => ("line 9 minus adjustments — v1 has no adjustments, so AGI = line 9".to_string(), "Form 1040, line 11".to_string()),
        "12" => (format!("standard deduction for filing status '{}' from params/2025.json", ret.status), params.source.clone()),
        "15" => ("line 11 minus line 12, floored at zero".to_string(), "Form 1040, line 15".to_string()),
        "16" => ("bracket tax on line 15 (ordinary brackets, params/2025.json); Tax Table/QDCG worksheet not implemented in v1".to_string(), params.source.clone()),
        "34" => ("line 33 minus line 16 when payments exceed tax, else 0".to_string(), "Form 1040, line 34".to_string()),
        "37" => ("line 16 minus line 33 when tax exceeds payments, else 0".to_string(), "Form 1040, line 37".to_string()),
        _ => ("computed directly from stored inputs".to_string(), "Form 1040 instructions".to_string()),
    };

    let value = json!({
        "returnId": ret.id,
        "line": line_no,
        "result": line_json(&fr),
        "derivation": derivation,
        "citation": citation,
    });
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
        } => cmd_explain(cli, r#return, line.clone(), fact.clone()),
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
        ReturnAction::Export {
            r#return,
            export_type,
        } => cmd_return(
            cli,
            ReturnAction::Export {
                r#return: r#return.clone(),
                export_type: export_type.clone(),
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
            eprintln!(
                "{}",
                json!({"success": false, "error": e.message, "code": e.code})
            );
            std::process::exit(1);
        }
    }
}
