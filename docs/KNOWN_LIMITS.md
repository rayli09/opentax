# Known limits (scaffold, TY2025)

This list is the contract: anything not listed as working is not claimed.

## Compute (tax-forms)

- Only Form 1040, only these lines: 1a, 1z, 2a, 2b, 3a, 3b, 7, 8, 9, 10,
  11, 12, 15, 16, 19, 21, 22, 23, 24, 25a, 25d, 28, 32, 33, 34, 37 —
  plus internal keys in the computed lines map, which are not Form
  1040 line numbers:
  `schd_net_short` / `schd_net_long` (net short-/long-term gain or loss
  from 1099-B), `schd_net_capital_gain` (the QDCG worksheet's
  capital-gain input), `schd_carryover_short` / `schd_carryover_long`
  (capital loss carryovers to the next year, as positive amounts),
  `schc_net_profit` (Schedule C net profit from 1099-NEC engagements),
  `schedse_net_earnings` (Schedule SE line 4a net earnings),
  `sched2_se_tax` (the Schedule SE tax entering Schedule 2 line 4,
  equal to line 23), and `sched8812_tentative` /
  `sched8812_credit_after_phaseout` / `sched8812_earned_income`
  (Schedule 8812 line 8, line 12, and line 18a measures behind the
  child credits on lines 19/28).
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
- Line 12 is the standard deduction only; no itemizing, no QBI
  deduction (line 13). The age-65/blind **additional** amounts ARE
  implemented from the stored identity block (`tax return
  set-identity`): per condition (age 65 or older — born before
  January 2, 1961 for TY2025 — or blind) $2,000 for single / head of
  household and $1,600 for married filing jointly / separately
  (Rev. Proc. 2024-40; IR-2024-273; amounts and the cutoff date are
  params data). The taxpayer's conditions always count; a spouse's
  count only for MFJ/MFS. Identity is optional: with no identity (or
  no date of birth) stored, line 12 is the base amount and
  `return validate` notes the additions were not checked. NOT
  implemented on line 12: the **dependent-filer limitation** (when
  someone can claim the taxpayer as a dependent, the standard
  deduction is limited to the greater of $1,350 or earned income +
  $450, capped at the regular amount) — the identity flag
  `can_be_claimed_as_dependent` fills the form's line 12a checkbox
  and validation warns that the limitation itself is not computed.
  Also NOT implemented: the OBBBA temporary $6,000 senior bonus
  deduction (2025–2028, Schedule 1-A → line 13b) — a separate
  deduction from the age/blind additions above.
- Line 10 is the deductible part of self-employment tax only (Schedule 1
  line 15); no other adjustments (student loan interest, HSA, IRA…).
- No education or foreign credits, no AMT, no NIIT. The only
  "other tax" is self-employment tax on line 23 (Schedule 2 line 4);
  line 24 = line 22 + line 23. Family credits (CTC / ODC / ACTC) are
  implemented — see the Schedule 8812 section below.
- Dependents and child credits (simplified Schedule 8812):
  - Dependents are stored entries (`dependent` nodes: name,
    relationship — child / stepchild / foster_child / other — age on
    December 31, and months lived with the taxpayer). Classification
    follows the Schedule 8812 tests that v1 models: the child tax
    credit needs a child-type relationship, age under 17 at year-end,
    and more than half the year lived with the taxpayer (months > 6);
    any other stored dependent with months > 6 takes the $500 credit
    for other dependents. TY2025 amounts (Schedule 8812 / its
    instructions): CTC $2,200 per qualifying child, ODC $500, ACTC cap
    $1,700 per qualifying child; phaseout thresholds $400,000 (MFJ) /
    $200,000 (all other statuses), reduction = the excess over the
    threshold rounded up to the next $1,000 × 5%.
  - MAGI is taken to be AGI (Schedule 8812 line 3 modifications — e.g.
    Forms 2555/4563 exclusions — do not exist in v1).
  - The nonrefundable credit is limited by the Schedule 8812 **Credit
    Limit Worksheet A**: Form 1040 line 18 (line 16 + line 17) minus
    Schedule 3 nonrefundable credits — in v1, line 16 only (line 17
    AMT is 0 and no Schedule 3 credits exist). The credit does **not**
    offset self-employment tax (line 23); the form subtracts credits
    from line 18 before line 23 is added on line 24.
  - The additional child tax credit (line 28) is Schedule 8812 Part
    II-A only: the smallest of the credit unused by line 19, the
    per-child cap × qualifying children, or 15% of (earned income −
    $2,500). The Part II-B alternative formula for three or more
    qualifying children (Social Security taxes vs. earned income
    credit) is NOT implemented.
  - Earned income (Schedule 8812 line 18a) is a v1 definition: total
    W-2 Box 1 wages, plus Schedule C net profit when positive. It does
    not subtract Schedule SE tax, and combat pay / clergy / other
    adjustments are not modeled.
  - The ODC path does **not** model the qualifying-relative tests:
    the 50% support test, the gross-income test, and the relationship /
    member-of-household rules are not checked — any stored dependent
    not CTC-eligible with months > 6 is credited. A dependent's SSN
    may be stored (optional `ssn` on the dependent entry, normalized
    to 9 digits) and then prints in the Dependents table. The legal
    SSN requirement for the child tax credit is modeled only as a
    validation **warning** (`dependent_ctc_ssn`: "CTC requires the
    child to have an SSN (v1 still computes; a real filing would
    deny)") — v1 never denies the credit for a missing SSN, and the
    SSN-issued-by-the-due-date timing rule and taxpayer ITIN rules
    are not modeled at all.
  - The earned income credit (EIC / EITC, Form 1040 line 27) is a
    different credit — implemented in its own section below. The
    child and dependent care credit (Schedule 3 / Form 2441) is not
    implemented.
  - With no dependents stored, lines 19/21/28/32 are stated Complete
    zeros and 22/24/33 reduce to the pre-8812 arithmetic.
- Earned income credit (Form 1040 line 27; EIC Worksheets A and B of
  the Instructions for Form 1040 (2025); TY2025 parameters from Rev.
  Proc. 2024-40, §3.01; credit amounts read from the official EIC
  Table extracted into `params/2025-eic-table.json` — 1,374 rows
  tiling $1–$68,700 in $50 steps, 8 columns: single/head-of-household
  and MFJ × 0/1/2/3+ qualifying children; the table is DATA, never
  synthesized from the §3.01 formulas):
  - Qualifying children are counted from the stored dependents under
    the tests v1 models: relationship child / stepchild / foster
    child; lived with the taxpayer more than half the year (months
    > 6); an **SSN stored** (a child without one does not count at
    all in v1 — `tax return validate` warns via `dependent_eic_ssn`);
    and the age test — under 19 at year-end, or under 24 and a
    full-time student (dependent flag `full_time_student`), or
    permanently and totally disabled at any age (flag
    `permanently_disabled`). NOT modeled: siblings / step- and
    half-siblings / descendants of any of them (the printed test
    covers them, but a `relationship: "other"` entry cannot be
    distinguished from an unrelated person, so it never counts); the
    "younger than the taxpayer (and spouse)" component of the age
    test (the dependent's date of birth is not stored); the child's
    joint-return test; the tie-breaker rules when two filers could
    claim the same child; the SSN-valid-for-employment and
    issued-by-the-due-date rules (only presence is checked); and the
    taxpayer's / spouse's own SSN validity.
  - Earned income follows the worksheets exactly: Form 1040 line 1z
    wages (Worksheet A, Step 5; Worksheet B, Part 4) plus, for the
    self-employed, Worksheet B Part 1 — Schedule SE **line 3** net
    earnings (= the Schedule C net profit, NOT reduced by the 92.35%
    factor of Schedule SE line 2, which applies only to figuring the
    SE tax on line 4a) **minus Schedule SE line 13**, the deductible
    part of SE tax (Form 1040 line 10). Pub 596 (2025), chapter 3:
    net earnings from self-employment are "reduced by the deduction
    for one-half of your SE tax". Not modeled: the optional-methods
    add-back (Worksheet B Part 3), the Worksheet B Part 2 path for net
    earnings under $400 with no Schedule SE filed (the engine always
    figures SE tax on a positive net), combat-pay election, clergy /
    church-employee / statutory-employee special rules, and Form 2555
    foreign earned income (any foreign exclusion zeroes the credit by
    law; v1 has no such inputs).
  - Investment income (IRC §32(i)) is scoped to the lines this engine
    computes: line 2a (tax-exempt interest counts) + line 2b + line
    3b + the positive part of line 7 (capital gain net income — line 7
    equals Schedule D line 16 whenever it is positive). Rental,
    royalty, and passive-activity amounts do not exist among v1
    inputs, so they cannot be counted. Over the TY2025 limit of
    $11,950 the credit is zero (`eic_investment_income_limit` validate
    finding, info severity).
  - The credit lookup is the worksheets' two-amount flow: look up
    earned income in the EIC Table; when AGI differs and is at or
    above the Rev. Proc. §3.01 phaseout threshold for the child count
    ($23,350 with children / $10,620 without, non-MFJ; $30,470 /
    $17,730 MFJ), also look up AGI and take the smaller. AGI or
    earned income at or above the completed phaseout amount ($19,104 /
    $50,434 / $57,310 / $61,555 non-MFJ; $26,214 / $57,554 / $64,430 /
    $68,675 MFJ, by 0/1/2/3+ children) yields zero before any lookup.
    The engine figures in cents; the paper worksheets figure in whole
    dollars (the table lookup floors to whole dollars), so a paper
    filing can differ by a dollar.
  - **Married filing separately takes no credit in v1.** The §32(d)
    separated-spouse exception (lived apart for the last 6 months with
    a qualifying child) is NOT modeled; `tax return validate` warns
    (`eic_mfs`) when an MFS return stores dependents.
  - **No-children credit and the DOB judgment call:** with no
    qualifying children, the age-25-to-64 test (born after 1960-12-31
    and before 2001-01-02; either spouse suffices on a joint return)
    is applied to the stored identity dates of birth. When no usable
    DOB is stored, the test is **undeterminable and v1 takes no
    credit**: line 27 is a stated Complete 0 (never Incomplete solely
    for this) and `tax return validate` raises the info finding
    `eic_childless_age_unknown`. This is a deliberate modeling choice
    — the alternative (an Incomplete line 27 on every dependent-free
    return without identity) would poison lines 32/33 for most
    returns; the finding tells the agent exactly what fact is missing.
    The same stated-zero rule applies when AGI itself is Incomplete
    but the credit is zero under every AGI (MFS, no positive earned
    income, or the childless age test unmet/undeterminable).
  - Not modeled at all: EIC recapture and the 2-/10-year bans (Form
    8862), due-diligence recordkeeping (Form 8867), the advance EIC,
    state EICs, and the rule that a taxpayer who is another filer's
    qualifying child cannot claim the credit.
  - Line 32 is now line 27 + line 28; line 33 follows. With no EIC
    (credit zero), lines 32/33/34 are exactly the pre-EIC amounts.
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
- Line 33 = line 25d + line 32 (withholding plus the refundable
  credits above); no other payment lines (26, 31, …) exist in v1.
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
  amount lines, the filing-status checkbox, and the page-1 Dependents
  table for stored dependents (name split first word / remainder into
  the (1)/(2) fields, the (3) SSN field when an SSN is stored for
  that dependent, and the question-(7) credit checkbox — the
  sibling `Btn` fields under the "Child tax credit" / "Credit for
  other dependents" sub-columns — for the first four dependents; more
  than four produces a warning). When a taxpayer identity block is
  stored (`tax return set-identity`), the export also fills the
  header identity fields (taxpayer/spouse names and SSNs, home
  address), the page-2 occupations, and the standard-deduction
  checkboxes (line 12a "You as a dependent" from
  `can_be_claimed_as_dependent`; the born-before-January-2-1961 and
  blind boxes for taxpayer and spouse from their DOB / blind flags —
  all geometrically verified like the rest of the map). With no
  identity stored, the export reports the historical aggregate skips
  (taxpayer_name / taxpayer_ssn / home_address); with a partial
  identity, only the pieces actually absent are reported under
  `skipped`. SSN values in the export JSON report are masked
  (`***-**-6789`): the full SSN appears only inside the filled PDF. Amounts are
  written as exact dollars-and-cents (e.g. `4231.50`); whole-dollar
  rounding is NOT applied in v1. TY2025 layout notes: AGI is written
  to both 11a and 11b (the form repeats it on page 2), the standard
  deduction to 12e and to line 14 (= 12e + 13a + 13b, and the engine
  computes no 13a/13b), and line 24 is written from engine line 24
  (= line 22 + line 23; line 22 = line 16 − line 21, the child
  credits).
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
  rest yet. Treat the directory as sensitive (it is; it contains tax
  data, and — once identity is set — taxpayer/spouse/dependent SSNs).
  Identity is stored locally only: `return get`, `return list`, and
  `return set-identity` show SSNs masked (`***-**-6789`), `return list`
  is PII-free (id / year / status / completeness / identity_set), and
  the full SSN is written only into a filled PDF produced by
  `return export --type pdf` (the JSON export report masks it too).
