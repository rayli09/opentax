---
name: tax-filing-guide
description: Filing guidance details for opentax stage S5 of THE RETURN PROCESS (skills/tax) — the only skill allowed near the filing step. Use to explain filing channels and walk the user to the point where THEY submit. Never handles credentials or transmits anything.
---

# tax-filing-guide — S5 details: the last mile, user-driven

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
   - **Paper**: print the completed forms (the packet export below),
     mail per IRS instructions for the user's state; keep a copy and
     mailing evidence.
   - A paid preparer / commercial software remains a valid choice; the
     exported JSON and line summary make that handoff quick.
   - The MeF XML export (`tax return export --type mef`) is **not a
     channel**: it produces a standards-shaped artifact (TY2025v5.2
     form documents) that a credentialed transmitter's software could
     import or a reviewer could inspect. It cannot be uploaded to the
     IRS by the user, and the export itself says so — never present
     it as something the user can submit.
3. Whichever channel: the user enters/submits; you may read back figures
   from `tax return get` to help them key accurately, quoting engine
   output only.
4. After the user confirms submission, record *their* confirmation (date,
   channel, confirmation/receipt details they provide) in the summary.
   That user report is the only evidence of filing — mark reviewer
   status 4 from it, never from anything the CLI produced.

## The paper packet: what you mail, in what order

For the paper channel, the deliverable is the packet export
(`tax return export --return <id> --type packet --out-dir <dir>`;
export mechanics in tax-preparer). Check its `manifest.json`
first: every form must be `filled` or `not_applicable`. A
`missing_template` entry (recovery: `tax forms fetch`, then
re-export), a Form 8949 overflow note, or an unchecked
Schedule C line 32 note means the packet is **not yet complete for
filing** — say so and stop; do not present it as ready to mail.

What goes in the envelope, front to back (the Form 1040 instructions'
assembly order):

1. Payment voucher / check if an amount is owed (per the 1040
   instructions — never cash).
2. Form 1040, with Copy B of each W-2 and any 1099 showing federal
   withholding attached to its front, as the 1040 instructions direct.
3. The schedules, behind the 1040, in **Attachment Sequence Number**
   order — the number printed at the top right of each form:
   Schedule 1-A (1A), Schedule 1 (01), Schedule 2 (02), Schedule C
   (09; one per 1099-NEC engagement, in packet order), Schedule D
   (12), Form 8949 (12A), Schedule SE (17), Schedule 8812 (47).
   Only the forms the manifest marks `filled` are included.
4. Sign and date the 1040 (both spouses on a joint return) — the user
   signs, always; an unsigned return is not filed.

Mail to the IRS service center for the user's state per the 1040
instructions, keep a full copy plus mailing evidence (certified mail
receipt or equivalent), and record the mailing as the user's
confirmation under step 4 above. The CLI produces the forms only; it
never mails, submits, or pays.

## California: the Form 540 is a separate filing

When the return has a California state block (`tax return set-state`),
the same packet export also produces a filled **Form 540** after the
federal forms. California is filed **separately from the federal
return** — its own envelope, its own address, its own payment. Never
put the 540 in the IRS envelope or the 1040 in the FTB envelope.

Manifest expectations for the CA entries:

- `f540` must be `filled`. A `missing_template` f540 (the blank
  `f540.pdf` was not in the forms dir) means the CA packet is not
  ready — say so and stop, same as a federal missing template.
- `f540ca` (Schedule CA) is **`not_applicable`, and that is correct**,
  whenever the CA computation is in v1 scope: Schedule CA is required
  only when there are California adjustments, and the CA engine
  states the Schedule CA Part I totals (Form 540 lines 14/16) as zero
  for every in-scope return — California conforms to the federal
  treatment of all v1 facts, including the deductible part of
  self-employment tax. Do not present the missing Schedule CA as a
  gap, and do not file a zero Schedule CA.

Before mailing, the filer completes by hand the fields the packet
intentionally leaves blank on the 540 (the manifest notes list them):
the county at time of filing; **line 91 use tax — the form says "Do
not leave blank"**, so enter -0- and check the applicable box ("No use
tax is owed" or "paid directly to CDTFA"); the line 92 full-year
health-coverage box if it applies; lines 98/99/115 if they want part
of the overpayment applied to next year's estimates or a direct
deposit (bank details are never stored, so the routing/account
fields are always blank in the packet); and the signature/date block
(both spouses sign a joint return — the user signs, always).

Assemble the CA envelope: Form 540, with the state copies (Copy 2) of
each W-2 and any 1099 showing California withholding attached as the
540 instructions direct; a check, if an amount is owed, payable to
"Franchise Tax Board" with the filer's SSN and "2025 Form 540" on it
(never cash). Mail to the Franchise Tax Board (2025 Form 540 booklet,
"Mailing Your Tax Return"; the same two addresses are printed on the
540 itself, Side 5):

- **Refund or no amount due:** Franchise Tax Board,
  PO Box 942840, Sacramento, CA 94240-0001
- **Payment enclosed:** Franchise Tax Board,
  PO Box 942867, Sacramento, CA 94267-0001

Keep a full copy plus mailing evidence, and record the mailing as the
user's confirmation under step 4 above.

E-file note: the FTB runs its own free e-file, **CalFile**, for
eligible filers, and commercial software can e-file the 540 as well —
either is a valid channel, with the user submitting themselves as
always. This tool does not e-file California: it produces the paper
Form 540 only, and there is no CA equivalent of the federal MeF XML
export in v1 (California's e-file XML is out of scope).

## Nonresident aliens: the Form 1040-NR packet is its own filing

When the return carries an `nra` block (`tax return set-nra`), the
packet export produces a different packet: **Form 1040-NR**,
**Schedule OI (Form 1040-NR)** when it has something to report, and
**Form 8843** — no resident Form 1040, no federal schedules, no
California forms (the manifest's `route` entry says exactly this;
California does not honor federal tax treaties, and Form 540NR is
not implemented — a California-resident NRA still owes the state a
return this tool cannot produce; say so plainly). `--type pdf` fills
the 1040-NR alone; `--type mef` is **not implemented** for NR
returns (the 1040-NR MeF schema package is separate) and the CLI
says so — never present the resident MeF export as usable for an NR
filer.

Manifest expectations for the NR entries:

- `f1040nr` must be `filled`; `f8843` must be `filled` (an exempt
  individual files it every year, even with no income); `f1040soi`
  is `filled` when the return claims a treaty exemption or carries
  days-of-presence facts, and `not_applicable` — correctly — when it
  has neither.
- A `missing_template` entry (the blank `f1040nr.pdf` /
  `f1040soi.pdf` / `f8843.pdf` was not in the forms dir) means the
  packet is not ready — say so and stop.
- Read the manifest notes before presenting the packet: they list
  everything left blank for hand-completion — the 1040-NR foreign
  address block and Digital Assets answer; Schedule OI items B–M
  whose answers are not stored facts; Form 8843's passport lines,
  the exact date of entry on line 1a, and lines 13/14. The 8843's
  two address fields are blank **by the form's own instruction**
  (they are filled only when the 8843 is mailed by itself, without
  a return). An incomplete NR return never exports: the CLI refuses
  and names the incomplete Form 1040-NR lines.

Assembly and mailing (Instructions for Form 1040-NR (2025), "When
and Where Should You File?" and "Where To File"; Form 8843
instructions, "When and Where To File"):

1. Sign and date the 1040-NR (the packet leaves the signature block
   blank — the user signs, always). Form 8843 **attaches to** the
   1040-NR; attached, it needs no separate signature.
2. Attach Copy B of each Form W-2 and the copies of each Form
   1042-S to the 1040-NR, as the instructions direct (the 1042-S
   copies go to the left margin of page 1 when claiming a refund of
   tax withheld at source).
3. Order in the envelope: Form 1040-NR first, then Schedule OI,
   then Form 8843.
4. **Due dates** (calendar-year filers): if the filer received wages
   subject to U.S. income tax withholding — every W-2 case in this
   tool's scope — the return is due **April 15** (April 15, 2026 for
   tax year 2025). With no such wages, it is due **June 15** (June
   15, 2026). Form 8843 follows the same deadline.
5. **Where to mail** — one pair of addresses for all 1040-NR filers:
   - *No payment enclosed:* Department of the Treasury, Internal
     Revenue Service, Austin, TX 73301-0215, USA.
   - *Payment enclosed:* Internal Revenue Service, P.O. Box 1303,
     Charlotte, NC 28201-1303, USA (check payable to "United States
     Treasury", with the filer's SSN, tax year, and "Form 1040-NR"
     on it — never cash).
6. **Form 8843 by itself** (exempt individual with no return to
   file): mail it alone to Department of the Treasury, Internal
   Revenue Service Center, Austin, TX 73301-0215 by the 1040-NR due
   date — in that case the filer *does* fill in the 8843's address
   fields, which the packet leaves blank for attached filing.

Keep a full copy plus mailing evidence, and record the mailing as
the user's confirmation under step 4 above, as always.

## Iron rules

The router's seven iron rules apply in full; rules 4 and 5 are the job here.
