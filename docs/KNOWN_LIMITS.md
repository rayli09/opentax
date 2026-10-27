# Known limits (scaffold, TY2025)

This list is the contract: anything not listed as working is not claimed.

## Compute (tax-forms)

- Only Form 1040, only these lines: 1a, 1z, 2b, 3b, 9, 11, 12, 15, 16, 25a,
  25d, 33, 34, 37.
- Income sources: W-2 wages, taxable interest, ordinary dividends. No
  Schedule C/D/E, no capital gains computation, no K-1, no crypto.
- Line 16 is **bracket tax** on taxable income for all amounts, named
  honestly: the IRS Tax Table (required below $100,000) and the Qualified
  Dividends and Capital Gain Tax Worksheet are NOT implemented in v1, so
  line 16 differs from a filed return wherever those would apply.
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

- Fill sets `/V` on named fields and regenerates **no** appearance streams:
  filled values may not render in every viewer until the backend spike
  (see ARCHITECTURE.md) lands a verified filler. PDF *export of a return*
  is therefore `not_implemented`; only field inspect/fill primitives exist.
- Checkbox/radio/choice field filling is not yet implemented (text fields
  are). Comb fields (SSN boxes) are untested.

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
