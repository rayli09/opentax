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
   (amount lines + filing status only; identity fields are reported as
   skipped, never invented). MeF export returns `not_implemented` in
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

v1 computes a TY2025 Form 1040 slice from W-2, 1099-INT, 1099-DIV, and
1099-B entries (see `docs/KNOWN_LIMITS.md`). Line 16 is bracket tax, not
the Tax Table / QDCG worksheet — say so when presenting it. Preparation
is not filing: this skill never submits.
