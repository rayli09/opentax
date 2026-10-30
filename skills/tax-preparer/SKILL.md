---
name: tax-preparer
description: Preparation details for opentax stages S4 (compute & present) and S5 (export) of THE RETURN PROCESS (skills/tax). Use when facts/documents are entered and the return should be computed, presented line by line, validated, explained, and exported (packet / PDF / JSON / MeF).
---

# tax-preparer — S4 presentation and S5 export details

The process lives in the router (skills/tax, stages S4–S5). This
skill holds the details: how to read and present `return get`, the
chains worth presenting explicitly, the validate findings you will
meet, and the export mechanics.

## Presenting `tax return get`

Quote lines verbatim from the output. Lines marked
`"state": "incomplete"` are **missing facts, not zeros** — list
them for the user and go back to the stage that owns the fact;
never present a total as final while the return is incomplete.

- **State section**: when the return has a state block, the output
  gains a top-level `"state"` object (California Form 540 in v1)
  with its own `lines`, `complete`, `incomplete`, and `notes`.
  Present it after the federal lines, keyed by the **540's printed
  line numbers** (a different form — never blend them into the
  federal list): CA AGI (17), taxable income (19), tax (31 — the
  output names the method as `line31_method`: `ca_tax_table` or
  `ca_rate_schedule`), exemption credits after the AGI limitation
  (32), total tax (64), withholding (71), CalEITC (75), Young
  Child Tax Credit (76), refund (97) / amount owed (100). The
  return's overall `complete` is false when either side is
  incomplete — say which side, and why, using the state section's
  own `incomplete` list.
- **NR route**: `return get` shows `"form": "1040-NR"` and the
  lines are the 1040-NR's printed lines, plus an `nra` object
  (determination, tax method, Schedule OI data, notes). Present it
  the same way: completeness first, then the refund (34) / amount
  owed (37).

Explain any line the user questions with
`tax explain --return <id> --line <n>` and quote its derivation
and citation. For a California line add `--state CA`
(`tax explain --return <id> --state CA --line 31`) — the return
must have the CA block stored, and the line numbers are the
540's. Do not improvise an explanation the CLI did not produce.

## Presentation chains (quote the engine's keys, name the gates)

- **Tax method (federal line 16)** is figured by the QDCG
  worksheet, the Tax Table, or the Tax Computation Worksheet; the
  engine reports which as `line16_method` — name it.
- **Side income (1099-NEC present):** Schedule C net profit
  (internal key `schc_net_profit`) on line 8, self-employment tax
  on line 23, its deductible half on line 10, and total tax on
  line 24 (= line 22 + line 23) — the amount owed/refund (34/37)
  is figured against line 24, not line 16.
- **Dependents/credits:** tentative credit (internal key
  `sched8812_tentative`), the nonrefundable part on lines 19/21,
  tax after credits on line 22, any additional child tax credit
  on line 28.
- **Earned income credit — present on every return, not just ones
  with dependents:** the qualifying-child count (internal key
  `eic_qualifying_children` — a count, shown as count × $1.00),
  the EIC earned income (internal key `eic_earned_income` — wages
  plus Schedule SE net earnings minus the deductible part of SE
  tax; note this differs from the Schedule 8812 earned-income
  key, which does not subtract SE tax), the investment-income
  figure (internal key `eic_investment_income`; over $11,950 in
  TY2025 the credit is zero by IRC §32(i)), and line 27 itself.
  Line 32 = line 27 + line 28 feeds total payments on line 33.
  When line 27 is zero, say which reason the engine reports (its
  compute notes name it: MFS, investment income over the limit,
  no positive earned income, phaseout complete, the childless age
  test unmet, or undeterminable because no date of birth is
  stored) — and surface the matching validate findings
  (`eic_mfs`, `eic_childless_age_unknown`,
  `eic_investment_income_limit`, `dependent_eic_ssn`) rather than
  letting a zero pass silently.
- **Senior deduction:** present the eligible-person count and
  per-person amount (Schedule 1-A line 35, after the 6% MAGI
  phaseout) on line 13b, and total deductions on line 14
  (= line 12 + line 13b). Line 13a (QBI) is never computed in
  v1, and Schedule 1-A Parts II–IV (tips / overtime / car loan
  interest) are out of scope.
- **CalEITC:** present its chain like the federal EIC's — the
  qualifying-child count (internal key
  `caleitc_qualifying_children`), the earned income (internal key
  `caleitc_earned_income`, from W-2 Box 16), and line 75. When it
  is zero, the state section's notes name the gate that decided
  it (MFS, the $32,901 AGI limit, the $4,814 investment-income
  limit, the childless age test) — quote them rather than letting
  a zero pass silently.

## Validate findings you will meet

`tax return validate` — surface **all** findings, grouped by
severity. Errors block export-as-final. With a state block set,
validate also reports the California findings — surface them with
the federal ones:

- `ca_state_withholding_missing` — a W-2 Box 17 or 1099-NEC Box 5
  was never transcribed; names the entry. Go back and record the
  box, or `"0"` when the form reports none.
- `ca_wages_box16_missing` — CalEITC needs W-2 Box 16.
- `caleitc_mfs`, `caleitc_ssn_unverified`.
- `yctc_zero_earned_income_path` — the YCTC zero-earned-income
  expansion is not computed in v1; line 76 is honestly
  incomplete.
- `ca_nec_classification_not_modeled` (info).

On the NR route, expect instead: `fica_withheld_while_exempt`
(a W-2 shows Social Security/Medicare withheld during the filer's
exempt years — the remedy is a refund claim, employer first, then
Forms 843/8316; it is **not** fixed by editing the return),
`nra_no_standard_deduction` / `nra_standard_deduction_india`
(info — why line 12 is what it is), `treaty_not_modeled`,
`treaty_wage_cap_reached`, `nra_schedule_nec_not_modeled`,
`nra_credits_not_modeled`, `scholarship_taxable`. The router's
error table covers command-level error codes.

## Export (S5)

- **Packet (the filing artifact):**
  `tax return export --return <id> --type packet --out-dir <dir>`
  — the 1040 plus every applicable schedule, filled; `--forms-dir`
  is optional (templates default to the forms dir from
  `tax forms fetch`). The packet writes `manifest.json` in the
  out-dir reporting every form as `filled`, `not_applicable`
  (with the reason), or `missing_template`. **Read the manifest
  before presenting the packet:** a `missing_template` entry
  means the return needs that form but its template was missing
  — recovery is `tax forms fetch`, then re-export; never present
  it as a completed packet. Likewise surface the per-form
  `notes` (unfilled lines and why, Form 8949 overflow, Schedule
  SE's Box 1 proxy on line 8a) rather than letting them pass
  silently. If the export refuses with `return_incomplete`, its
  `missing_inputs` list names the root-cause inputs with the
  exact command for each — work those, don't hunt. Packet scope
  is in `docs/KNOWN_LIMITS.md` (Schedule D / 8949, Schedules 1,
  2, C per 1099-NEC engagement, SE, 8812 Parts I + II-A only,
  Schedule 1-A Parts I/V/VI only). With a state block, the packet
  also fills the Form 540; on a 1040-NR return it instead fills
  the 1040-NR, Schedule OI, and Form 8843.
- **Single-form PDF:** `--type pdf --template <blank TY2025
  f1040.pdf> --out <file>` fills amount lines, filing status, and
  the page-1 Dependents table for stored dependents (names,
  stored SSNs, credit checkboxes, first four); with an identity
  block stored, also the header names/SSNs/address, the page-2
  occupations, and the standard-deduction age/blind/claim
  checkboxes. Identity pieces that are not stored are reported as
  skipped, never invented; SSNs in the export report are masked
  — the full SSN appears only inside the filled PDF. On an NR
  return, `--type pdf` fills the 1040-NR alone.
- **JSON:** `--type json` — the computed return as data (the
  worksheet a human or another tool files from).
- **MeF XML:** `--type mef --out <file.xml>` — MeF-shaped XML
  (TY2025v5.2 form documents — IRS1040 and the schedules the
  engine computes — in a Return/ReturnHeader/ReturnData envelope;
  whole-dollar amounts; validated against the official XSDs by
  scripts/mef_acceptance.py). It requires a stored identity
  (`identity_required` otherwise) and a complete return; the full
  SSN appears inside the XML (it is the filing artifact) but
  never in its stdout report. It is NOT transmissible — no e-file
  credentials exist in this tool; say so plainly if asked whether
  it can be filed. Resident route only: NR returns get
  `not_implemented`. See docs/KNOWN_LIMITS.md (Filing).

## Closing summary

Present: refund/owed line values (34/37) quoted from engine
output, completeness status, open findings, and the exported
file path. With a state block, add the California refund/owed
(state lines 97/100) quoted from the state section. Then offer
tax-reviewer (independent check) and tax-filing-guide (how
filing works) as separate next steps. Preparation is not filing:
this skill never submits.

## Iron rules

The router's iron rules apply in full. Rule 1 is the job: every
number you present comes from engine output in this session —
not from memory, not from your own calculation, not from a prior
draft.
