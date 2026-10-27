# v1 completeness audit — 2026-10-27

Coordinator audit of the v1 completeness definition (federal, end-to-end,
for the target persona: W-2 paycheck + a few stock/bank forms, optional
1099-NEC side income, optional family with CTC/EIC, standard deduction).
Every item below was verified on merged `main` by re-running the gates and
exercising the flow, not taken from lane reports alone.

## Checklist

| # | Item | Status | Evidence |
|---|------|--------|----------|
| 1 | Taxpayer identity stored + exported (names/SSN/address/spouse/occupation/DOB, dependent SSNs), outputs masked | ✅ | `return set-identity`; export fills 18 identity fields + dependent SSN row; acceptance readback; masking unit tests |
| 2 | Age-65/blind additional standard deduction | ✅ | Rev. Proc. 2024-40 amounts as params ($2,000 single/HoH, $1,600 MFJ/MFS per condition); case 008 |
| 3 | `return list` housekeeping | ✅ | PII-free listing, unit-tested |
| 4 | EIC (line 27) | ✅ | Official TY2025 EIC Table extracted as data (1,374 rows; 99.8% of cells == Rev. Proc. 2024-40 §3.01 formula, residuals asserted individually); Worksheets A/B; cases 007 (updated) + 009 |
| 5 | OBBBA senior bonus deduction (Schedule 1-A → 13b) | ✅ | Per-person phaseout per the real form; cases 008 (updated) + 010 |
| 6 | Full filing packet export | ✅ | `return export --type packet`: 1040 + Sch D/8949/1/2/C/SE/8812/1-A, geometrically verified maps (290 entries), manifest with honest statuses; `scripts/packet_acceptance.py` (~110 readback assertions) passes |
| 7 | Born-digital doc extraction | ✅ | `tax doc extract` for W-2/1099-INT/DIV/NEC: label-anchored + AcroForm passes, evidence per field, `needs_ocr`/`partial` honesty, no taxpayer SSN in output; 12 exact-cents fixture tests; extract output feeds `form add` verbatim |
| 8 | MeF XML export | ✅ with disclosed tier | Form documents validated against the official TY2025 (2025v5.2) XSDs by `scripts/mef_acceptance.py` (142 checks); envelope is Pub 4164-shaped and explicitly NOT XSD-validated; report's `xsd_validated` is `false`; transmission impossible without a firm's own EFIN/ATS (see KNOWN_LIMITS) |
| 9 | Direct interest/dividends recordable after creation | ✅ | `return set-other-income` (audit-found gap, fixed same day; negative amounts rejected at both create and set) |

## End-to-end exercise (audit run)

Fresh store → `doc extract` on the four synthetic fixture PDFs (W-2,
1099-INT, 1099-DIV, 1099-NEC) → `form add` each extracted node verbatim →
`set-identity` → `return get` complete, every total hand-checked against
the fixture values (income $71,825.31, AGI $70,712.60, SE tax $2,225.43,
owed $4,136.95) → packet export (1040/Sch 1/2/C/SE filled; Sch D/8949,
8812, 1-A correctly `not_applicable`) → MeF export (IRS1040 + Sch 1/2/C/SE
documents generated).

## Gates at audit time

- `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean
- `cargo test --workspace`: 139 tests, 0 failures
- `tax selftest`: 10/10 golden cases, each value derived from IRS sources in the case's `_sources`
- CI green on `main`

## Declared non-goal

**State taxes.** v1 is federal-only by explicit decision. Nothing in the
product implies state support; this is the one deliberate exclusion from
"complete."

## Remaining known limits (all in docs/KNOWN_LIMITS.md)

Itemizing/Schedule A, rental, K-1, AMT, QBI (13a), 28%/§1250 worksheets,
NIIT, prior-year capital-loss carryovers in, Schedule 1-A Parts II–IV
(tips/overtime/car-loan), Schedule C itemized expenses (single aggregate),
SE wage-base Box 1 proxy, 8949 continuation pages (>11 rows per group are
loudly flagged in the manifest), 1099-B statement extraction (agent-vision
path), OCR for scanned documents (stub), MeF transmission (credentialed
firms only).

---

## Addendum — California (Form 540), 2026-10-27

Per the user's direction the same day ("continue with state taxes, starting
with California"), the declared non-goal #15 is lifted for CA. Verified on
merged `main` (5087c3c) by the coordinator, not from lane reports alone.

- **Compute (CA-1, merge 158eac0):** full-year-resident Form 540 from federal
  AGI. TY2025 params verified from the official FTB 540 booklet: standard
  deduction $5,706 single/MFS, $11,412 MFJ/HoH; exemption credits $153
  personal / $475 dependent with the AGI Limitation Worksheet phaseout; CA
  Tax Table (1,001 rows) and CA EITC Table (658 rows) extracted as data — all
  3,003 tax-table cells equal the rate schedule at the documented midpoint
  rule. CalEITC (earned income from W-2 Box 16) + YCTC $1,189/return. State
  withholding/Box 16/Box 17 are recorded inputs — absent ⇒ the affected CA
  lines stay Incomplete (missing ≠ 0). Golden cases: CA-001 refund $460;
  CA-002 CalEITC $207 + YCTC $1,064, refund $1,471; CA-003 owed $611.
- **Correction recorded:** the coordinator's brief assumed CA adds back the
  federal ½-SE-tax deduction on Schedule CA. The 2025 Schedule CA (540)
  instructions show CA **conforms** when self-employment classification
  matches on both sides — no adjustment. CA-003 is derived accordingly (its
  `_sources` document the evaluation). The Sch CA lane was NOT built on the
  wrong premise.
- **Forms (CA-2, merge 5087c3c):** 66-entry geometrically verified f540 map
  (widget geometry cross-checked against each field's /TU line tooltip);
  packet gains f540 (filled) + f540ca (always `not_applicable` in v1 — Sch CA
  is required only when adjustments exist, and none are in scope; a zero form
  would misrepresent the filing). `scripts/ca_packet_acceptance.py` (52
  checks) passes on merged main: CA-002 readback 13/17/19/31/32/71/75/76/78/97
  all match the engine; full SSNs appear only inside the PDF. A tax-pdf bug
  found en route (indirect AcroForm /Fields arrays, FTB shape) is fixed with
  a regression test; federal acceptance suites pass unchanged.
- **Still out for CA (KNOWN_LIMITS):** 540NR/part-year, Schedule CA
  adjustments beyond engine facts, itemized deductions, renter's credit,
  credits lines 40–47, FYTC, YCTC zero-earned-income expansion (flagged
  Incomplete + warning), CA e-file XML.
