---
name: tax-reviewer
description: Independent review details for opentax stage S4 of THE RETURN PROCESS (skills/tax). Use to check a prepared return against its source documents, run the cross-checks, track the four readiness statuses separately, and report discrepancies without overwriting the prepared return.
---

# tax-reviewer — S4 details: the independent check

The process lives in the router (skills/tax, stage S4). You did
not prepare this return (even if you did — review as if you
didn't). Recompute nothing yourself; re-derive everything from
sources and engine output.

## The four statuses (track and report them separately)

1. **Documents available** — every entered entry traces to a
   source document.
2. **Calculation clean** — `tax selftest` passes and
   `tax return get` shows no incomplete lines that anyone is
   treating as final.
3. **Human review done** — the user has seen the findings and
   the summary.
4. **Filing evidenced** — only the user can make this true (an
   IRS/state acceptance or a filed paper copy). Validation,
   export, or this review are **not** filing. Never report
   status 4 from CLI output.

## Workflow

1. `tax return get --return <id>` — list every line, flag
   incompletes. (`tax return status` shows which process steps
   are still open.)
2. For each stored entry, compare the entered figures against
   the source document figure by figure (box numbers, verbatim).
   Any mismatch is a discrepancy: report it, with both values and
   both sources. Do not edit the prepared return from this skill.
3. `tax explain --return <id> --line <n>` for the lines that
   drive the result (9, 11, 15, 16, 34/37) — check each
   derivation against its citation. (CA return: the state
   section's 17/19/31/64/97/100 with `--state CA`. NR return:
   the 1040-NR's own driving lines.)
4. `tax return validate --return <id>` — reconcile its findings
   with your own; anything either of you found stays on the
   list.
5. Run the cross-checks (below).
6. Check for the classic misses *within v1 scope*: a W-2 not
   entered (compare against the intake checklist), withholding
   not matching Box 2 totals, filing status inconsistent with
   intake.
7. Report: four statuses, discrepancies, open findings, and a
   clear "ready / not ready for the user's own filing step"
   conclusion.

## Cross-checks (quote engine output; never compute)

- **W-2 statutory rates (resident route).** The extractor's
  `checks` and `validate` verify Social Security tax ≈ 6.2% of
  Social Security wages (up to the wage base) and Medicare tax ≈
  1.45% of Medicare wages. A flagged W-2 means re-read the
  document — if the printed value stands, it stays, and the
  finding stays visible. **NR route:** zero FICA is the *expected*
  state and these rate checks are suppressed; the live check is
  the reverse — `fica_withheld_while_exempt` means tax was
  withheld during exempt years and the filer's remedy is a
  refund claim (employer first, then Forms 843/8316), not a
  return edit.
- **Withholding sanity.** Total federal withholding on the
  return (line 33 via 25d, plus 1042-S withholding on line 25g
  for NR) must trace to the sum of the entered documents' Box 2
  / 1042-S withholding figures — compare against the entries,
  figure by figure, per workflow step 2.
- **Refund/owed plausibility.** The refund (34) can never exceed
  total payments (33); amount owed (37) and refund (34) are
  never both nonzero; the state section obeys the same shape
  (78 → 97/100). If any of these shapes looks wrong in `return
  get`, stop — that is an engine bug report, not a number to
  work around.
- **Route sanity.** The form matches the S1 route: a resident
  return shows Form 1040 lines; an NR return shows
  `"form": "1040-NR"` and no resident lines; a CA section exists
  only on a resident return with the state block stored.

## Handling findings

Never silence a finding by editing a number, and never edit the
prepared return from this skill — corrections go back through
tax-ingest/tax-preparer so the audit trail stays clean. Some
findings are not return defects at all: the FICA-exempt finding
(above) is a claim the *filer* pursues outside the return;
`yctc_zero_earned_income_path` and
`ca_nec_classification_not_modeled` mark v1 scope boundaries —
report them as known limits, not errors to fix.

## Iron rules

The router's iron rules apply in full, plus the one above: the
reviewer never overwrites the prepared return.
