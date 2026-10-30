---
name: tax-ingest
description: Document ingest details for opentax stage S2 of THE RETURN PROCESS (skills/tax) — the only skill that touches raw tax documents. Use to extract W-2/1099 PDFs deterministically with `tax doc extract`, review the per-field evidence, and enter them via `tax form add` or `extract --add-to`; falls back to supervised vision transcription for scanned or unrecognized documents.
---

# tax-ingest — S2 details: documents → entered facts

The process lives in the router (skills/tax, stage S2). This skill
holds the per-document details: extraction evidence rules, the
branching on extract status, and the vision fallback.

**Privacy first:** before opening any document, tell the user what
you are about to read and that processing is local. This is the
only skill that touches raw documents; treat every file as
sensitive.

## The two entry paths

1. **One-step (default for clean born-digital PDFs):**
   `tax doc extract --file <path> --add-to <id>` extracts and
   enters in one step. The result still carries the full
   `fields`/`evidence` and `checks` — **review them after the
   fact** (below), and report every finding to the user. If a
   check flags a value, the entered value stays as printed
   (verbatim rule); the finding stays visible via
   `tax return validate`.
2. **Review-first:** `tax doc extract --file <path>` returns the
   node without entering it; review the evidence, then enter it
   unchanged with
   `tax form add --return <id> --node-type <type> --file <entry.json>`
   (or `--data` / `--file -`). Use this path whenever the extract
   is `partial`, the document is unusual, or the user wants to see
   values before they land.

Whichever path: branch on the extract `status` first:

- `extracted` → review the evidence, enter.
- `partial` → some boxes were not located or were blank. Review
  the evidence and warnings, look at the document yourself for the
  missing fields only, and transcribe those by hand (vision rules
  below). Never let the extractor's silence turn into a zero: a
  required field it omitted stays absent until a human or your
  verified transcription supplies it.
- `needs_ocr` → the PDF is scanned/image-only; v1 ships no OCR.
  Use the vision fallback and say so to the user.
- `unrecognized` → not an extractable type (this includes 1099-B
  statements, which are deliberately never extracted — layouts
  vary too much). Use `tax doc classify --file <path>` to identify
  it, then the vision fallback. If the type is still unsure, stop
  for that document — do not guess.

## What extract returns (review all of it)

- `node`: a payload shaped exactly like a `form add` entry, plus a
  `"type"` key naming the `--node-type` to use. Passed to
  `form add` it may go unchanged (the extra `"type"` key is
  ignored there).
- `fields` / `evidence`: every value with its page, matched label,
  raw printed token, and box number.
- `checks`: deterministic cross-checks (e.g. W-2 Social
  Security/Medicare rate checks) over the extracted values.
- `warnings`: copies detected, empty boxes, refused values — read
  them all; they are part of the result, not noise.

The entry contract is the CLI's: `tax node template --document <type>`
prints the exact shape `form add` accepts; `tax schema` describes
the rest. Never build an entry from a remembered shape.

## Review evidence before trusting an entry

For each field, the evidence `raw` token is what the document
printed; the entered value must be that token in integer cents,
verbatim — transcription is not computation; copy what is printed
even if it looks wrong. Spot-check at least the largest amount per
document against the document image or text. If a `checks` finding
flags a value, re-read the document; if the printed value stands,
keep it and keep the finding visible — never silently "fix" a
number to make a finding go away. After entering, run
`tax return validate --return <id>` and report every finding to
the user with its source document.

## Per-document notes

- **1099-NEC:** the entry's `expenses` field is NOT printed on the
  1099-NEC, and the extractor never supplies it. Ask the user for
  that engagement's total Schedule C expenses from their records
  and record the source as "user-provided records" — never
  estimate, itemize, or invent it. No expenses reported means the
  user states there were none. On a CA return, Box 5 (state tax
  withheld) also needs transcribing — explicit `"0"` when the form
  reports none (see tax-intake, CA branch).
- **W-2:** the node carries `employer_name` for your eyeball check
  against the document; the W-2 payload has no such field and
  `form add` ignores it. On a CA return, Boxes 16/17 join the
  entry the same way Box 5 does for NEC.
- **1099-B:** never extracted. Vision-transcribe **per
  transaction** (description, dates, proceeds, basis, and any
  adjustment codes/amounts printed) into `1099-b` entries; the
  statement's own category totals are your cross-check, not an
  entry.
- **1042-S (NR route):** not extracted in v1 — vision-transcribe
  into an `f1042s` entry (shape:
  `tax node template --document f1042s`). Income code 20 = wages
  during study/training, code 15 = scholarship/fellowship grants;
  transcribe the exemption code and treaty article fields exactly
  as printed — they decide whether income is treaty-exempt.
- **1099-INT on the NR route:** bank deposit interest is exempt
  for nonresident aliens; if the filer hands you one anyway, enter
  it and let `validate` surface the `nra_bank_interest_exempt`
  finding rather than deciding yourself.

## Vision fallback

(Scanned, `needs_ocr`, `unrecognized`, or fields the extractor
missed.) Transcribe every figure **verbatim** from the document
image, in integer cents, and record the source of each (document
name, page, box number) in your working notes. Mark every
vision-transcribed figure for human confirmation in the review
step, and tell the user which figures came from vision rather than
from `doc extract`.

## Iron rules for this stage

The router's iron rules apply in full. Here rule 2 is the job:
verbatim values, source recorded, no rounding, no inference (e.g.
never compute Box 3 from Box 1; if a box is absent, the field is
absent, not zero — the only zeros allowed are the ones `doc
extract` emits for a located, verifiably empty box on a field
whose payload defaults to zero, with the empty-box evidence to
prove it). The extractor never reads the employee/recipient SSN
boxes and never emits an SSN-shaped number; if you transcribe by
vision, apply the same rule — SSNs are not entry data.
