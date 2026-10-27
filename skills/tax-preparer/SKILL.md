---
name: tax-preparer
description: Preparation skill for opentax. Use when facts/documents are entered and the return should be computed, presented, validated, explained, and exported as JSON.
---

# tax-preparer — compute, present, validate, export

## Workflow

1. Confirm setup: `tax doctor`, `tax selftest` (router rules).
2. Show the computed return: `tax return get --return <id>`.
   - Quote lines verbatim from this output. Lines marked
     `"state": "incomplete"` are **missing facts, not zeros** — list them
     for the user and go back to tax-intake/tax-ingest for what's missing;
     never present a total as final while the return is incomplete.
3. Explain any line the user questions with
   `tax explain --return <id> --line <n>` and quote its derivation and
   citation. Do not improvise an explanation the CLI did not produce.
4. Validate: `tax return validate --return <id>`. Surface **all**
   findings, grouped by severity. Errors block export-as-final.
5. Export the artifact: `tax return export --return <id> --type json`,
   or a filled Form 1040 PDF:
   `tax return export --return <id> --type pdf --template <blank TY2025 f1040.pdf> --out <file>`
   (amount lines, filing status, and the page-1 Dependents table for
   stored dependents — names, stored SSNs, and credit checkboxes for
   the first four; when an identity block is stored via
   `tax return set-identity`, also the header names/SSNs/address, the
   page-2 occupations, and the standard-deduction age/blind/claim
   checkboxes. Identity pieces that are not stored are reported as
   skipped, never invented, and SSNs in the export report are masked —
   the full SSN appears only inside the filled PDF). MeF export returns `not_implemented` in
   v1 — report that plainly if asked; do not claim a file exists when
   none was produced.
6. Present a closing summary: refund/owed line values (34/37) quoted from
   engine output, completeness status, open findings, and the exported
   file path. Then offer tax-reviewer (independent check) and
   tax-filing-guide (how filing works) as separate next steps.

## Iron rules

The router's six iron rules apply in full. Rule 1 is the job: every number
you present comes from engine output in this session — not from memory,
not from your own calculation, not from a prior draft.

## Authority limits

v1 computes a TY2025 Form 1040 slice from W-2, 1099-INT, 1099-DIV,
1099-B, and 1099-NEC entries, plus stored dependents with the child
credits and the earned income credit (see `docs/KNOWN_LIMITS.md`).
Line 16 is
figured by the prescribed method — the QDCG worksheet, the Tax Table,
or the Tax Computation Worksheet — and the engine reports which one as
`line16_method`; name the method when presenting the tax. When 1099-NEC
entries are present, present the side-income chain explicitly: Schedule
C net profit (internal key `schc_net_profit`) on line 8, self-employment
tax on line 23, its deductible half on line 10, and total tax on
line 24 (= line 22 + line 23) — the amount owed/refunded (34/37) is
figured against line 24, not line 16. When dependents are present,
present the credit chain explicitly: tentative credit (internal key
`sched8812_tentative`), the nonrefundable part on lines 19/21, tax
after credits on line 22, and any additional child tax credit on
line 28. Present the earned income credit chain on every return, not
just ones with dependents: the EIC qualifying-child count (internal
key `eic_qualifying_children` — a count, shown as count × $1.00), the
EIC earned income (internal key `eic_earned_income` — wages plus
Schedule SE line 3 net earnings minus the deductible part of SE tax;
note this differs from the Schedule 8812 earned-income key, which
does not subtract SE tax), the investment-income figure (internal
key `eic_investment_income`; over $11,950 in TY2025 the credit is
zero by IRC §32(i)), and line 27 itself. Line 32 = line 27 + line 28
feeds total payments on line 33. When line 27 is zero, say which
reason the engine reports (its compute notes name it: MFS, investment
income over the limit, no positive earned income, phaseout complete,
the childless age test unmet, or the childless age test
undeterminable because no date of birth is stored) — and surface the
matching validate findings (`eic_mfs`, `eic_childless_age_unknown`,
`eic_investment_income_limit`, `dependent_eic_ssn`) rather than
letting a zero pass silently. Preparation is not
filing: this skill never submits.
