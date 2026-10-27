# PDF fill spike — lopdf (/V only) vs pdfer_forms vs pdfcpu on real IRS forms

Date: 2026-10-26 (America/Los_Angeles). Spike code: `spikes/pdf-fill/`
(standalone Cargo project, not a workspace member). All filled data is
synthetic ("Spike Tester", SSN 123-45-6789, …). No real personal data.

## Verdict

**The pure-Rust lopdf `/V`-only path in `tax-pdf` cannot fill real IRS
forms today — for two independent reasons, either of which is fatal.**

1. **It cannot even address the fields.** IRS LiveCycle Designer PDFs store
   every AcroForm `/T` field name as a UTF-16BE PDF string (with BOM).
   `tax-pdf` decodes names with `String::from_utf8_lossy`, so on all three
   real forms **100% of inspected field names come back mangled**
   (`\u{feff}\0t\0o\0p\0m…`). Counts are right (199 / 55 / 202 terminal
   fields, matching pdfcpu and pypdf exactly), but no caller-supplied name
   ever matches: `fill_fields` filled **0 of 57** target fields.
2. **Even with names fixed, `/V`-only filling does not produce a rendered
   form.** Bypassing the name bug (feeding `fill_fields` its own mangled
   names) sets text/comb `/V` values that pypdf reads back fine (53/53),
   but creates **zero appearance streams**, writes checkbox `/V` as a
   *string* instead of a *name* (`TextStringObject "/1"`, spec-invalid for
   `/Btn`), never sets widget `/AS`, and thereby **breaks other tools**:
   pdfcpu aborts its field-tree parse of the whole file
   (`entry=V: expected name, got types.StringLiteral`). Setting
   `NeedAppearances=true` (arm A2) changed nothing measurable — renders are
   byte-identical to A1; the flag is advice to viewers, it does not create
   appearances in the file.

**pdfer_forms (arm B) and pdfcpu (arm C) both produce filled forms that
read back AND render**: pypdf readback 57/57 for both, regenerated `/AP`
on 57/57 widgets, checkbox `/V` as a proper name with widget `/AS` set.
The deciding evidence: comb fields and checkboxes (below) — plus two
caveats that keep this from being a clean win for pdfer_forms: its
regenerated appearances **do not spread comb digits** across cells
(pdfcpu's do), and its leaf-name matching fallback **double-set a second
filing-status field** on Form 1040. Recommendation: adopt pdfer_forms as
the default fill backend with guards (see Recommendation), fix tax-pdf's
string decoding regardless, keep pdfcpu as reference/fallback.

### Deciding evidence — comb field (taxpayer SSN, `f1_16`, 9 cells)

Crops of the rendered SSN box are committed at
`spikes/pdf-fill/results/crop-ssn-{A1,B,C}-1.png`:

- **A1 (lopdf /V only):** digits render spread across the comb cells —
  but only because *poppler itself* regenerates appearances from `/V`
  when `/AP` is absent. The file contains no appearance; any consumer
  that does not regenerate (print pipelines, several mobile/desktop
  viewers) sees a blank box.
- **B (pdfer_forms):** digits render as one contiguous string
  (`123456789` crammed left-aligned, not spread into cells). Data is
  correct; the regenerated appearance ignores the comb flag.
- **C (pdfcpu):** digits render spread across the cells — its appearance
  generation honours comb.

### Deciding evidence — checkboxes

Data level (the level other software reads): **B and C** set field `/V`
to a `NameObject` on-state and the widget `/AS` to the same state, for
all 4 target checkboxes (1040 filing status Single = `/1`; Schedule D
QOF "No" = `/2` — the on-state really is `/2` there; Schedule D line 17
Yes = `/1`; 8949 Box A = `/1`). **A1/A2** wrote a string `/V` and left
`/AS = /Off` on every checkbox.

Render level: **no arm's check mark renders in poppler** — including a
hand-built control (pypdf setting `/V` + `/AS` on the blank original),
whose widget-region pixels are identical to the blank form (508 dark
pixels in the test region for blank, A1, B, C, and the control). The
on-state appearance stream in the original is a valid ZapfDingbats ✔
(73-byte stream), so a conforming renderer selecting by `/AS` (Acrobat,
PDFium) has what it needs; poppler simply does not render the on-state
for these Designer radio widgets. The checkbox render test is therefore
**inconclusive via poppler** and the verdict rests on the data-level
state, which B and C get right and A1/A2 get wrong-typed.

## Fixtures

Downloaded 2026-10-26 from irs.gov. PDFs are **not** committed
(`spikes/pdf-fill/fixtures/*.pdf` is git-ignored).

| Form | URL | SHA-256 | Pages | PDF | Producer / revision |
|---|---|---|---|---|---|
| Form 1040 (TY2025) | https://www.irs.gov/pub/irs-pdf/f1040.pdf | `3d31c226df0d189ced80e039d01cf0f8820c1019681a0f0ca6264de277b7e982` | 2 | 1.7 | Designer 6.5; pdfinfo Title "2025 Form 1040", created 2026-01-02 |
| Schedule D | https://www.irs.gov/pub/irs-pdf/f1040sd.pdf | `90564c8b7e49280363612639b804d113ebedb516c2e87c70649f29c844da1d2e` | 2 | 1.7 | Designer 6.5; "2025 Schedule D (Form 1040)", created 2025-12-17 |
| Form 8949 | https://www.irs.gov/pub/irs-pdf/f8949.pdf | `274513891e4e281d11e14286f14b9df724c24b6030fca0f0681da64fb2e7f525` | 2 | 1.7 | Designer 6.5; "2025 Form 8949", created 2025-12-11 |

All three are **XFA + AcroForm hybrids** (AcroForm dict contains `/XFA`,
`SigFlags 2`). **`/NeedAppearances` is absent in all three** (i.e. not
set — viewers are not asked to regenerate appearances). Ground-truth
tools: pdfcpu **0.16.3** (GitHub release, linux x86_64), pypdf **6.21.2**,
poppler **24.02.0** (`pdfinfo`/`pdftoppm`/`pdftotext`). pdfer_forms
**0.3.4** (crates.io; depends on lopdf ^0.40 — note our workspace uses
lopdf 0.46). tax-pdf uses lopdf **0.46.0**.

### Field enumeration reconciliation

| Form | pdfcpu `form list` (terminal) | pypdf total (incl. containers) | pypdf terminal (`/FT` present) | tax-pdf inspect | pdfer `get_fields` | XFA template `<field>` elements |
|---|---|---|---|---|---|---|
| f1040 | 199 (126 Tx + 73 Btn) | 229 | 199 | 199, **all names mangled** | 229, set identical to pypdf | 199 |
| f1040sd | 55 (47 Tx + 8 Btn) | 68 | 55 | 55, **all names mangled** | 68, set identical to pypdf | 55 |
| f8949 | 202 (190 Tx + 12 Btn) | 229 | 202 | 202, **all names mangled** | 229, set identical to pypdf | 203 |

Discrepancies, all explained: pdfer and pypdf count container nodes
(field_type `None`); tax-pdf and pdfcpu report terminal fields only.
The XFA template has one extra "field" on 8949 —
`topmostSubform[0].designer__defaultHyphenation[0]`, a design-time
pseudo-field with no AcroForm counterpart. After decoding UTF-16BE,
tax-pdf's field *counts* and types agree with everyone; its *names* do
not (see Verdict #1).

## Field-name mapping (the deliverable table)

Semantic identification comes from the XFA template embedded in each
PDF: every `<field>` carries an `<assist><speak>` accessibility string
naming its form line, comb fields carry `<comb numberOfCells>`, and
1040 / Schedule D line labels are bound to fields by `<draw name="Ln…">`
traversal refs. The generator is committed at
`spikes/pdf-fill/scripts/make_targets.py`; the machine-readable version
is `spikes/pdf-fill/targets.json` (57 targets: 1040 ×31, Sch D ×12,
8949 ×14). Values are the synthetic values filled identically in every
arm (checkbox value = the widget on-state used).

#### f1040

| Semantic field | Qualified AcroForm name | Kind | Value filled |
|---|---|---|---|
| taxpayer first name and middle initial | `topmostSubform[0].Page1[0].f1_14[0]` | text | `Spike T` |
| taxpayer last name | `topmostSubform[0].Page1[0].f1_15[0]` | text | `Tester` |
| taxpayer SSN (comb, 9 cells) | `topmostSubform[0].Page1[0].f1_16[0]` | comb | `123456789` |
| spouse first name and middle initial | `topmostSubform[0].Page1[0].f1_17[0]` | text | `Partner` |
| spouse last name | `topmostSubform[0].Page1[0].f1_18[0]` | text | `Tester` |
| spouse SSN (comb, 9 cells) | `topmostSubform[0].Page1[0].f1_19[0]` | comb | `987654321` |
| home address | `topmostSubform[0].Page1[0].Address_ReadOrder[0].f1_20[0]` | text | `123 Main St` |
| apartment no. | `topmostSubform[0].Page1[0].Address_ReadOrder[0].f1_21[0]` | text | `4B` |
| city | `topmostSubform[0].Page1[0].Address_ReadOrder[0].f1_22[0]` | text | `Springfield` |
| state | `topmostSubform[0].Page1[0].Address_ReadOrder[0].f1_23[0]` | text | `IL` |
| ZIP code | `topmostSubform[0].Page1[0].Address_ReadOrder[0].f1_24[0]` | text | `62704` |
| filing status = Single | `topmostSubform[0].Page1[0].Checkbox_ReadOrder[0].c1_8[0]` | checkbox | `/1` |
| line 1a | `topmostSubform[0].Page1[0].f1_47[0]` | text | `50000` |
| line 1z | `topmostSubform[0].Page1[0].f1_57[0]` | text | `50000` |
| line 2b | `topmostSubform[0].Page1[0].f1_59[0]` | text | `100` |
| line 3b | `topmostSubform[0].Page1[0].f1_61[0]` | text | `200` |
| line 9 | `topmostSubform[0].Page1[0].f1_73[0]` | text | `50300` |
| line 11a | `topmostSubform[0].Page1[0].f1_75[0]` | text | `50300` |
| line 11b | `topmostSubform[0].Page2[0].f2_01[0]` | text | `50300` |
| line 12e | `topmostSubform[0].Page2[0].f2_02[0]` | text | `15750` |
| line 15 | `topmostSubform[0].Page2[0].f2_06[0]` | text | `34550` |
| line 16 | `topmostSubform[0].Page2[0].f2_08[0]` | text | `3900` |
| line 24 | `topmostSubform[0].Page2[0].f2_16[0]` | text | `3900` |
| line 25a | `topmostSubform[0].Page2[0].f2_17[0]` | text | `4000` |
| line 25d | `topmostSubform[0].Page2[0].f2_20[0]` | text | `4000` |
| line 33 | `topmostSubform[0].Page2[0].f2_29[0]` | text | `4000` |
| line 34 | `topmostSubform[0].Page2[0].f2_30[0]` | text | `100` |
| line 35a | `topmostSubform[0].Page2[0].f2_31[0]` | text | `100` |
| routing number (comb, 9 cells) | `topmostSubform[0].Page2[0].RoutingNo[0].f2_32[0]` | comb | `123456789` |
| account number (comb, 17 cells) | `topmostSubform[0].Page2[0].AccountNo[0].f2_33[0]` | comb | `12345678901234567` |
| taxpayer occupation | `topmostSubform[0].Page2[0].f2_40[0]` | text | `Engineer` |

#### f1040sd

| Semantic field | Qualified AcroForm name | Kind | Value filled |
|---|---|---|---|
| name(s) shown on return | `topmostSubform[0].Page1[0].f1_1[0]` | text | `Spike Tester` |
| SSN | `topmostSubform[0].Page1[0].f1_2[0]` | text | `123456789` |
| qualified opportunity fund question = No | `topmostSubform[0].Page1[0].c1_1[1]` | checkbox | `/2` |
| line 4 | `topmostSubform[0].Page1[0].f1_19[0]` | text | `1000` |
| line 5 | `topmostSubform[0].Page1[0].f1_20[0]` | text | `2000` |
| line 7 | `topmostSubform[0].Page1[0].f1_22[0]` | text | `3000` |
| line 11 | `topmostSubform[0].Page1[0].f1_39[0]` | text | `4000` |
| line 15 | `topmostSubform[0].Page1[0].f1_43[0]` | text | `5000` |
| line 16 | `topmostSubform[0].Page2[0].f2_1[0]` | text | `3900` |
| line 18 | `topmostSubform[0].Page2[0].f2_2[0]` | text | `100` |
| line 17 (both gains) = Yes | `topmostSubform[0].Page2[0].c2_1[0]` | checkbox | `/1` |
| Part I row 1a totals, column (h) | `topmostSubform[0].Page1[0].Table_PartI[0].Row1a[0].f1_6[0]` | text | `6000` |

#### f8949

| Semantic field | Qualified AcroForm name | Kind | Value filled |
|---|---|---|---|
| name(s) shown on return | `topmostSubform[0].Page1[0].f1_01[0]` | text | `Spike Tester` |
| SSN/TIN | `topmostSubform[0].Page1[0].f1_02[0]` | text | `123456789` |
| Part I Box A (basis reported to IRS) | `topmostSubform[0].Page1[0].c1_1[0]` | checkbox | `/1` |
| row 1 column (a) | `topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row1[0].f1_03[0]` | text | `100 sh. XYZ Co.` |
| row 1 column (b) | `topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row1[0].f1_04[0]` | text | `01/15/2025` |
| row 1 column (c) | `topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row1[0].f1_05[0]` | text | `06/15/2025` |
| row 1 column (d) | `topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row1[0].f1_06[0]` | text | `10000` |
| row 1 column (e) | `topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row1[0].f1_07[0]` | text | `7000` |
| row 1 column (g) | `topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row1[0].f1_09[0]` | text | `500` |
| row 1 column (h) | `topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row1[0].f1_10[0]` | text | `3500` |
| row 2 totals, column (d) proceeds | `topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row2[0].f1_14[0]` | text | `10000` |
| row 2 totals, column (h) gain/loss | `topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row2[0].f1_18[0]` | text | `3500` |
| row 2 totals, column (d) proceeds | `topmostSubform[0].Page2[0].Table_Line1_Part2[0].Row2[0].f2_14[0]` | text | `10000` |
| row 2 totals, column (h) gain/loss | `topmostSubform[0].Page2[0].Table_Line1_Part2[0].Row2[0].f2_18[0]` | text | `3500` |

Notes on the mapping itself:

- Form 1040 filing status is **five separate merged field+widget
  objects**, not one radio field: left column
  `…Checkbox_ReadOrder[0].c1_8[0..2]` (Single/MFJ/MFS, on-states
  `/1`/`/2`/`/3`) and right column direct children of Page1,
  `topmostSubform[0].Page1[0].c1_8[0..1]` (HOH/QSS, on-states `/4`/`/5`).
  Two of them share the leaf name `c1_8[0]` under different parents —
  which is exactly what tripped pdfer_forms' leaf-matching (below).
- Schedule D's QOF Yes/No and lines 17/20/22 are likewise separate
  fields per option, with per-field on-states (`/1`, `/2`).
- 8949's Part I category boxes A/B/C/G/H/I are separate fields
  `c1_1[0..5]`, each single-widget.

## Results matrix

Per form × arm: targets attempted / pypdf `/V` readback OK / pdfcpu
`form export` readback OK / appearance OK (text/comb: widget has a
non-empty `/AP` `/N` stream; checkbox: widget `/AS` = on-state).
Verification code: `spikes/pdf-fill/scripts/verify.py`, raw results:
`spikes/pdf-fill/results/verify.json`.

| Form | Arm | Attempted | pypdf readback | pdfcpu readback | Appearance OK | Notes |
|---|---|---|---|---|---|---|
| f1040 | A — tax-pdf, clean names (as shipped) | 31 | 0 | 0 | 0 | **Name-matching failure: 0 filled** (UTF-16BE `/T` bug). |
| f1040 | A1 — tax-pdf /V-only, raw names | 31 | 30 | 0 | 0 | Text/comb `/V` correct; checkbox `/V` is a *string*. pdfcpu aborts field parse on the string `/V` (`expected name, got StringLiteral`) → reads nothing. No `/AP` created. |
| f1040 | A2 — A1 + `NeedAppearances=true` | 31 | 30 | 0 | 0 | Identical to A1 on every measure; page-1 render **byte-identical** to A1. |
| f1040 | B — pdfer_forms | 31 | 31 | 31 | 31 | Comb appearance not spread (see Verdict). **Side effect:** leaf-name fallback also set the right-column `c1_8[0]` (HOH) to its on-state `/4` — two filing statuses set at data level. |
| f1040 | C — pdfcpu | 31 | 31 | 31 | 31 | `/XFA` stripped by pdfcpu (see XFA note). |
| f1040sd | A | 12 | 0 | 0 | 0 | Same name failure. |
| f1040sd | A1 | 12 | 10 | 0 | 0 | 2 failures = the 2 checkboxes (string `/V`). |
| f1040sd | A2 | 12 | 10 | 0 | 0 | Same as A1. |
| f1040sd | B | 12 | 12 | 12 | 12 | — |
| f1040sd | C | 12 | 12 | 12 | 12 | — |
| f8949 | A | 14 | 0 | 0 | 0 | Same name failure. |
| f8949 | A1 | 14 | 13 | 0 | 0 | 1 failure = Box A checkbox (string `/V`). |
| f8949 | A2 | 14 | 13 | 0 | 0 | Same as A1. |
| f8949 | B | 14 | 14 | 12 | 14 | pdfcpu misses = row-1 date fields (b)/(c) — see below, affects all arms incl. C. |
| f8949 | C | 14 | 14 | 12 | 14 | Same 2 pdfcpu date-field misses. |

Control run **A1T** (tax-pdf /V-only, text/comb targets only, no
checkboxes): pdfcpu readback **30/30 (1040), 10/10 (Sch D), 11/13
(8949)**. This isolates the checkbox string-`/V` as the sole cause of
pdfcpu's total readback failure on A1/A2, and shows lopdf-written text
values are otherwise perfectly legible to pdfcpu. The 2 missing 8949
fields under pdfcpu are the date fields `…Row1[0].f1_04[0]` /
`f1_05[0]`: pdfcpu's `form export` does not list them for **any** arm
(including its own fill), a pdfcpu export quirk for these fields —
pypdf reads the correct values (`01/15/2025`, `06/15/2025`) in arms B
and C, so the fills are right and the *reader* is the gap.

Text visibility (`pdftotext` of the output contains the value;
whitespace-insensitive for digits): text/comb targets **53/53 visible**
in arms A1, A2, B, C on all forms. Read this correctly: poppler
regenerates text appearances from `/V` on the fly, so pdftotext/pdftoppm
visibility does **not** prove a file carries appearances — `/AP`
presence (the matrix column) does. The pdftotext signal mainly confirms
poppler-class renderers will show A1/A2 values; it says nothing about
Acrobat, print pipelines, or macOS Preview.

### Checkboxes — what each arm actually does

- **Arm A (as shipped):** nothing — no field is ever matched.
- **Arm A1/A2 (tax-pdf):** writes `/V` as a PDF *string* `"/1"` for every
  field type, and never touches widget `/AS` (stays `/Off`). For `/Btn`
  fields the spec requires `/V` to be a *name*; the string value is
  wrong-typed, renders unchecked even in regenerating viewers, and
  poisons pdfcpu's field-tree parser for the whole document.
- **Arm B (pdfer_forms):** `FieldInput::from("/1")` is resolved against
  each field's actual appearance states (`resolve_button_state`), `/V`
  is written as a name, `/AS` is set on the widget, and a text
  appearance is regenerated. Correct on all 4 targets — **plus** the
  unwanted HOH field, because its matcher also accepts a *leaf-name*
  match (`partial_name == requested leaf`), and two distinct 1040
  fields share the leaf `c1_8[0]`.
- **Arm C (pdfcpu):** JSON `checkbox: value=true` per exact qualified
  name; `/V` name + `/AS` set correctly, no collateral fields.

### Comb fields — what each arm does

Comb is a *rendering* property (field flag bit + `MaxLen`/XFA
`<comb numberOfCells>`); the stored `/V` is the plain digit string in
every arm. A1's file has no appearance at all (poppler spreads the
digits when regenerating; nothing in the file does). B's regenerated
appearance draws the string contiguously, ignoring comb — visually
wrong for a form whose cells are the point (see the committed SSN
crops). C's appearance spreads digits one per cell, as the printed form
intends. Four comb targets were exercised: taxpayer SSN (9), spouse SSN
(9), routing number (9), account number (17).

### Flatten test (Form 1040, arm B with `flatten=true`)

`update_page_form_field_values(..., flatten=true)` bakes the filled
appearances into the page content streams. Decisive test: strip **all**
widget annotations from the output with pypdf, then extract text —
flattened output still shows `Spike T` and `123456789`; the unflattened
arm-B output shows neither. Caveats: field objects remain in the
AcroForm tree (pypdf still lists 229 fields and `/V` readback still
works) — pdfer's flatten merges appearances but does not remove the
widgets/fields; its own example calls `remove_annotations(["/Widget"])`
separately for full flattening. Treat "flatten" in pdfer_forms as
*bake-in*, with annotation removal as an explicit second step.

### The XFA note (applies to every arm)

All three forms are XFA hybrids. **No AcroForm fill arm updates the XFA
datasets** (in arms A1/B/Bflat the datasets stream is byte-identical to
the blank original, length 3715, no filled values). pdfcpu instead
**removes `/XFA`** from its output entirely, forcing every viewer onto
the AcroForm layer. pdfer_forms keeps `/XFA` with stale datasets.
Practical consequence: a viewer that prefers the XFA layer (Acrobat's
default for such forms) may present stale/blank data for a pdfer-filled
form whose AcroForm layer is perfect. We could not test Acrobat in
this environment (no Acrobat/PDFium renderer available); the poppler
results above describe the AcroForm layer only. Any production fill
path must make an explicit XFA decision (strip it, like pdfcpu, or
update datasets) rather than inherit one by accident.

## Recommendation

**Default backend: pdfer_forms, behind the existing `PdfBackend`
trait — with guards. Do not keep lopdf `/V`-only filling; do not rely
on `NeedAppearances`.** (Decision + implied code changes only; this
spike implements none of them in `crates/`.)

Why: pdfer_forms is the only pure-Rust arm that produces files which
read back correctly in independent tools *and* carry their own
appearances, including correct checkbox name/`/AS` handling and a
working bake-in flatten. Its defects (comb appearances, leaf-match
fallback, lopdf 0.40 dependency vs our 0.46) are bounded and testable;
the lopdf path's defects (name decoding, wrong-typed checkbox values,
no appearances at all) are in the foundation.

Implied code changes for `crates/tax-pdf`:

1. **Fix PDF string decoding first, regardless of backend.** Decode
   `/T` (and `/V`) as PDF text strings: UTF-16BE when a BOM is present,
   PDFDocEncoding otherwise — never `from_utf8_lossy` on raw bytes.
   This repairs `inspect_fields` (currently 100% mangled names on real
   IRS forms) and is prerequisite to everything else. Add a regression
   test against a real-form-shaped fixture.
2. **Add a `PdferBackend` implementing `PdfBackend`** via pdfer_forms
   0.3.4: fill = `update_page_form_field_values` with
   `NeedAppearances` explicitly false (appearances are generated, not
   requested); checkboxes passed as their on-state name strings.
   Reconcile the lopdf version split (pdfer_forms pins lopdf ^0.40;
   the workspace uses 0.46) — accept both versions in the tree
   initially, or vendor the trait boundary so only tax-pdf sees it.
3. **Guard the leaf-name fallback.** Before filling, detect duplicate
   partial (leaf) names among the form's fields; after filling, diff
   the full field set and fail the fill if any non-target field
   changed (this catches the HOH double-set). Both checks are cheap
   with `get_fields` before/after.
4. **Track comb appearances as a known defect** in KNOWN_LIMITS:
   values are data-correct but not cell-spread under pdfer_forms.
   Options when it matters (paper-print fidelity): route comb fields
   through the fallback backend, or post-process comb appearances.
   Do not silently ship it.
5. **Keep `LopdfBackend` for inspect/text extraction** (after fix 1)
   and as a zero-dependency fallback whose output is labelled
   "values present, appearances not generated — verify in a
   regenerating viewer".
6. **Keep pdfcpu as the external reference/verification tool** in the
   spike harness (and evaluate it as the fallback fill backend: it is
   the only arm that spreads comb *and* strips XFA — but it is an
   external Go binary, not a library, so it cannot be the in-process
   default for a single-binary CLI).
7. **Decide XFA explicitly** in the fill API (strip like pdfcpu, or
   document that output targets AcroForm renderers), and say which in
   the output metadata. Do not let the backend choose by accident.

## Reproduction

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd spikes/pdf-fill

# fixtures (git-ignored; record hashes after download)
mkdir -p fixtures
for f in f1040 f1040sd f8949; do
  curl -sL -o fixtures/$f.pdf https://www.irs.gov/pub/irs-pdf/$f.pdf
done
sha256sum fixtures/*.pdf

# ground truth tools
#   pdfcpu 0.16.3: https://github.com/pdfcpu/pdfcpu/releases/download/v0.16.3/pdfcpu_0.16.3_Linux_x86_64.tar.xz
#   pypdf:         python3 -m venv /tmp/venv && /tmp/venv/bin/pip install pypdf   (6.21.2 used)

# targets + spike binary
python3 scripts/make_targets.py fixtures targets.json   # needs pypdf
cargo build

B=./target/debug/pdf-fill-spike
for f in f1040 f1040sd f8949; do
  $B enumerate   fixtures/$f.pdf results/enum-taxpdf-$f.json
  $B enumerate-b fixtures/$f.pdf results/enum-pdfer-$f.json
  $B fill-a      fixtures/$f.pdf targets.json $f results/$f-A.pdf
  $B fill-a-names fixtures/$f.pdf targets.json $f results/enum-taxpdf-$f.json results/$f-A1.pdf
  $B fill-a2     results/$f-A1.pdf results/$f-A2.pdf
  $B fill-b      fixtures/$f.pdf targets.json $f results/$f-B.pdf
  python3 scripts/fill_pdfcpu.py /path/to/pdfcpu fixtures/$f.pdf targets.json $f results/$f-C.pdf
done
$B fill-b fixtures/f1040.pdf targets.json f1040 results/f1040-Bflat.pdf --flatten

# verification (readback + appearances + text visibility)
python3 scripts/verify.py /path/to/pdfcpu results targets.json results/verify.json

# renders + crops (poppler)
pdftoppm -png -r 100 -f 1 -l 1 results/f1040-B.pdf results/f1040-B-p1
pdftoppm -png -r 100 -f 1 -l 1 -x 640 -y 120 -W 170 -H 40 results/f1040-B.pdf results/crop-ssn-B
```

Repository gates (must stay green; run from repo root):
`cargo fmt --check`, `cargo clippy --workspace -- -D warnings`,
`cargo test --workspace`, `tax selftest`.
