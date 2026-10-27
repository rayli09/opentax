---
name: tax-ingest
description: Document ingest skill for opentax — the only skill that touches raw tax documents. Use to extract W-2/1099 PDFs deterministically with `tax doc extract`, review the per-field evidence, and enter them via `tax form add`; falls back to supervised vision transcription for scanned or unrecognized documents.
---

# tax-ingest — documents → entered facts

**Privacy first:** before opening any document, tell the user what you are
about to read and that processing is local. This is the only skill that
touches raw documents; treat every file as sensitive.

## Workflow

1. **Extract first** (born-digital PDFs): `tax doc extract --file <path>`.
   It handles W-2, 1099-INT, 1099-DIV, and 1099-NEC, and returns:
   - `status`: `extracted` | `partial` | `needs_ocr` | `unrecognized`
   - `node`: a payload shaped exactly like a `form add` entry, plus a
     `"type"` key naming the `--node-type` to use. You may pass the node
     to `form add` unchanged (the extra `"type"` key is ignored there).
   - `fields` / `evidence`: every value with its page, matched label,
     raw printed token, and box number.
   - `checks`: deterministic cross-checks (e.g. W-2 Social
     Security/Medicare rate checks) over the extracted values.
   - `warnings`: copies detected, empty boxes, refused values — read
     them all; they are part of the result, not noise.
   Branch on `status`:
   - `extracted` → review the evidence (step 3), then enter the node
     (step 4).
   - `partial` → some boxes were not located or were blank. Review the
     evidence and warnings, look at the document yourself for the
     missing fields only, and transcribe those by hand (step 5 rules).
     Never let the extractor's silence turn into a zero: a required
     field it omitted stays absent until a human or your verified
     transcription supplies it.
   - `needs_ocr` → the PDF is scanned/image-only; v1 ships no OCR. Use
     the vision fallback (step 5) and say so to the user.
   - `unrecognized` → not an extractable type (this includes 1099-B
     statements, which are deliberately never extracted — layouts vary
     too much). Use `tax doc classify --file <path>` to identify it,
     then the vision fallback (step 5). If the type is still unsure,
     stop for that document — do not guess.
2. Get the entry contract: `tax node template --document <type>` and
   `tax schema` — the node's fields must match exactly those templates.
3. **Review evidence before entering.** For each field, the evidence
   `raw` token is what the document printed; the entered value must be
   that token in integer cents, verbatim — transcription is not
   computation, copy what is printed even if it looks wrong. Spot-check
   at least the largest amount per document against the document image
   or text. If a `checks` finding flags a value, re-read the document;
   if the printed value stands, keep it and keep the finding visible —
   never silently "fix" a number to make a finding go away.
4. Enter each document:
   `tax form add --return <id> --node-type <type> --file <entry.json>`
   (the entry JSON is the extract `node`, or your hand-built payload on
   the fallback path). Then run `tax return validate --return <id>`
   and report every finding to the user with its source document.
   - **1099-NEC exception:** the entry's `expenses` field is NOT printed
     on the 1099-NEC, and the extractor never supplies it. Ask the user
     for that engagement's total Schedule C expenses from their records
     and record the source as "user-provided records" — never estimate,
     itemize, or invent it. No expenses reported means the user states
     there were none.
   - **W-2 note:** the node carries `employer_name` for your eyeball
     check against the document; the W-2 payload has no such field and
     `form add` ignores it.
5. **Vision fallback** (scanned, `needs_ocr`, `unrecognized`, or fields
   the extractor missed): transcribe every figure **verbatim** from the
   document image, in integer cents, and record the source of each
   (document name, page, box number) in your working notes. Mark every
   vision-transcribed figure for human confirmation in the review step,
   and tell the user which figures came from vision rather than from
   `doc extract`.

## Iron rules

The router's six iron rules apply in full. In this skill rule 2 is the
job: verbatim values, source recorded, no rounding, no inference (e.g.
never compute Box 3 from Box 1; if a box is absent, the field is absent,
not zero — the only zeros allowed are the ones `doc extract` emits for
a located, verifiably empty box on a field whose payload defaults to
zero, with the empty-box evidence to prove it).

## Authority limits

The extractor never reads the employee/recipient SSN boxes and never
emits an SSN-shaped number; if you transcribe by vision, apply the same
rule — SSNs are not entry data. Scanned/photographed documents: v1
ships no OCR; your vision is the transcription source, say so, and mark
the figures for human confirmation.
