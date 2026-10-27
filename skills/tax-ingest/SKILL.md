---
name: tax-ingest
description: Document ingest skill for opentax — the only skill that touches raw tax documents. Use to classify W-2/1099 PDFs or text, transcribe their figures verbatim with sources, and enter them via `tax form add`.
---

# tax-ingest — documents → entered facts

**Privacy first:** before opening any document, tell the user what you are
about to read and that processing is local. This is the only skill that
touches raw documents; treat every file as sensitive.

## Workflow

1. Classify each file: `tax doc classify --file <path>` (.pdf or .txt).
   - If it returns `"status": "unknown"`, do not guess the type. Use your
     own vision on the document image if available and ask the user to
     confirm the type; if still unsure, stop for that document.
2. Get the entry contract: `tax node template --document <type>` and
   `tax schema` — fill exactly those fields, no others.
3. Transcribe every figure **verbatim** from the document, in integer
   cents, and record the source of each (document name, page, box number)
   in your working notes. Transcription is not computation: copy what is
   printed, even if it looks wrong.
4. Enter each document:
   `tax form add --return <id> --node-type w2 --file <entry.json>`
   (v1 computes W-2 entries; 1099 templates exist but entering them
   returns `not_implemented` — tell the user, don't work around it.)
5. Run `tax return validate --return <id>`. The deterministic checks
   (e.g. W-2 Social Security/Medicare rate cross-checks) will flag
   transcription errors and genuinely odd documents alike. Report every
   finding to the user with its source document; never silently "fix" a
   number to make a finding go away — re-read the document, and if the
   printed value stands, keep it and keep the finding visible.

## Iron rules

The router's six iron rules apply in full. In this skill rule 2 is the
job: verbatim transcription, source recorded, no rounding, no inference
(e.g. never compute Box 3 from Box 1; if a box is absent, the field is
absent, not zero).

## Authority limits

Scanned/photographed documents: v1 ships no OCR. Use your vision as the
transcription source, say so, and mark every vision-transcribed figure for
human confirmation in the review step.
