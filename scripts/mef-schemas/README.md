# MeF schema copies used by the MeF export acceptance harness

These are **reference copies for validation only** — the `tax` binary does not
read them at runtime. `scripts/mef_acceptance.py` validates the form documents
emitted by `tax return export --type mef` against them.

## `2025v5.2/` — TY2025 per-form schemas (IRS 1040 MeF package)

- Source: the IRS TY2025 1040 Modernized e-File (MeF) schema package,
  version **2025v5.2**, MaturityLevel "Final Release", ReleaseDate
  **February 19, 2026** (stated in each file's own annotation — this matches
  the IRS's published TY2025 schema version history, where v5.2 was posted
  2026-02-19).
- The IRS distributes this package only through its Secure Object
  Repository (SOR) mailbox to registered software developers; it is not
  posted for public download. These copies come from a public mirror of the
  package (the TaxMD `irs-form-view-poc` project, `public/xsl/2025/`),
  cross-checked against the IRS version table above.
- Files: IRS1040, IRS1040Schedule1, IRS1040Schedule1A, IRS1040Schedule2,
  IRS1040ScheduleC, IRS1040ScheduleD, IRS1040ScheduleEIC, IRS1040ScheduleSE,
  IRS1040Schedule8812, IRS8949, IRSW2.

## `common/efileTypes.xsd` — shared types (authentic, older vintage)

- The per-form schemas pull in shared types via
  `<xsd:include schemaLocation="../../../Common/efileTypes.xsd"/>`.
- This copy is the authentic IRS `efileTypes.xsd` from the **TY2015**
  (2015v3.0) MeF package (Final Release, May 27, 2016), mirrored publicly
  by CharityNavigator's `990_metadata` project. The TY2025 Common file is
  SOR-gated like the rest of the package and was not publicly obtainable.
- Consequence, stated plainly: validation resolves the shared types
  (USAmountType, SSNType, name/address types, …) as of TY2015. The shared
  types exercised by the emitted documents are stable across that span, and
  every emitted element name/order comes from the TY2025 per-form schemas
  above; but this is a **disclosed substitution**, not the exact TY2025
  Common file. Three leaf types referenced by the TY2025 forms but absent
  from the TY2015 Common (`MilesNNType`, `SpecialConditionTextType`,
  `Text2Type`) are supplied by the harness at validation time; none of them
  gates an element the exporter emits.
- The envelope schemas (`Return1040.xsd`, `ReturnHeader1040x.xsd`) are not
  publicly available at all, so the `Return`/`ReturnHeader` envelope is
  **not** XSD-validated; it follows IRS Publication 4164 (Rev. 12-2025)
  §2.10.1 and the element vocabulary of Code for America's production
  vita-min transmitter.

IRS schema files are works of the U.S. federal government.
