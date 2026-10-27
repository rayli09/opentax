# opentax

An **agent-first, open-source US tax CLI** in Rust. The CLI supplies
deterministic functionality — calculation, validation, PDF inspection,
document checks — and **AI agents drive it** through a set of bundled
skills: interview → document ingest → compute → review → filing guidance.

The agent never does arithmetic. Every number on a return is produced by
tested, deterministic code, or transcribed verbatim from a source document
with its source recorded.

> **Status: scaffold.** A working vertical slice (TY2025 Form 1040 with
> W-2 income, bank/broker 1099s, 1099-NEC side income with Schedule
> C / Schedule SE, and dependents with the child tax credits via a
> simplified Schedule 8812), not a finished tax product. See
> `docs/KNOWN_LIMITS.md` for exactly what is missing. JSON export and
> filled-PDF export (amounts, filing status, and the Dependents table
> onto a user-supplied TY2025 1040 template) work; MeF e-file does not
> (it reports `not_implemented` honestly).

## Iron rules

1. The LLM never calculates. Quote engine output only.
2. Missing input is **incomplete**, never silently zero.
3. Every extracted number is a verbatim transcription with its source recorded.
4. The validator must pass before a computed return is trusted.
5. No credentials are ever handled; the user alone submits/files.
6. Everything is local by default. Documents do not leave the machine.

## Quickstart

```sh
cargo build --release          # binary: target/release/tax

tax doctor                     # environment + parameter-pack checks
tax selftest                   # runs the golden cases before anything else

tax return create --year 2025 --status single
# -> {"returnId": "r2025-..."}

cat > w2.json <<'JSON'
{"employer_ein":"12-3456789","wages":"5000000","federal_withholding":"400000"}
JSON
tax form add --return <id> --node-type w2 --file w2.json

tax return get --return <id>   # lines, completeness, warnings (JSON when piped)
tax return validate --return <id>
tax explain --return <id> --line 15
tax return export --return <id> --type json

tax node list
tax node template --document w2
tax doc classify --file some-document.pdf
tax skills list

# PDF form primitives (inspect any IRS form; fill it with an audited report)
tax pdf inspect f1040.pdf
cat > values.json <<'JSON'
{"topmostSubform[0].Page1[0].f1_15[0]": "Tester"}
JSON
tax pdf fill f1040.pdf --out f1040-filled.pdf --data values.json   # default backend: pdfer
```

Money in JSON is **integer cents as a string**: `"5000000"` is $50,000.00.
No floats anywhere in the filed path.

### The golden example

Single filer, one W-2, $50,000 wages, $4,000 withheld, nothing else:
AGI $50,000 − standard deduction $15,750 = taxable $34,250;
tax = 11,925×10% + 22,325×12% = **$3,871.50**; refund **$128.50**.
This case lives at `benchmark/cases/f1040/2025/001-single-w2-minimal/` and
`tax selftest` checks the engine against it.

## Layout

```text
crates/tax-core   Money (integer cents), fact-completeness model, expression
                  graph, per-year parameters as DATA (params/2025.json)
crates/tax-forms  Form 1040 TY2025 minimal slice + input types (W-2, 1099s)
crates/tax-pdf    AcroForm inspect (lopdf) + audited fill (pdfer_forms),
                  behind a PdfBackend trait
crates/tax-doc    Document classification, W-2 cross-checks, OCR/text traits
crates/tax-cli    The `tax` binary (clap; JSON on stdout when piped)
skills/           Router + intake / ingest / preparer / reviewer / filing-guide
benchmark/cases/  Golden cases with per-value source citations
docs/             ARCHITECTURE.md, KNOWN_LIMITS.md
```

## Design in one paragraph

Hybrid engine: per-year **parameters as data** + a serde **expression graph**
with IRS Fact Graph completeness semantics + hardcoded Rust modules for
procedural worksheets. Layered document pipeline: born-digital text first,
geometric templates second, agent vision as supervised fallback, OCR as a
pluggable trait — with a deterministic validation layer as the trust moat.
PDF form *filling* was the known weak link in pure Rust; the fill spike
(`docs/PDF_SPIKE.md`) settled it — pdfer_forms is the default fill backend,
with a post-fill audit that fails any fill touching unrequested fields.

License: MIT © 2026 Ray Li.
