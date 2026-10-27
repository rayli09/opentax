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
use tax_forms::{compute_1040, Form1040Input, W2Input};
use tax_pdf::{LopdfBackend, PdfBackend};

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

fn compute_view(ret: &StoredReturn, params: &TaxParams) -> CResult<ComputedView> {
    let w2s = stored_w2s(ret)?;
    let status = FilingStatus::parse(&ret.status).map_err(|e| CliErr::new("invalid_status", e))?;
    let mut warnings = Vec::new();

    if w2s.is_empty() && ret.other_income.is_none() {
        let all = [
            "1a", "1z", "2b", "3b", "9", "11", "12", "15", "16", "25a", "25d", "33", "34", "37",
        ];
        let lines: BTreeMap<String, FactResult<Money>> = all
            .iter()
            .map(|l| (l.to_string(), FactResult::Incomplete))
            .collect();
        return Ok(ComputedView {
            incomplete: all.iter().map(|s| s.to_string()).collect(),
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
            };
            let computed = compute_1040(&input, params);
            warnings.extend(computed.notes.iter().cloned());
            Ok(ComputedView {
                complete: computed.complete,
                incomplete: Vec::new(),
                lines: computed.lines,
                warnings,
            })
        }
        None => {
            // W-2s are known; interest/dividends are NOT recorded, so every
            // line that depends on them is Incomplete — never zero-filled.
            let wages: Money = w2s.iter().map(|w| w.wages).sum();
            let withholding: Money = w2s.iter().map(|w| w.federal_withholding).sum();
            let deduction = params.standard_deduction(status);
            let mut lines: BTreeMap<String, FactResult<Money>> = BTreeMap::new();
            lines.insert("1a".into(), FactResult::Complete(wages));
            lines.insert("1z".into(), FactResult::Complete(wages));
            lines.insert("12".into(), FactResult::Complete(deduction));
            lines.insert("25a".into(), FactResult::Complete(withholding));
            let incomplete = ["2b", "3b", "9", "11", "15", "16", "25d", "33", "34", "37"];
            for l in incomplete {
                lines.insert(l.to_string(), FactResult::Incomplete);
            }
            warnings.push(
                "interest_income and ordinary_dividends have not been recorded for this \
                 return (create it with --interest-cents/--dividends-cents, or supply the \
                 full Form1040Input to the engine); dependent lines are incomplete, not zero"
                    .to_string(),
            );
            Ok(ComputedView {
                incomplete: incomplete.iter().map(|s| s.to_string()).collect(),
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
    let backend = LopdfBackend;
    push(
        "pdf_backend",
        true,
        true,
        format!(
            "{} (pure Rust; AcroForm /AP regeneration is a known limit)",
            backend.name()
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
    // Prefer the on-disk benchmark (so edited cases are what run); fall back
    // to the copies embedded at build time (same files).
    let case_dir = Path::new("benchmark/cases/f1040/2025/001-single-w2-minimal");
    let (input_text, correct_text, origin) = if case_dir.join("input.json").exists() {
        (
            std::fs::read_to_string(case_dir.join("input.json")).map_err(io_err)?,
            std::fs::read_to_string(case_dir.join("correct.json")).map_err(io_err)?,
            case_dir.display().to_string(),
        )
    } else {
        (
            GOLDEN_INPUT.to_string(),
            GOLDEN_CORRECT.to_string(),
            "embedded benchmark/cases/f1040/2025/001-single-w2-minimal".to_string(),
        )
    };
    let (pass, mismatches) = run_golden(&input_text, &correct_text)?;
    let value = json!({
        "cases": [{"case": "f1040/2025/001-single-w2-minimal", "origin": origin, "pass": pass, "mismatches": mismatches}],
        "pass": pass,
    });
    let human = if pass {
        "selftest: 1/1 golden cases pass".to_string()
    } else {
        format!("selftest: FAIL\n{}", mismatches.join("\n"))
    };
    Ok(Out {
        value,
        human,
        exit_code: if pass { 0 } else { 1 },
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
                "ordinary_dividends": {"type": "money_cents_string"}
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
                "interest_income": {"type": "money_cents_string", "form": "1099-INT Box 1"},
                "federal_withholding": {"type": "money_cents_string", "form": "1099-INT Box 4"}
            },
            "I1099DivInput": {
                "payer_name": {"type": "string"},
                "total_ordinary_dividends": {"type": "money_cents_string", "form": "1099-DIV Box 1a"},
                "qualified_dividends": {"type": "money_cents_string", "form": "1099-DIV Box 1b"},
                "federal_withholding": {"type": "money_cents_string", "form": "1099-DIV Box 4"}
            },
            "I1099BInput": {
                "payer_name": {"type": "string"},
                "total_proceeds": {"type": "money_cents_string", "form": "1099-B Box 1d total"},
                "total_basis": {"type": "money_cents_string", "form": "1099-B Box 1e total"},
                "federal_withholding": {"type": "money_cents_string", "form": "1099-B Box 4"}
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
            false,
            json!({"payer_name": "", "interest_income": "0", "federal_withholding": "0"}),
        ),
        (
            "1099-div".to_string(),
            false,
            json!({"payer_name": "", "total_ordinary_dividends": "0", "qualified_dividends": "0", "federal_withholding": "0"}),
        ),
        (
            "1099-b".to_string(),
            false,
            json!({"payer_name": "", "total_proceeds": "0", "total_basis": "0", "federal_withholding": "0"}),
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
                "mef" | "pdf" => Ok(Out::ok(
                    json!({"status": "not_implemented", "format": export_type,
                            "detail": "MeF XML and filled-PDF export are not implemented in v1; no file was produced. JSON export is the supported artifact."}),
                    format!("{export_type} export: not implemented in v1 (no file produced)"),
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
            if node_type != "w2" {
                return Ok(Out::ok(
                    json!({"status": "not_implemented", "node_type": node_type,
                            "detail": "in v1 only w2 entries can be added and computed; 1099 templates exist for the ingest contract (see `tax node list`)"}),
                    format!("adding {node_type}: not implemented in v1"),
                ));
            }
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
            let _w2: W2Input = serde_json::from_value(value.clone()).map_err(json_err)?;
            let mut ret = load_return(&cli.store, &r#return)?;
            let n = ret.entries.iter().filter(|e| e.node_type == "w2").count() + 1;
            let entry_id = format!("w2_{n:02}");
            ret.entries.push(StoredEntry {
                entry_id: entry_id.clone(),
                node_type: "w2".to_string(),
                data: value,
            });
            save_return(&cli.store, &ret)?;
            Ok(Out::ok(
                json!({"returnId": ret.id, "entryId": entry_id, "node_type": "w2"}),
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

fn cmd_explain(cli: &Cli, id: &str, line: Option<String>, fact: Option<String>) -> CResult<Out> {
    let fact_map: &[(&str, &str)] = &[
        ("wages", "1a"),
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
        "9" => ("line 1z + line 2b + line 3b".to_string(), "Form 1040, line 9".to_string()),
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
