# Known limits (scaffold, TY2025)

This list is the contract: anything not listed as working is not claimed.

## Compute (tax-forms)

- Only Form 1040, only these lines: 1a, 1z, 2a, 2b, 3a, 3b, 7, 8, 9, 10,
  11, 12, 13b, 14, 15, 16, 19, 21, 22, 23, 24, 25a, 25d, 27, 28, 32, 33,
  34, 37 —
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
  child credits on lines 19/28), and `senior_bonus` (the Schedule 1-A
  line 37 enhanced deduction for seniors behind line 13b).
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
- Line 13b is the Schedule 1-A total as computed in v1: ONLY Part V
  (enhanced deduction for seniors — the OBBBA temporary deduction,
  TY2025–2028) is implemented, so line 13b equals Schedule 1-A
  line 37. Per person: $6,000 for a person with a valid SSN who was
  born before January 2, 1961 (v1 proxy for a valid SSN: a normalized
  SSN is stored — validity and issuance timing are not modeled, the
  same standing proxy as the EIC), reduced by 6% of modified AGI
  over $75,000 ($150,000 if married filing jointly); the phaseout is
  per person (Schedule 1-A lines 31–37: lines 36a/36b each take the
  full line 35 amount). The spouse counts only on a joint return;
  married filing separately takes NO senior deduction (Part V
  caution: married taxpayers must file jointly). Schedule 1-A
  Parts II (tips), III (overtime pay), and IV (car loan interest)
  are NOT implemented — their inputs do not exist in v1, so a filer
  with those deductions is out of scope. Modified AGI is taken to
  be AGI (the Part I add-backs for excluded foreign income,
  §§911/931/933, do not exist among v1 inputs). Line 13a (the QBI
  deduction, Form 8995/8995-A) is not computed and its field is not
  in the export map. Line 14 = line 12 + line 13b (printed form:
  12e + 13a + 13b), and line 15 = line 11 − line 14. The senior
  deduction is separate from, and in addition to, the age-65
  standard-deduction additions on line 12, and is allowed whether
  or not the taxpayer itemizes. With no eligible person stored,
  line 13b is a stated zero (`return validate` notes when no date
  of birth is on file, info finding `senior_bonus_age_unknown`;
  warning `senior_bonus_mfs` when an MFS filer is 65+).
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
- Filing statuses: single, mfj, hoh, mfs parameters are included; mfs
  special rules are not modeled.

## California — Form 540 (tax-forms `ca540`, TY2025)

Computed only when the return has a state block stored
(`tax return set-state`, `{"state": "CA", "residency": "full_year"}`).
Lines are keyed by the printed Form 540 line numbers in a separate
map (7, 8, 9, 10, 11, 13, 14, 15, 16, 17, 18, 19, 31, 32, 33, 34, 35,
48, 62, 64, 71, 75, 76, 78, 97, 100), plus two internal keys:
`caleitc_earned_income` (FTB 3514 line 19) and
`caleitc_qualifying_children` (a count, carried as count × $1.00 like
the federal `eic_qualifying_children`). All figures come from the
official FTB 2025 Form 540 booklet and FTB 3514 booklet;
`crates/tax-core/params/2025-ca.json` carries parameters with
sources, and the CA Tax Table and CA EITC Table are extracted data
files (`2025-ca-tax-table.json`, `2025-ca-eitc-table.json`) validated
cell-for-cell against the booklet's Tax Rate Schedules at range
midpoints (Tax Table) and by tiling/unimodality/maxima/spot reads
(EITC Table — FTB publishes no formula for it).

- **Full-year residents only.** Form 540NR (nonresidents and
  part-year residents) and any other residency value are explicit
  errors, not fallbacks. No other state is implemented.
- **Schedule CA adjustments are scoped to the facts the federal
  engine produces**, and for the v1 persona those facts are all
  California-conforming, so lines 14/16 are stated zeros: wages,
  taxable interest, dividends, capital gains (California taxes them
  at ordinary rates; the amount carries over unchanged), and
  Schedule C net all carry from the federal return as-is.
  In particular the federal deductible part of self-employment tax
  (1040 line 10) is **not** added back: the 2025 Instructions for
  Schedule CA (540), Part I, Section C, state California conforms to
  federal law for the Section C adjustments except as listed, and the
  only listed line 15 difference is a classification case
  (independent contractor federally, employee for California → the
  column A amount goes in column B, a subtraction). A filer whose
  facts include that classification difference is out of scope (the
  engine flags every return with 1099-NEC entries with an info
  finding saying it is not modeled). Adjustments the engine cannot
  see — HSA contributions/deductions (a genuine nonconformity),
  state income tax refunds, Social Security benefit differences,
  US-obligation interest, California lottery, and everything else on
  Schedule CA Parts I/III — are out of scope: a filer with those
  facts is outside CA v1.
- **Line 12 (California state wages) is not computed** — it is
  informational on the printed form; W-2 Box 16 is stored per entry
  and used for the CalEITC earned income. It is left blank on the
  filled Form 540 for the same reason.
- **Standard deduction only** ($5,706 single/MFS, $11,412 MFJ/HoH).
  California itemized deductions (Schedule CA Part II, Form 540
  line 18 alternative) are not computed even when the federal return
  itemizes (the federal engine takes the standard deduction too).
  Age 65 and blindness earn **exemption credits** ($153 each, lines
  8/9), not a deduction addition — the two mechanisms are distinct.
  The senior exemption uses the same born-before-January-2-1961
  cutoff as the federal standard-deduction addition.
- **Exemption credits**: $153 per personal/blind/senior exemption,
  $475 per dependent. Every stored dependent entry counts toward the
  dependent credit (the form's dependent list); the federal
  eligible-child rules do not gate it. Credits are reduced by the
  AGI Limitation Worksheet when federal AGI exceeds $252,203
  (single/MFS), $504,411 (MFJ), or $378,310 (HoH): whole-dollar
  steps of $2,500 ($1,250 MFS) × $6 per exemption, applied to the
  personal/blind/senior group and the dependent group separately,
  each floored at zero. The worksheet's rounding to whole dollars is
  preserved.
- **Tax**: the extracted CA Tax Table at taxable income ≤ $100,000;
  the Tax Rate Schedules (params) above. Line 34 (lump-sum tax) is a
  stated zero. The Behavioral Health Services Tax (line 62, 1% of
  taxable income over $1,000,000) **is** computed; the engine keeps
  cents where the printed worksheet rounds to whole dollars.
- **No other nonrefundable credits**: lines 40–47 — including the
  Nonrefundable Renter's Credit (line 46), the child and dependent
  care credit, and the college access credit — are not computed, so
  line 48 equals line 35. No AMT (line 61) or other taxes (line 63).
- **Payments**: line 71 is W-2 Box 17 plus 1099-NEC Box 5 state
  withholding. Estimated payments (72), other-state withholding forms
  592-B/593 (73), the film credit (74), and the Foster Youth Tax
  Credit (77 — also $1,189; foster-youth eligibility is not modeled)
  are not computed. Use tax (91) and Individual Shared Responsibility
  (95) are treated as zero, so lines 97/100 are the exact difference
  when those do not apply.
- **CalEITC (line 75)** inlines form FTB 3514: federal AGI must be
  under $32,901 and investment income (the federal EIC investment
  aggregate) at most $4,814. Earned income is W-2 **Box 16**
  California wages — not Box 1 — plus business income per Worksheet 3
  (Schedule C net minus the deductible part of SE tax); other federal
  earned-income components (statutory-employee wages, clergy,
  disability) are not modeled. Qualifying children are the federal
  EIC child tests with one California difference honored and one
  assumed: California accepts an ITIN for the child (the stored
  9-digit identifier is accepted without distinguishing SSN from
  ITIN, and ITIN filers can claim the credit — the Step 1b/2b
  SSN-or-ITIN gate is checked for presence only, with a
  `caleitc_ssn_unverified` validate warning when the credit is
  allowed but no identifier is stored), and the child is taken to
  have lived in California more than half the year from the
  full-year-resident selection (a child who lived elsewhere is out
  of scope). Married filing separately is barred; the separated-
  spouse exception California conforms to federally is not modeled
  (0 + a validate warning). With no qualifying children the filer
  must be at least 18 (born on or before January 1, 2008); with
  qualifying children there is no filer age test (FTB 3514 Step 3),
  so none is applied. When any W-2's Box 16 was not transcribed the
  credit is Incomplete rather than zero (unless a wage-independent
  gate already decided it), mirroring the withholding doctrine.
- **Young Child Tax Credit (line 76)** requires an allowed CalEITC
  and a CalEITC qualifying child younger than six as of the last day
  of the taxable year; it is $1,189 **per return, not per child**,
  reduced by $21.71 per $100 (two decimal places, unrounded) of
  earned income over $27,425, with the form's $0–$1 → $1 rule.
  The zero-or-negative-earned-income expansion (TY2022+, FTB 3514
  lines 23a/23b: wage and net-loss caps of $35,640 and a federal AGI
  cap of $32,900) is **not computed**: when it may apply, line 76 is
  Incomplete with a validate warning, never a guessed amount.
- **Completeness doctrine**: a stored CA block conjoins with the
  federal completeness — `return get` reports the return incomplete
  if either side is. Any W-2 without Box 17 or 1099-NEC without Box 5
  makes line 71 (and with it 78 and the refund/due) Incomplete with a
  per-entry `ca_state_withholding_missing` validate finding; filers
  record `"0"` explicitly to state that no state tax was withheld.
- **PDF filling (packet export)**: when a state block is stored, the
  packet export fills the official FTB Form 540 after the federal
  forms, from the CA engine's stated lines and the stored identity,
  through the geometrically verified map
  `crates/tax-forms/forms/f540-2025-field-map.json` (widget Rects vs
  `pdftotext -bbox` printed labels, cross-checked against every
  field's /TU tooltip; template:
  https://www.ftb.ca.gov/forms/2025/2025-540.pdf, kept as the
  git-ignored fixture `f540.pdf`). A CA-incomplete return fails the
  packet export naming the incomplete California lines. Fields the
  engine does not state stay blank and the manifest notes say why:
  line 12, county at time of filing (not stored), lines 40–47, 61,
  63, 72–74, 77, 91–96, 98, 99, 110–115, the direct-deposit fields
  (bank details are not stored), and the signature block. Line 62 is
  printed only when greater than zero; lines 97/100 print only the
  side of the pair that is greater than zero. The exemption **counts**
  (lines 7–10 boxes) come from the same `exemption_counts`
  derivation as the engine's amounts. The Dependents section prints
  the first three stored dependents (the form has three columns;
  credits still compute for all). Form 540 has no occupation field,
  so occupations never print on it. Two FTB form quirks the stack
  handles: its AcroForm `/Fields` is an *indirect* array (tax-pdf
  enumeration resolves that shape since this lane; IRS forms use a
  direct array), and the running name/SSN headers on Sides 2–6 are
  single fields with one widget per side, not per-side fields.
  **Schedule CA is never filled in v1** (`f540ca` template:
  https://www.ftb.ca.gov/forms/2025/2025-540-ca.pdf, fixture
  `f540ca.pdf`): the packet reports it `not_applicable` with the
  no-adjustments note above, because Schedule CA is required only
  when there are adjustments and the engine states none. A
  federal-only return's packet gains both CA entries as
  `not_applicable`. California's e-file XML remains out of scope
  entirely.

## Nonresident aliens — Form 1040-NR (tax-forms `f1040nr`, TY2025)

The student route: an optional `nra` block on the return (set with
`tax return set-nra`) reroutes the federal computation to Form
1040-NR. Returns without the block are computed exactly as before.

- **Who is in scope.** F-1 / J-1 **students** in their
  exempt-individual years: at most 5 calendar years in that status,
  counting the current year (Pub 519 (2025), chapter 1: any part of
  a calendar year counts as a whole year, and an individual is not
  an exempt individual as a student after being exempt for any part
  of more than 5 calendar years). Filing status **single only**.
  **Simplifications, stated plainly:** the real exempt-individual
  rules are richer than v1's count — Pub 519 keeps a
  facts-and-circumstances exception beyond 5 years (no intent to
  reside permanently plus substantial visa compliance), J-visa
  teachers/trainees use a different 2-of-6-years rule with look-back
  wrinkles for prior exempt years, and the substantial presence
  test is the actual residency determination. v1 models **none** of
  that: `set-nra` **refuses** more than 5 years (the error names the
  exception and dual-status as undetermined rather than declaring
  the filer a resident), refuses other visas and other statuses,
  and an info finding states the rule used. **Dual-status returns
  are out of scope.**
- **Treaties** (params in `params/2025-treaties.json`, figures from
  the treaty texts on irs.gov). **China, Article 20:** (c) up to
  **$5,000** per year of U.S. personal-services income is exempt;
  (b) grants or awards from a government, scientific, educational,
  or other tax-exempt organization are exempt — a university
  scholarship (Form 1042-S income code 15) paid by the educational
  institution is modeled exempt in full under 20(b), with **no**
  dollar cap; (a) payments from abroad are outside U.S. taxing
  jurisdiction and never enter the return. **India, Article 21:**
  (2) a student is entitled to the same reliefs as residents, so
  the Form 1040 **standard deduction** applies (TY2025 single
  $15,750, from `params/2025.json`); Article 21(1) exempts only
  payments arising *outside* the U.S., so there is **no**
  U.S.-source wage or scholarship exemption. **Any other
  citizenship** computes with **no treaty benefits at all** plus a
  `treaty_not_modeled` warning — benefits are never invented.
  Treaty-exempt amounts are reported on line 1k with Schedule OI
  data (country, article, amount) in the output. The wage
  exemption is applied **exactly once** across the two reporting
  patterns (Instructions for Form 1040-NR (2025), "Amounts Exempt
  Under a Treaty"): when the payer split the pay with Form 8233,
  the exempt part arrives on a treaty-marked Form 1042-S (income
  code 20, exemption code "04") and the W-2 already excludes it —
  the on-return claim is the treaty cap minus the already-excluded
  part, floored at zero and capped by the W-2 wages.
- **Income scope.** Effectively connected **wages** (W-2 box 1 +
  Form 1042-S code 20) and **scholarship/fellowship** (code 15:
  treaty-exempt per above, otherwise taxable via Schedule 1 line
  8r). Form 1042-S income codes **other than 15/20** are recorded
  but make the NR section **incomplete** with a loud finding —
  never silently taxed or dropped. **Bank deposit interest is
  excluded** from a nonresident alien's income (IRC §871(i));
  1099-INT entries on an NR return produce an info finding and are
  not taxed. **Dividends, capital gains, and nonemployee
  compensation belong on Schedule NEC** (taxed at flat rates, not
  connected with a U.S. trade or business): **not modeled** — their
  presence raises a loud warning and those amounts are excluded
  from the NR computation. Schedule 1 adjustments (line 10) are
  zero; **Schedule A itemized deductions are not computed** (line
  12 is 0, or the India standard deduction).
- **Credits: none.** No EIC, no child tax credit / ACTC, no senior
  bonus on this route (Form 1040-NR line 32 is 0 in v1). Dependents
  stored on an NR return raise `nra_credits_not_modeled` and change
  nothing — note the real rules are narrower still (only U.S.
  nationals and residents of Canada/Mexico, and Indian students on
  limited terms, may claim dependents at all; residents of China
  cannot).
- **FICA check.** An exempt individual's services performed to
  carry out the purpose of their admission are not covered
  employment (Pub 519 (2025), chapter 8), so no Social Security or
  Medicare tax should have been withheld. If any stored W-2 shows
  Social Security or Medicare tax withheld, validate warns
  (`fica_withheld_while_exempt`): the remedy is an employer refund
  first, else Form 843 with Form 8316 — the CLI does not file
  those. Correspondingly, the resident W-2 statutory-rate
  cross-checks are suppressed on this route (zero FICA tax is the
  expected state here).
- **Section 117 is not re-figured.** The qualified-scholarship
  exclusion (tuition and required fees for a degree candidate)
  applies to nonresident aliens too, but payers apply it before
  reporting on Form 1042-S; the engine taxes code-15 amounts as
  reported (minus treaty exemption) and does not second-guess the
  payer's split between qualified and nonqualified amounts.
- **Treaty claims that require Form 8833** (a treaty-based return
  position disclosed on that form — e.g. claiming a code-15
  scholarship exemption directly on the return when the payer did
  not mark the Form 1042-S as treaty-exempt) **are not modeled**:
  in v1 an unmarked code-15 amount is taxable. The marked-1042-S
  pattern (the common university case) is fully handled.
- **Form 8843 facts are stored, and the forms are filled.** The
  `form_8843` object (school and program-director name/address/
  phone, days present for the current and two prior years) is
  stored verbatim and validated for presence. `return export
  --type packet` on an NR return fills the official TY2025 **Form
  1040-NR**, **Schedule OI (Form 1040-NR)**, and **Form 8843**
  from geometrically verified field maps
  (`crates/tax-forms/forms/f1040nr-2025-field-map.json`,
  `f1040soi-2025-field-map.json`, `f8843-2025-field-map.json`;
  acceptance: `scripts/nra_packet_acceptance.py`), and `--type
  pdf` fills the 1040-NR alone. Fields whose answers are not
  stored facts stay blank under manifest notes — the 1040-NR
  foreign address block and Digital Assets answer, Schedule OI
  items B–M beyond citizenship/visa/days/treaty rows, the 8843
  passport lines and exact date of entry, and the 8843 address
  fields (blank by the form's own instruction when the 8843 is
  attached to a return). The identity model stores one U.S.
  mailing address and no passport number, so those are
  hand-completed. **`export --type mef` stays an explicit
  `not_implemented` error** for NR returns: the MeF exporter emits
  the resident Form 1040 package, and the Form 1040-NR MeF schema
  package is separate and not implemented. An exempt individual
  must file a fully completed Form 8843 for every exempt year,
  including years with no income and no 1040-NR — in a no-income
  year the 8843 is mailed by itself, and then its address fields
  (blank in the packet) must be completed by hand.
- **State taxation of nonresident aliens is NOT handled.**
  California does **not** honor federal tax treaties (the
  treaty-exempt $5,000 is California-taxable) and nonresidents file
  Form **540NR**, which is not implemented. A CA state block and an
  nra block on the same return are an **explicit error**
  (`nra_state_conflict`) on `set-nra`, `set-state`, compute,
  validate, and export — never a silent resident computation.

## Documents (tax-doc)

- Classification is title matching on extracted text only. Known
  weakness (pre-existing, unchanged by the extraction lane): the
  substring rules match cross-references inside other forms — a real
  blank Form 1040 classifies as `1099-r` and Schedule D / Form 8949
  as `1099-b`, because their text mentions those forms. `doc extract`
  inherits the label but still fails safe: those kinds are not
  extractable, so it reports `unrecognized` and extracts nothing.
- W-2 validation implements the Social Security and Medicare rate checks
  only, and only when Boxes 3–6 were provided.
- No OCR ships in v1 (`NoOcr` reports unavailable). Scanned documents need
  the agent-vision path described in the ingest skill.
- Text extraction for `doc classify` is lopdf's crude built-in text
  dump: no positions, no tables, reading order not guaranteed.

### `tax doc extract` (born-digital W-2 / 1099-INT / 1099-DIV / 1099-NEC)

- Born-digital PDFs only. A document whose text layer yields fewer
  than 25 characters reports `needs_ocr`; nothing is ever guessed.
- Extraction is label-anchored and best-effort: issuer layouts vary,
  so a form whose labels/values are laid out unusually (values above
  labels, far-right columns beyond the search bands, rotated text)
  extracts fewer fields and reports `partial` with per-field evidence
  and warnings — review the evidence before entering the node.
- Layout rules, in order: AcroForm field values (fillable copies)
  first, then the text layer — label phrase found (box-numbered hits
  preferred over look-alike phrases in titles), value token searched
  same-line-right, then ≤30 pt below, then ≤45 pt below, then
  same-line far right. The below-band is capped at 45 pt so an empty
  box never steals the next box's value.
- A located-but-empty box emits `"0"` ONLY where the payload field
  defaults to zero (1099 withholding boxes, tax-exempt interest,
  qualified dividends). Required boxes (W-2 Boxes 1–2, INT Box 1, DIV
  Box 1a, NEC Box 1) and W-2's optional `Option` boxes are omitted
  when empty — absence, not zero.
- Payer/employer name = the first text line of the name block below
  the printed label, column-clustered; multi-line names and names
  sharing a row with unexpected text can truncate or misread —
  evidence shows the raw line.
- Privacy: the taxpayer's SSN is never extracted. No rule reads the
  employee/recipient SSN or TIN boxes, and a PAYER'S TIN found in SSN
  shape (`NNN-NN-NNNN`) is refused, never emitted — only EIN-shaped
  (`NN-NNNNNNN`) payer/employer IDs are extracted (W-2 `employer_ein`;
  1099-NEC `payer_ein`). The W-2 payload has no employer-name field;
  the extracted `employer_name` rides along in the node for the
  agent's eyeball check and is ignored by `form add`.
- 1099-B statements are deliberately NOT extracted: broker layouts
  vary too much for label-anchored rules (status `unrecognized`,
  agent-vision path remains). 1099-R is likewise not implemented.
- Geometry limits: word positions come from the content-stream text
  state (Tm/Td/TJ, `cm`); page `/Rotate`, vertical/CJK text, and
  Type0 fonts without a `/ToUnicode` map are not handled (undecodable
  text simply fails to match labels). Glyph widths resolve from
  `/Widths` / CID `/W`, then base-14 AFM fallback tables, then a
  1000-unit default — widths affect word boxes only, never values.
- Multiple copies of a form on one page (e.g. W-2 Copy B + C): the
  first copy in reading order is extracted; a warning records the
  copies, and differing values across copies are flagged loudly.
- The 1099-NEC `expenses` field is not printed on the form and is
  never extracted; the filer supplies it from their records.

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
  deduction to 12e, line 13b (the Schedule 1-A senior deduction; the
  engine computes no 13a QBI amount and that field is not in the
  map), line 14 from engine line 14 (= 12e + 13a + 13b as the engine
  computes it), and line 24 is written from engine line 24
  (= line 22 + line 23; line 22 = line 16 − line 21, the child
  credits).
  Lines 8, 10, and 23 are exported too (geometrically verified like the
  rest: line 8 = f1_72, line 10 = f1_74 — on page 1, between lines 9
  and 11a — and line 23 = f2_15). An
  incomplete return is refused (`return_incomplete`, no file written).
- *Filing packet export* is implemented:
  `tax return export --return <id> --type packet --forms-dir <dir>
  --out-dir <dir>` writes the filled Form 1040 (the `--type pdf` path
  above, with the template taken from the forms dir) plus every
  applicable schedule, each filled through the same audited backend
  from its own geometrically verified map
  (`crates/tax-forms/forms/*-2025-field-map.json`, same derivation
  method as the 1040 map), and a `manifest.json` reporting every form
  as `filled`, `not_applicable`, or `missing_template` — a form whose
  inputs exist but whose template is absent from `--forms-dir` is a
  loud manifest entry, never a silent skip. Template convention: the
  blank TY2025 PDFs live in `--forms-dir` under the packet's canonical
  names (`f1040.pdf`, `f1040sd.pdf`, `f8949.pdf`, `f1040s1.pdf`,
  `f1040s2.pdf`, `f1040sc.pdf`, `f1040sse.pdf`, `f10408812.pdf` — the
  IRS publishes Schedule 8812 as `f1040s8.pdf` — and `f1040s1a.pdf`);
  one `f1040sc.pdf` template serves every Schedule C. An incomplete
  return is refused, like the PDF export. Acceptance:
  `scripts/packet_acceptance.py` builds the golden returns 002 / 005 /
  009 / 010 through the CLI, exports packets, and reads the filled
  fields back with pypdf. Packet scope and simplifications:
  - Schedule D + Form 8949: every stored 1099-B transaction is listed
    on Form 8949. v1 treats all entries as basis-reported (the input
    carries no basis-reported flag): short-term → Form 8949 Part I
    Box A, long-term → Part II Box D, so the totals land on Schedule D
    lines **1b/8b** — the printed TY2025 routing for 8949-reported
    transactions — not the 1a/8a aggregate shortcut, which is only for
    transactions NOT listed on Form 8949. Amounts and lines 7/15/16
    are identical either way. Schedule D lines 6/14 (prior-year
    carryovers in) stay blank — carryovers are computed out, not
    consumed, in v1; line 13 (capital gain distributions) stays blank
    (not ingested); lines 18/19 (28% / §1250 worksheets) stay blank
    (out of scope) and line 20 is answered on that basis; the QOF
    question stays unanswered (not modeled).
  - Form 8949's TY2025 fillable has **11 transaction rows per part**
    (not 14). A (term × box) group with more rows fills the first 11;
    the overflow is reported in the manifest notes and the page totals
    cover the listed rows only (Schedule D still carries the full
    engine totals). Continuation pages are out of scope — an
    overflowing packet is NOT complete for filing. Columns (b)/(c)
    (dates) and (f) (adjustment code) fill only when the transaction
    stored them (`date_acquired` / `date_sold` / `adjustment_code` on
    the 1099-B entry); blanks are noted in the manifest.
  - Schedule C: one per 1099-NEC engagement; expenses are the single
    stored aggregate, listed as one Part V row, totaled on line 48,
    and carried to line **27b** (TY2025 line 27a is the Form 7205
    deduction). Header boxes A/B (principal business / code), E
    (address), F (accounting method), G/H/I/J stay blank (not
    modeled) and are flagged in the manifest notes; a loss-making
    Schedule C still needs one line 32 at-risk box checked by hand.
  - Schedule SE: line 8a takes the engine's W-2 Box 1 wage total as
    the proxy for Box 3 Social Security wages (the return stores no
    Box 3 total; same proxy as the `nec_ss_wage_proxy` validation
    finding). The engine does not apply the form's $400 net-earnings
    filing threshold; when it would matter, the manifest says so.
    Line 7's wage base is pre-printed on the TY2025 form (no field).
  - Schedule 8812: Part I and Part II-A are filled; **Part II-B is
    not implemented** (lines 21–26 blank) — when line 16b is $5,100
    or more and line 20 is less than line 17, the Part II-A amount on
    line 27 can understate the ACTC, and the manifest notes the
    routing.
  - Schedule 1-A: Parts I (MAGI), V (seniors), and VI (total) are
    filled when a senior deduction applies. Parts II–IV (tips /
    overtime / car loan interest) stay blank — those deductions are
    not modeled at all in v1, so no return can need them in the
    packet.
  - An internal `schedules` view (per-form applicability and the
    internal schedule values, integer cents) is included in the
    compute output (`tax return get`, JSON export, packet export);
    it reuses the packet derivations, so it cannot disagree with the
    filled forms.

## Filing

- No e-file and no transmission of any kind. MeF **export** exists
  (`tax return export --type mef --out <file.xml>`); it produces a
  standards-shaped artifact for import/review/testing — it cannot be
  filed. Transmission requires a firm-level EFIN/ETIN, annual IRS
  Assurance Testing (ATS) per form family, and per-state approvals;
  credentials are non-transferable and none exist in this tool, by
  design (v1). No IRS/state credential handling of any kind.
- MeF export scope and honesty tiers:
  - **Form documents (Tier A)**: `IRS1040` plus the schedules the
    engine computes, with the packet's applicability rules —
    Schedules 1, 2, C (one per 1099-NEC engagement), D, SE, 8812,
    1-A, Form 8949, and Schedule EIC when it can be represented.
    Element names, order, and types follow the official IRS TY2025
    MeF schema package, version **2025v5.2** (Final Release,
    2026-02-19; the IRS distributes the package only through its
    Secure Object Repository, so reference copies are vendored with
    provenance under `scripts/mef-schemas/`). The emitted documents
    are validated against those XSDs by `scripts/mef_acceptance.py`
    — the binary itself performs no XSD validation and its export
    report says so (`validation.xsd_validated: false`). One disclosed
    substitution: the schemas' shared `Common/efileTypes.xsd` is the
    authentic TY2015 file (the TY2025 Common file is SOR-gated too),
    with three post-2015 leaf types supplied by the harness; none of
    them gates an emitted element.
  - **Envelope (Tier B)**: `Return` / `ReturnHeader` / `ReturnData`
    per IRS Publication 4164 (Rev. 12-2025) §2.10.1 and the header
    vocabulary of a production transmitter — NOT XSD-validated (the
    envelope schemas are not publicly available). The header omits
    originator (EFIN), software ID, PIN/signature, and
    filing-security groups, which only a credentialed transmitter
    can populate.
  - Amounts are **whole dollars** (the MeF amount types are integer
    dollars), rounded per field from the engine's cents, half away
    from zero; the export report lists every rounded field and any
    total that stops cross-footing by a dollar. The JSON/PDF exports
    keep cents and are the precise artifacts.
  - A stored taxpayer identity is **required** (unlike PDF export,
    which tolerates its absence and reports identity fields
    skipped): the ReturnHeader Filer block needs the taxpayer's name
    and SSN. The full SSN appears in the XML file — it is the filing
    artifact — and never in the CLI's stdout report.
  - `IRSW2` documents are not emitted: the return stores each W-2's
    EIN and amounts, but not the employer's name, name control, or
    address, which the schema requires.
  - Schedule EIC is omitted (with a note in the export report) when
    an EIC qualifying child's relationship cannot be represented:
    that form's `ChildRelationshipCd` enumeration is gender-specific
    (SON / DAUGHTER / …) with no generic value, and the return
    records a child without gender. On Form 1040's `DependentDetail`
    the same child is emitted as OTHER (a disclosed lossy mapping;
    STEPCHILD and FOSTER CHILD map exactly in both places).
  - The schema requires two answers the return does not record:
    `VirtualCurAcquiredDurTYInd` (digital assets) is emitted `false`
    — no digital-asset transaction exists anywhere in the return's
    modeled scope — and Schedule C's accounting-method choice is
    emitted as cash, matching the engine's cash-shaped Schedule C
    model (the PDF packet leaves that box blank). Both are disclosed
    in the export report's validation notes.
  - The refund is emitted as a full refund to the filer
    (`OverpaidAmt` = `RefundAmt`): no Form 8888 split and nothing
    applied to next year's estimated tax, matching the engine.

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
