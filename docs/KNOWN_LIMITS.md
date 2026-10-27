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
- **Duplicate-leaf *text* fills are blocked by that audit, by design.**
  pdfer_forms also matches on leaf names, so when two non-button fields
  share a leaf the fill over-matches and the audit refuses the output.
- **Button fields (checkboxes, radios) no longer go through pdfer_forms.**
  They are filled by a direct lopdf pass inside `PdferBackend` that
  resolves the EXACT qualified name, sets the field's `/V` to the
  on-state as a proper name, and selects the widget carrying that state
  via `/AS` (all other widgets of the field get `/AS = /Off`). Designer
  widgets already carry their on-state `/AP` templates, so no appearance
  streams are generated in this path. A requested on-state that the
  field's widgets do not offer is an error naming the available states.
  Verified on the real TY2025 f1040: filling filing-status Single
  (`…Checkbox_ReadOrder[0].c1_8[0]` = `/1`) plus one text field changed
  exactly those two fields — the same-leaf sibling
  (`…Page1[0].c1_8[0]`, state `/4`) stayed `/Off`, where pdfer's leaf
  matching had previously set it and tripped the audit.
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
  state, which the direct button pass sets and the post-fill audit +
  independent readback verify (see above).
- pdfer_forms 0.3.x pulls lopdf ^0.40 alongside this crate's lopdf 0.46.
  Accepted: `PdferBackend` works on file paths only and never shares
  `Document` objects across the boundary.
- PDF *export of a computed return* is implemented:
  `tax return export --return <id> --type pdf --template <blank TY2025
  f1040.pdf> --out <file>` fills a user-supplied TY2025 Form 1040
  template through the audited fill backend, using the geometrically
  verified line→field map in `crates/tax-forms/forms/f1040-2025-field-map.json`
  (widget rectangles matched against the printed line labels; the map
  file records the method and per-field evidence). Scope: computed
  amount lines + the filing-status checkbox only. Identity fields
  (name/SSN/address) are NOT filled — they are not stored in a return —
  and are reported under `skipped`. Amounts are written as exact
  dollars-and-cents (e.g. `4231.50`); whole-dollar rounding is NOT
  applied in v1. TY2025 layout notes: AGI is written to both 11a and
  11b (the form repeats it on page 2), the standard deduction to 12e
  and to line 14 (= 12e + 13a + 13b, and the engine computes no
  13a/13b), and total tax to both 16 and 24 (= 22 + 23, no 17–23
  amounts). Schedules are not exported. An incomplete return is
  refused (`return_incomplete`, no file written).

## Filing

- No e-file. MeF export returns `not_implemented`. JSON and filled-PDF
  export only (PDF scope above).
- No IRS/state credential handling of any kind, by design (v1).

## CLI

- `tax schema` returns a hand-maintained structured description. It is
  real JSON and versioned, but it is not yet generated from the Rust
  structs via schemars; generation replaces the static version later.
- `tax skills list` reads `./skills` (override with `--skills-dir`).
- Return store is plain JSON files under `.opentax/` — no encryption at
  rest yet. Treat the directory as sensitive (it is; it contains tax data).
