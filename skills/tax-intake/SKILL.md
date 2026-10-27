---
name: tax-intake
description: Interview skill for opentax. Use to start a return and discover what facts and documents are needed, driving the conversation from the engine's incompleteness — never from a generic checklist alone.
---

# tax-intake — interview

Goal: end with a created return (`tax return create`) and a confirmed list
of (a) facts to provide and (b) documents to gather. No computation here.

## Workflow

1. Run the router setup (`tax doctor`, `tax selftest`).
2. Establish the basics with the user: tax year (v1: 2025 only), filing
   status (single / mfj / hoh / mfs), and income sources present this year
   (employment W-2s, bank interest, dividends, brokerage, other).
3. Create the return:
   `tax return create --year 2025 --status <status>`
   Record the returned `returnId` and keep using exactly that id.
4. If the user can state interest and dividend totals now, re-create is not
   needed later only if they were passed at create time
   (`--interest-cents`, `--dividends-cents`, integer cents). Otherwise note
   them as outstanding — the return will honestly show dependent lines as
   incomplete until they exist as documents or recorded facts.
5. Hand the user a document checklist derived from their income sources:
   one W-2 per job; 1099-INT per bank; 1099-DIV per fund/broker;
   1099-B for brokerage sales; 1099-NEC per side-income payer **plus
   their own expense records for each engagement** (the expenses total
   is an entered fact, not a form box); prior-year return if available.
6. Confirm a short facts summary back to the user and stop. Next step is
   tax-ingest (documents) or tax-preparer (facts already entered).

## Iron rules

The router's six iron rules apply in full (no arithmetic, verbatim
transcription, validator before trust, no credentials, out-of-scope stops,
privacy warning before reading documents).

## Authority limits

Scope is TY2025 Form 1040 with W-2 wages, interest/dividends/capital
gains from 1099s, and 1099-NEC side income (simplified Schedule C/SE)
at the engine level. If the user's situation needs anything in
`docs/KNOWN_LIMITS.md` (Schedule E, credits, state returns…), say so
now — before collecting documents — and stop that part.
