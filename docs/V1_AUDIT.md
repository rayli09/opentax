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
