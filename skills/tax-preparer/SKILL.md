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
   stored dependents — names and credit checkboxes for the first four;
   identity fields and dependent SSNs are reported as skipped, never
   invented). MeF export returns `not_implemented` in
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
credits (see `docs/KNOWN_LIMITS.md`). Line 16 is
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
lines 28/32 feeding total payments on line 33. Preparation is not
filing: this skill never submits.
