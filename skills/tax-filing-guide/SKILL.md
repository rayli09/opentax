---
name: tax-filing-guide
description: Filing guidance skill for opentax — the only skill allowed near the filing step. Use to explain filing channels and walk the user to the point where THEY submit. Never handles credentials or transmits anything.
---

# tax-filing-guide — the last mile, user-driven

v1 of opentax **does not e-file and does not transmit anything**. MeF
transmission requires a firm-level EFIN/ETIN, suitability checks, and
annual IRS assurance testing; credentials are non-transferable and an
open-source CLI cannot ship one. This skill exists so that boundary is
explained honestly and the user still gets a working path to filed.

## Hard stops

- **You never submit, transmit, sign, or pay.** The user alone performs
  submit / e-file / pay actions, on the filing site, themselves.
- **You never see or handle credentials**: no IRS account logins, EFINs,
  PINs, or bank logins. If a step needs one, describe the step and let the
  user do it.
- If asked to "just file it for me", decline that step and offer the
  walkthrough below instead.

## Workflow

1. Confirm the return is ready per tax-reviewer: documents available,
   calculation clean, human review done. Filing an incomplete or
   unreviewed return is a stop, not a shortcut.
2. Present the channels factually:
   - **IRS Free File** (income-eligible): guided commercial software,
     free, files electronically under the partner's authorization.
   - **Free File Fillable Forms**: no income limit, federal only, no
     guidance or state return — the user keys the computed figures in
     themselves. The JSON export from tax-preparer is their worksheet.
   - **Paper**: print the completed forms, mail per IRS instructions for
     the user's state; keep a copy and mailing evidence.
   - A paid preparer / commercial software remains a valid choice; the
     exported JSON and line summary make that handoff quick.
3. Whichever channel: the user enters/submits; you may read back figures
   from `tax return get` to help them key accurately, quoting engine
   output only.
4. After the user confirms submission, record *their* confirmation (date,
   channel, confirmation/receipt details they provide) in the summary.
   That user report is the only evidence of filing — mark reviewer
   status 4 from it, never from anything the CLI produced.

## Iron rules

The router's six iron rules apply in full; rules 4 and 5 are the job here.
