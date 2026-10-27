# Known limits (scaffold, TY2025)

This list is the contract: anything not listed as working is not claimed.

## Compute (tax-forms)

- Only Form 1040, only these lines: 1a, 1z, 2a, 2b, 3a, 3b, 7, 9, 11, 12,
  15, 16, 25a, 25d, 33, 34, 37 — plus two internal keys in the computed
  lines map, `schd_net_short` / `schd_net_long` (net short-/long-term
  gain or loss from 1099-B), which feed a future Schedule D and are not
  Form 1040 line numbers.
- Income sources: W-2 wages; 1099-INT (Box 1 → line 2b, Box 8
  tax-exempt interest → line 2a, Box 4 withholding → line 25d);
  1099-DIV (Box 1a → line 3b, Box 1b qualified → line 3a, Box 4 →
  line 25d); 1099-B (per-transaction Box 1d proceeds − Box 1e basis +
  Box 1g adjustment as reported, aggregated by term → line 7). A 1099-B
  transaction missing proceeds or basis makes the capital-gain lines
  (7, both `schd_*` keys, and everything downstream) Incomplete, never
  zero. Wash-sale and other adjustments are taken as reported in
  Box 1g — they are not computed. No Schedule C/E, no K-1, no crypto,
  and Schedule D itself is not produced: line 7 is the direct net of
  the reported 1099-B transactions, with no Schedule D worksheet,
  netting rules beyond the short/long split, or loss limits.
- Line 16 is **bracket tax** on taxable income for all amounts, named
  honestly: the IRS Tax Table (required below $100,000) and the Qualified
  Dividends and Capital Gain Tax Worksheet are NOT implemented in v1, so
  qualified dividends (line 3a) and capital gains (line 7) are taxed at
  ordinary rates here, and line 16 differs from a filed return wherever
  those would apply.
- Line 12 is the standard deduction only; no itemizing, no additional
  amounts (age/blindness), no QBI deduction (line 13).
- AGI = total income: no adjustments (student loan interest, HSA, SE tax…).
- No credits (CTC, EITC, education, foreign), no AMT, no NIIT, no other taxes.
- Line 33 is simplified to equal 25d (withholding only).
- No state returns.
- Filing statuses: single, mfj, hoh, mfs parameters are included; mfs
  special rules are not modeled.

## Documents (tax-doc)

- Classification is title matching on extracted text only.
- W-2 validation implements the Social Security and Medicare rate checks
  only, and only when Boxes 3–6 were provided.
- No OCR ships in v1 (`NoOcr` reports unavailable). Scanned documents need
  the agent-vision path described in the ingest skill.
- Text extraction is lopdf's crude built-in text dump: no positions, no
  tables, reading order not guaranteed.

## PDF (tax-pdf)

- Field-name decoding is FIXED: `/T` and string `/V` are decoded as PDF
  text strings (UTF-16BE/LE with BOM, else PDFDocEncoding) — the spike had
  found 100% of real IRS field names mangled by lossy UTF-8 decoding
  (docs/PDF_SPIKE.md). Verified on the real TY2025 f1040.pdf: 199 terminal
  fields, zero replacement characters.
- Default fill backend is **pdfer_forms** (regenerates `/AP` appearance
  streams, correct checkbox name/`/AS` handling), behind the `PdfBackend`
  trait, with a post-fill audit: any field that changed without being
  requested fails the fill and **no output file is written**. The lopdf
  backend remains the inspect default and an honest `/V`-only fallback
  (its FillReport says so and reports `appearance_verified: 0`).
- **Duplicate-leaf fills are blocked by that audit, by design.** pdfer_forms
  also matches on leaf names, so when two fields share a leaf the fill
  over-matches and the audit refuses the output. Concretely: Form 1040's
  five filing-status fields include two with leaf `c1_8[0]` (Single under
  `Checkbox_ReadOrder[0]`, HOH as a direct Page1 child) — filling Single
  also sets HOH, the audit fails it, and **filing status cannot be filled
  on f1040 via this path yet**. Needs an id-based fill or a post-fill
  repair pass; future work, not hidden.
- **Comb appearances under pdfer_forms are crammed, not cell-spread**
  (data-correct; the regenerated appearance ignores the comb flag —
  pdfcpu spreads correctly). Filling a comb field adds a warning to the
  FillReport naming it.
- **XFA is preserved, never updated.** IRS forms are XFA+AcroForm hybrids;
  AcroForm fills do not touch the XFA datasets, so viewers that prefer the
  XFA layer (Acrobat's default) may show stale data. Every FillReport
  carries this warning. The alternative (stripping `/XFA` like pdfcpu)
  was considered and rejected as a silent default.
- Checkbox *rendering* is still not verifiable with poppler (it does not
  render Designer radio on-states for any tool, including a hand-built
  control); checkbox correctness rests on data-level `/V` name + `/AS`
  state, which pdfer_forms gets right (spike evidence).
- pdfer_forms 0.3.x pulls lopdf ^0.40 alongside this crate's lopdf 0.46.
  Accepted: `PdferBackend` works on file paths only and never shares
  `Document` objects across the boundary.
- PDF *export of a computed return* is still `not_implemented` — the
  blocker is now the 1040 line→field mapping (the next step), not the
  fill backend. `tax pdf inspect|fill` primitives work on real forms.

## Filing

- No e-file. MeF export returns `not_implemented`. JSON export only.
- No IRS/state credential handling of any kind, by design (v1).

## CLI

- `tax schema` returns a hand-maintained structured description. It is
  real JSON and versioned, but it is not yet generated from the Rust
  structs via schemars; generation replaces the static version later.
- `tax skills list` reads `./skills` (override with `--skills-dir`).
- Return store is plain JSON files under `.opentax/` — no encryption at
  rest yet. Treat the directory as sensitive (it is; it contains tax data).
