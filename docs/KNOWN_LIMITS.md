# Known limits (scaffold, TY2025)

This list is the contract: anything not listed as working is not claimed.

## Compute (tax-forms)

- Only Form 1040, only these lines: 1a, 1z, 2a, 2b, 3a, 3b, 7, 8, 9, 10,
  11, 12, 15, 16, 23, 24, 25a, 25d, 33, 34, 37 — plus internal keys in
  the computed lines map, which are not Form 1040 line numbers:
  `schd_net_short` / `schd_net_long` (net short-/long-term gain or loss
  from 1099-B), `schd_net_capital_gain` (the QDCG worksheet's
  capital-gain input), `schd_carryover_short` / `schd_carryover_long`
  (capital loss carryovers to the next year, as positive amounts),
  `schc_net_profit` (Schedule C net profit from 1099-NEC engagements),
  `schedse_net_earnings` (Schedule SE line 4a net earnings), and
  `sched2_se_tax` (the Schedule SE tax entering Schedule 2 line 4,
  equal to line 23).
- Income sources: W-2 wages; 1099-INT (Box 1 → line 2b, Box 8
  tax-exempt interest → line 2a, Box 4 withholding → line 25d);
  1099-DIV (Box 1a → line 3b, Box 1b qualified → line 3a, Box 4 →
  line 25d); 1099-B (per-transaction Box 1d proceeds − Box 1e basis +
  Box 1g adjustment as reported, aggregated by term); 1099-NEC (Box 1 →
  Schedule C → line 8, Box 4 withholding → line 25d). A 1099-B
  transaction missing proceeds or basis makes the capital-gain lines
  (7, the `schd_*` keys, and everything downstream) Incomplete, never
  zero. Wash-sale and other adjustments are taken as reported in
  Box 1g — they are not computed. No Schedule E, no K-1, no crypto.
- Schedule D is computed as the simplified two-net combination, not
  produced as a form: line 7 = Schedule D line 16 when it is a gain;
  when it is a loss, the Schedule D line 21 amount — the loss limited
  to $3,000 ($1,500 MFS) by IRC §1211(b) — and the unused loss is
  carried over per the Capital Loss Carryover Worksheet (Instructions
  for Schedule D), exposed as `schd_carryover_short` /
  `schd_carryover_long`. The other Schedule D components (K-1 amounts,
  Forms 6252/4684, capital gain distributions from 1099-DIV Box 2a —
  Box 2a is not ingested — and prior-year carryovers *in*) do not exist
  in the v1 input set, so Schedule D lines 6/14 enter as zero.
- Line 16 is figured by the method the 1040 instructions prescribe,
  and the result names it (`line16_method` in the computed return, in
  `return get`, and in `tax explain --line 16`):
  - `qdcg` — the Qualified Dividends and Capital Gain Tax Worksheet
    (Instructions for Form 1040), used when qualified dividends
    (line 3a) were reported or Schedule D lines 15 and 16 are both
    gains. Its capital-gain input is the smaller of Schedule D lines
    15/16, floored at 0; the 0%/15% thresholds are Rev. Proc. 2024-40
    §3.03 (params/2025.json).
  - `tax_table` — the TY2025 Tax Table, used below $100,000 of taxable
    income. The table ships as data (params/2025-tax-table.json),
    extracted from the official Publication 1040 "Tax and Earned
    Income Credit Tables" PDF — never synthesized from the brackets;
    extraction was validated cell-by-cell (all 8,248 cells equal the
    statutory tax at each range's midpoint, the table's construction).
  - `tax_computation` — the Tax Computation Worksheet (ordinary
    brackets) at $100,000 or more.
  Still NOT implemented: the 28%-rate worksheet and the unrecaptured
  §1250 worksheet (and therefore the full Schedule D Tax Worksheet —
  its triggers cannot arise from v1 inputs), NIIT (Form 8960), the
  Form 4952 / Form 2555 tax worksheets, and any credit-related tax
  worksheets. Worksheets are figured in cents end-to-end; the paper
  worksheets use whole dollars, and Tax Table rows are selected by the
  floored dollar amount — a return whose lines carry cents can differ
  from a whole-dollar paper filing by a dollar.
- Line 12 is the standard deduction only; no itemizing, no additional
  amounts (age/blindness), no QBI deduction (line 13).
- Line 10 is the deductible part of self-employment tax only (Schedule 1
  line 15); no other adjustments (student loan interest, HSA, IRA…).
- No credits (CTC, EITC, education, foreign), no AMT, no NIIT. The only
  "other tax" is self-employment tax on line 23 (Schedule 2 line 4);
  line 24 = line 16 + line 23.
- Schedule C / Schedule SE (simplified, for 1099-NEC side income):
  - One `expenses` total per 1099-NEC engagement stands in for the whole
    Schedule C expense section — no per-category expenses, no cost of
    goods sold or inventory, no home-office deduction (Form 8829 /
    simplified method), no vehicle methods. `expenses` is not a
    1099-NEC box; it comes from the filer's records. A net loss on an
    engagement is allowed and flows to line 8 in full (no at-risk /
    passive-activity limits).
  - Schedule SE is the Short Schedule SE logic: net earnings = Schedule
    C net profit × 92.35% (IRC §1402(a); zero when the net is a loss);
    Social Security part = 12.4% (IRC §1401(a)) of net earnings capped
    at the TY2025 wage base ($176,100, SSA 2025 announcement) minus
    W-2 Social Security wages; Medicare part = 2.9% (IRC §1401(b)) of
    net earnings, uncapped. Rates and the wage base are params data
    (params/2025.json, with sources).
  - The W-2 Social Security wage input is approximated by Σ W-2 **Box 1**
    wages — Schedule SE line 8a takes Box 3, but Box 1 is what v1
    requires on a W-2 entry. The two differ when Box 1 is reduced by
    pre-tax amounts that remain Social Security wages (e.g. 401(k)
    deferrals), which can overstate the SE Social Security part.
    `tax return validate` raises an info finding (`nec_ss_wage_proxy`)
    on any return holding both W-2 and 1099-NEC entries.
  - No Additional Medicare Tax (0.9%, Form 8959), no exemptions from SE
    tax (ministers, etc.), no optional methods, no church-employee
    income, no estimated-tax underpayment penalty (Form 2210), and no
    §199A QBI deduction on the Schedule C profit. SE figures are kept in
    cents; the paper Schedule SE rounds to whole dollars, so a paper
    filing can differ by a dollar.
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
  13a/13b), and line 24 is written from engine line 24 (= 16 + 23;
  engine line 22 is always 0 since no lines 17–21 amounts exist).
  Lines 8, 10, and 23 are exported too (geometrically verified like the
  rest: line 8 = f1_72, line 10 = f1_74 — on page 1, between lines 9
  and 11a — and line 23 = f2_15). Schedules are not exported. An
  incomplete return is refused (`return_incomplete`, no file written).

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
