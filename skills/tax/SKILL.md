---
name: tax
description: Router and canonical process for opentax. Use when a user wants to prepare, review, or understand a US individual tax return with the `tax` CLI. Defines THE RETURN PROCESS (stages S0–S5); the stage skills (tax-intake, tax-ingest, tax-preparer, tax-reviewer, tax-filing-guide) hold the details for each stage.
---

# tax — router: THE RETURN PROCESS

You drive the `tax` CLI; the CLI does all calculation and validation.
A return is built in six stages, **S0–S5**, defined below. This file is
the only definition of the process — the stage skills hold details and
link back here.

**The progressive rule.** Do only the stages and branches the filer's
facts require. Never collect stage N+1 data during stage N. Most
branches (dependents, California, treaties) trigger off facts learned
in S1 — until then they do not exist. **When unsure what to do next,
run `tax return status --return <id>`: it returns the flow state
(`stage`, and `steps` each with `done` and the exact `command` to run
next).** Mutating commands also return `next_steps` with exact
commands — follow them instead of guessing.

**Payloads.** The CLI is the schema source of truth — never copy a
JSON shape from any skill, including this one. Get shapes from
`tax node template --document <type>` (document entries) and
`tax schema` (stored blocks and command inputs). Every payload
command (`form add`, `return set-identity`, `return set-state`,
`return set-nra`) accepts the payload three ways: `--file <path>`,
`--data '<json>'` inline, or `--file -` (stdin). Money is always
**integer cents as a string**: `"5000000"` is $50,000.00.

## Quickstart — the happy path (resident, one W-2)

After S0 setup (below), the whole return is eight commands:

```sh
tax forms fetch                                            # 1. all 14 blank form templates, verified
tax return create --year 2025 --status single              # 2. → note the returned returnId
tax doc extract --file w2.pdf --add-to <id>                # 3. extract the W-2 and enter it, one step
tax return set-identity --return <id> --data '<identity>'  # 4. identity payload — shape: tax schema
tax return set-other-income --return <id> --interest-cents 0 --dividends-cents 0
                                                           # 5. record direct interest/dividends (even 0)
tax return get --return <id>                               # 6. every line, with completeness
tax return validate --return <id>                          # 7. deterministic findings — work them all
tax return export --return <id> --type packet --out-dir packet/
                                                           # 8. filled forms + manifest.json
```

Then S5: read `packet/manifest.json` and tell the filer what is
filled, what is not applicable, and what they complete by hand.

## The stages

### S0 — Setup (once per machine)

*Purpose:* a working, trustworthy CLI before any real data.
*Entry:* a checkout of the repo.
*Commands:* `cargo build --release` (binary: `target/release/tax`) ·
`tax doctor` (follow any failing check's `hint` exactly) ·
`tax forms fetch` (downloads + verifies all 14 IRS/FTB templates
into the forms dir — default `.opentax/forms`; override with
`--forms-dir` or `OPENTAX_FORMS_DIR`; idempotent) ·
`tax selftest` (the golden cases).
*Done when:* doctor passes, selftest passes, fetch reports all
templates present. If selftest fails, do not touch a real return;
report it.

### S1 — Route (details: tax-intake)

*Purpose:* decide **which form** this filer files, then create the
return. **A wrong route invalidates everything downstream** —
residency is asked first, before income, before documents.
*Entry:* S0 done; the filer available for questions.
*Commands:*
- Citizen / resident alien → resident route (Form 1040):
  `tax return create --year 2025 --status <single|mfj|hoh|mfs>`.
- F-1 / J-1 visa, **≤ 5 calendar years in that status** (any part of
  a year counts) → nonresident student route (Form 1040-NR): create
  with `--status single` (the only status this route supports),
  then `tax return set-nra --return <id> --data '<nra block>'`
  (payload shape: `tax schema`). Citizenship CN / IN carry treaty
  modeling; any other country computes with **no** treaty benefits
  and a loud `treaty_not_modeled` warning — say so plainly.
- F/J **> 5 years**, dual-status years, married NR filers, other
  visas → **stop**: out of scope (the filer is likely a resident
  alien under the substantial presence test; `set-nra` refuses
  > 5 years by design).
*Done when:* the return exists and the route is stored — resident:
`tax return get` computes Form 1040 lines; NR: `return get` shows
`"form": "1040-NR"`. `tax return status` shows the route/create
steps done.

### S2 — Documents (details: tax-ingest)

*Purpose:* every income document becomes an entered entry, verbatim.
*Entry:* S1 route stored; the filer has documents (or states facts
for direct entry).
*Commands:* per document —
`tax doc extract --file <pdf> --add-to <id>` (born-digital: extract
+ enter in one step; the result carries per-field `evidence` and
`checks` — review them), or extract first, review, then
`tax form add --return <id> --node-type <type> --file <entry.json>`.
Entry shapes come from `tax node template --document <type>` — never
from memory.
*Routing table* (what a document becomes):

| Document | Entry | Notes |
|---|---|---|
| W-2 | `w2` | extract handles it; CA route also needs Boxes 16/17 transcribed |
| 1099-INT | `1099-int` | on the NR route, bank interest is exempt — see tax-ingest |
| 1099-DIV | `1099-div` | qualified vs ordinary matters; copy both boxes |
| 1099-B | `1099-b` | **never auto-extracted** (layouts vary) — vision-transcribe per transaction |
| 1099-NEC | `1099-nec` | `expenses` is NOT on the form — ask the filer (their records) |
| 1042-S | `f1042s` | NR route only; income code 20 = wages, 15 = scholarship/fellowship |

*Branch on the extract `status`:* `extracted` → review evidence,
enter. `partial` → transcribe only the missing fields by hand.
`needs_ocr` / `unrecognized` → vision fallback (tax-ingest step 5);
if the type is still unsure, stop for that document — do not guess.
*Done when:* every document on the S1 inventory has an entry (or a
documented stop), and `tax return validate` shows no finding about a
missing entry.

### S3 — Identity & options (details: tax-intake)

*Purpose:* store who the filer is, and open only the branches their
facts trigger.
*Entry:* return created (any time after S1; before S4 completes).
*Commands (core, every return):*
- `tax return set-identity --return <id> --data '<identity>'` —
  names, SSN, date of birth, occupation, address, blindness.
  Stored locally; SSNs are masked in all output.
- `tax return set-other-income --return <id> --interest-cents <n> --dividends-cents <n>`
  — direct interest/dividend totals, recorded **explicitly, even
  when 0**. Unrecorded is *incomplete*, never zero. (Totals already
  passed at `return create` count as recorded.)
*Branches — only when S1 facts trigger them:*
- **Dependents** → one `dependent` entry per person via `form add`
  (unlocks CTC/ODC, EIC, CA Young Child Tax Credit). NR route: skip —
  no credits are computed there.
- **California** (full-year CA resident, **resident route only**) →
  `tax return set-state --return <id> --state CA --data '<state block>'`
  (unlocks Form 540 alongside the 1040). Any other state, or
  part-year/nonresident CA → out of scope. A state block and an nra
  block together are an explicit error (`nra_state_conflict`).
- **NR route** → already routed in S1; in S3 collect the Form 8843
  facts (school/director/days present) into the nra block.
*Done when:* `return get` shows the identity set, `validate` no
longer reports `other_income_not_recorded` / `identity_not_set`,
and every triggered branch is stored.

### S4 — Compute & review (details: tax-preparer, tax-reviewer)

*Purpose:* a complete, validated, explainable return.
*Entry:* S2 + S3 done.
*Commands:* `tax return get --return <id>` ·
`tax return validate --return <id>` ·
`tax explain --return <id> --line <n>` (CA lines: add `--state CA`).
*How to work it:* read **completeness, not just totals** — lines
marked `"state": "incomplete"` are missing facts, not zeros; the
return's overall `complete` is false while either the federal side
or a state section is incomplete. Work **every** validate finding:
errors block calling the return final; findings named in the stage
skills (FICA withheld while exempt, CA withholding missing,
credit gates) have specific handling — never silence one by editing
a number. Explain any line the filer questions, quoting the
engine's derivation. The reviewer pass (tax-reviewer) independently
traces entries to source documents and tracks readiness.
*Done when:* `return get` reports `"complete": true`, validate has
no unaddressed error-severity findings, and the filer has seen the
summary (refund/owed quoted from engine output — lines 34/37;
CA: state lines 97/100; NR: 1040-NR lines 34/37).

### S5 — Export & file (details: tax-preparer for export, tax-filing-guide for filing)

*Purpose:* produce the paperwork and hand the filing step to the
filer. **The CLI never files, transmits, signs, or pays.**
*Entry:* S4 done.
*Commands:* `tax return export --return <id> --type packet --out-dir <dir>`
(filled forms + `manifest.json`; `--forms-dir` optional — templates
come from the forms dir fetched in S0). Single-form and data
variants: `--type pdf` (one filled form; NR route: the 1040-NR),
`--type json`, `--type mef` (resident route only; a standards-shaped
artifact, **not** a filing channel).
*Done when:* you have **read the manifest aloud to the filer**:
every form `filled` or `not_applicable` (with its reason), every
`missing_template` treated as a blocker (recovery: `tax forms fetch`),
and the manifest's hand-complete list (signature, county, fields
never stored) delivered. Then tax-filing-guide owns the ending —
assembly order, what copies to attach, due dates, and the mailing
address **for the route**: resident (IRS service center), CA (FTB —
a separate envelope), NR (Austin / Charlotte; Form 8843 attaches to
the 1040-NR, or mails alone when no return is due).

## Error → recovery

Errors arrive as `{"success": false, "error": "<message>", "code":
"<code>"}` on stderr; some carry extra fields (named below).

| Code | Means | Recovery |
|---|---|---|
| `return_incomplete` | a line the step needs is incomplete; extra `incomplete_lines`; on packet export also `missing_inputs`: `[{input, command}]` root causes | run each `missing_inputs[].command`; otherwise find the missing fact via `return status` / `return get` and go back to its stage — never zero-fill |
| `missing_template` | a needed blank form PDF is not in the forms dir (also a per-form manifest status) | `tax forms fetch`, then retry |
| `missing_out` | an export flag is missing (`--out` / `--out-dir`) | re-run with the flag |
| `nra_state_conflict` | a state block and an nra block on the same return | the routes are mutually exclusive — CA full-year resident + NR is out of scope; remove the wrong block by re-running the correct S1/S3 step |
| `unsupported_state` | `set-state` with anything but CA + full-year resident | stop that part; other states / 540NR are out of scope |
| `unsupported_tax_year` | a year the engine/schema package does not cover (v1: TY2025 only) | stop; do not approximate another year |
| `nra_determination_failed` | the stored facts do not determine NR status (e.g. > 5 years in status) | revisit S1 with the filer; likely resident route or out of scope |
| `identity_required` | an export (MeF) needs a stored identity and none is set | S3: `tax return set-identity …`, then retry |
| `invalid_input` | a value failed validation (e.g. negative amount, bad field) | fix the named value; payload shapes: `tax schema` / `tax node template` |
| `invalid_json` | a payload did not parse | fix the JSON; compare against `tax schema` |
| `invalid_status` | a filing status the CLI does not accept | use single / mfj / hoh / mfs (NR route: single only) |
| `return_not_found` | no return with that id in the store | `tax return list` for the real id; check `--store` points at the right directory |
| `io_error` | the store or a file could not be read/written | check the path and permissions; `tax doctor` diagnoses the store |
| `not_implemented` | the CLI does not do this (e.g. MeF for an NR return, an unmodeled node type) | honest stop — say so; see `docs/KNOWN_LIMITS.md` |
| `unknown_fact` | `tax explain` was asked for a line/fact the engine does not carry | ask for a line that appears in `return get` |
| `extract_error` / `pdf_error` / `pdf_fill_*` | the PDF layer failed on a file | verify the file is the document claimed; for fills, the template must be the blank TY2025 form (`tax forms fetch`) |
| extract status `needs_ocr` | the PDF is scanned/image-only; v1 ships no OCR | vision fallback (tax-ingest step 5); mark figures for human confirmation |
| extract status `unrecognized` | not an extractable type (includes all 1099-B statements) | `tax doc classify --file <path>` to identify; vision fallback; if still unsure, stop for that document |

## Which skill holds which stage's details

- **tax-intake** — S1 questions (residency script, filing status,
  dependents, CA/NR branches) and the S3 identity facts.
- **tax-ingest** — S2: extraction evidence rules, per-document
  notes, the vision fallback. The only skill that touches raw
  documents.
- **tax-preparer** — S4 presentation (how to read `return get`,
  the credit/withholding chains) and S5 export mechanics.
- **tax-reviewer** — the S4 independent check: source tracing,
  cross-checks, the four readiness statuses.
- **tax-filing-guide** — S5's ending: channels, assembly, mailing
  facts per route. The only skill allowed near the filing step.

## Iron rules (bind every stage)

1. **Never do arithmetic yourself.** Quote engine output (`tax return
   get`, `tax explain`) verbatim. If a number is not in engine
   output, it does not go on the return.
2. **Every extracted number is a verbatim transcription with its
   source recorded** (document, page, box). Never round, infer, or
   "correct" a transcribed value — flag discrepancies via
   `tax return validate` instead.
3. **Missing is incomplete, never zero.** When facts are missing,
   name them; never fill them with zeros or guesses.
4. **The validator must pass before a computed return is trusted.**
   Run `tax return validate` and surface every finding to the filer.
5. **Never touch credentials.** No EFIN/ETIN, IRS account, passwords,
   or PINs. The filer alone submits/files.
6. **Out of scope → say so and stop.** Never approximate a form,
   schedule, or rule the CLI does not compute
   (see `docs/KNOWN_LIMITS.md`).
7. **Privacy:** warn the filer before reading any tax document;
   everything is local by default and nothing is sent anywhere by
   the CLI.

## Authority limits

You prepare and explain; you do not file, sign, or give personalized
tax advice beyond what the engine computes. Scope is TY2025:
resident Form 1040 (W-2, 1099-INT/DIV/B/NEC, dependents + credits,
EIC, senior deduction), California Form 540 for full-year residents,
and the Form 1040-NR student route (F-1/J-1, ≤ 5 years, CN/IN
treaties modeled) — exact boundaries in `docs/KNOWN_LIMITS.md`.
