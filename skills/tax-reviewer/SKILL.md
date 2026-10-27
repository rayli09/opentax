---
name: tax-reviewer
description: Independent review skill for opentax. Use to check a prepared return against its source documents, track the four readiness statuses separately, and report discrepancies without overwriting the prepared return.
---

# tax-reviewer — independent check

You did not prepare this return (even if you did — review as if you
didn't). Recompute nothing yourself; re-derive everything from sources
and engine output.

## The four statuses (track and report them separately)

1. **Documents available** — every entered entry traces to a source document.
2. **Calculation clean** — `tax selftest` passes and `tax return get`
   shows no incomplete lines that anyone is treating as final.
3. **Human review done** — the user has seen the findings and the summary.
4. **Filing evidenced** — only the user can make this true (an IRS/state
   acceptance or a filed paper copy). Validation, export, or this review
   are **not** filing. Never report status 4 from CLI output.

## Workflow

1. `tax return get --return <id>` — list every line, flag incompletes.
2. For each stored entry, compare the entered figures against the source
   document figure by figure (box numbers, verbatim). Any mismatch is a
   discrepancy: report it, with both values and both sources. Do not edit
   the prepared return from this skill.
3. `tax explain --return <id> --line <n>` for the lines that drive the
   result (9, 11, 15, 16, 34/37) — check each derivation against its
   citation.
4. `tax return validate --return <id>` — reconcile its findings with your
   own; anything either of you found stays on the list.
5. Check for the classic misses *within v1 scope*: a W-2 not entered
   (compare against the intake checklist), withholding not matching
   Box 2 totals, filing status inconsistent with intake.
6. Report: four statuses, discrepancies, open findings, and a clear
   "ready / not ready for the user's own filing step" conclusion.

## Iron rules

The router's six iron rules apply in full, plus: never overwrite the
prepared return from the reviewer skill — corrections go back through
tax-ingest/tax-preparer so the audit trail stays clean.
