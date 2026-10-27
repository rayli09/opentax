# Architecture

## Shape

```
        ┌───────────── AI agent (any) ─────────────┐
        │  skills: tax (router) → intake / ingest / │
        │  preparer / reviewer / filing-guide       │
        └──────────────────┬───────────────────────┘
                           │ CLI commands, JSON in/out
        ┌──────────────────▼───────────────────────┐
        │  tax-cli  (`tax` binary)                  │
        │  doctor · selftest · schema · node ·      │
        │  return · form · doc · explain · skills   │
        ├──────────┬──────────┬──────────┬──────────┤
        │ tax-forms│ tax-core │ tax-pdf  │ tax-doc  │
        │  1040    │ engine   │ AcroForm │ classify │
        │  slice   │ + params │ + fill   │ validate │
        └──────────┴──────────┴──────────┴──────────┘
              local state: .opentax/returns/<id>/
```

The CLI is the tool layer; the agent is the driver. The CLI never interviews
the user and never guesses — it computes, validates, inspects, and reports
completeness. The skills (in `skills/`) own the conversation and workflow.

## Engine: hybrid, three tiers

1. **Parameters as data** — brackets, standard deductions, thresholds live
   in `crates/tax-core/params/<year>.json` with a `source` citation. A new
   tax year is a data diff, reviewable in a PR.
2. **Line arithmetic as an expression graph** — `tax_core::Op`, a
   serde-tagged graph (`Add`, `Sub`, `Mul`, `Max`, `Min`, `Clamp`,
   `BracketTax`, `TaxTableQuantize`, `Input`, `Literal`), adapted from
   tenforty-graph's Rust runtime. Every evaluation returns
   `FactResult<T> { Complete, Incomplete, Placeholder }` — IRS Fact Graph
   semantics: a derived fact over missing inputs is Incomplete, never zero.
   Completeness is what turns the engine into an interview: the incomplete
   facts are exactly the next questions (`tax-intake` skill).
3. **Hardcoded Rust modules** for the procedural minority — worksheets,
   eligibility logic, carryforwards — registered alongside the graph so the
   executor cannot tell them apart.

Money is integer cents (`Money`, serialized as a string of cents). No
floats in the filed path. Validation is a **separate** layer from
computation (like MeF business rules), so it runs on partial returns.

## Document pipeline (layers 0–5)

- **L0 Ingest**: hash/dedupe, split multi-document PDFs, probe text layers.
- **L1 Born-digital text** (primary): extract text with positions; classify
  by printed form title; per-form, per-year positional templates anchored
  on box labels. Most W-2s/1099s from payroll/brokerages live here, exactly.
  Shipped (`tax doc extract`, W-2 / 1099-INT / 1099-DIV / 1099-NEC):
  label-anchored geometric rules rather than fixed per-issuer templates —
  an AcroForm pass for fillable copies, then the text layer with
  box-number-preferred label hits and bounded value-search regions.
- **L2 OCR fallback** (pages with no usable text layer): pluggable
  `OcrBackend` trait in tax-doc. v1 ships `NoOcr` (reports unavailable);
  ocrs (pure Rust) / external Tesseract or PaddleOCR-ONNX slot in later.
- **L3 Agent vision**: the CLI renders pages and supplies schema + grounded
  text; the driving (multimodal) agent returns values that must cite a
  source span. Used for scans, photos, and untemplated layouts (e.g. K-1).
- **L4 Deterministic validation** (the moat): intra-form math (W-2 Box 4 ≈
  6.2% of Box 3 up to the wage base, Box 6 ≈ 1.45% of Box 5), table
  reconciliation (1099-B rows must sum to printed summary totals),
  cross-document consistency, grounding checks. The validator never
  silently fixes a number.
- **L5 Review & export**: annotated pages, a ranked flag list, and export
  of the computed return (JSON today; filled PDF and MeF XML next).

## PDF filling: spike verdict → implemented decision

Reading IRS PDFs and inspecting AcroForm fields is solved in pure Rust on
`lopdf`. **Filling** was the weak link: setting `/V` without regenerating
the widget appearance stream (`/AP`) produces forms that render blank in
viewers that do not regenerate appearances.

**The spike ran** (docs/PDF_SPIKE.md; real TY2025 f1040 / Schedule D /
Form 8949, 57 target fields): the lopdf `/V`-only path failed twice —
Designer UTF-16BE field names were decoded lossily (100% mangled,
0/57 filled), and even with names worked around it created no `/AP`,
wrote checkbox `/V` as a spec-invalid *string*, and poisoned other
parsers. `NeedAppearances=true` changed nothing. pdfer_forms and pdfcpu
both produced forms that read back AND render (57/57 + `/AP`).

**Decision (implemented):**

- **PDF string decoding fixed** in tax-pdf: `/T` and string `/V` decode
  as PDF text strings (UTF-16BE/LE with BOM, else PDFDocEncoding); PDF
  *name* objects decode through a separate helper. Inspect is exact on
  real IRS forms again (199/55/202 terminal fields, clean names).
- **`PdferBackend` is the default fill backend** behind `PdfBackend`
  (lopdf stays the inspect default and an honest `/V`-only fallback).
  Fills return an audited `FillReport`: pre-flight name resolution +
  duplicate-leaf warnings, post-fill diff of every field's `/V` — any
  unrequested change fails the fill and no output is written (this
  catches pdfer_forms' leaf-name over-matching, e.g. Form 1040's two
  `c1_8[0]` filing-status fields; those fills are blocked until an
  id-based path exists — see KNOWN_LIMITS).
- **XFA handling is explicit**: preserved as-is, with a warning in every
  FillReport; never silently stripped (pdfcpu's choice) or updated.
- Verification stays an independent path (independent readback + render),
  and pdfcpu stays the external reference tool in the spike harness.
- `anydoc` (Firecrawl) remains rejected as a core: it converts documents
  to Markdown with hosted-only OCR and discards AcroForm structure and
  box geometry — the opposite of what tax extraction needs.

## The MeF moat (why v1 exports, and does not transmit)

US e-file transmission (IRS Modernized e-File) requires a firm-level
EFIN/ETIN, principal suitability checks/fingerprinting, annual MeF
Assurance Testing per form family, and separate per-state approvals.
Credentials are non-transferable: an open-source CLI cannot ship one.
Therefore v1 produces a correct computed return + export artifacts and the
filing-guide skill walks the user through filing channels (Free File,
fillable forms, paper, or a future authorized-transmitter partnership).
Transmitter support, when it comes, will use the *user's own* credentials.

## CLI conventions for agents

- stdout is JSON when piped or `--output json`; human tables on a TTY.
  Progress/diagnostics go to stderr. Errors: `{"success":false,"error","code"}`
  on stderr + exit 1.
- Schemas and templates are served by the binary (`tax schema`,
  `tax node template`), and skills are served by the binary
  (`tax skills list`) — installed skill files are discovery stubs, so docs
  cannot drift from the installed version.
- State is a local directory (`.opentax/`), every mutating command is
  re-runnable, and partial returns are first-class.
- `tax mcp serve` (stdio, same handlers) is a planned second surface.
