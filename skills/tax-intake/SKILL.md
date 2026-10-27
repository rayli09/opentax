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
   status (single / mfj / hoh / mfs), income sources present this year
   (employment W-2s, bank interest, dividends, brokerage, other), and
   **dependents**: for each person the user claims, ask their
   relationship (child / stepchild / foster child / other), their age
   on December 31 of the tax year, how many months they lived with
   the taxpayer — these drive the child tax credit / credit for other
   dependents (simplified Schedule 8812) and the earned income
   credit — and their **SSN**: a dependent's SSN is stored with the
   entry and prints in the Dependents table; the child tax credit
   legally requires the child to have one (the engine warns, but
   still computes, when it is missing), and the **earned income
   credit does not count a child without one at all** — collect it
   whenever the user has it. For each child also ask the two EIC
   facts: were they a **full-time student** (during some part of each
   of 5 calendar months — `full_time_student`), and are they
   **permanently and totally disabled** (`permanently_disabled`)?
   These decide whether a child who is 19 or older can still be an
   EIC qualifying child.
3. Create the return:
   `tax return create --year 2025 --status <status>`
   Record the returned `returnId` and keep using exactly that id.
4. Collect the **taxpayer identity** and store it with
   `tax return set-identity --return <id> --file identity.json`:
   first/last name, SSN (dashed or 9 digits), date of birth,
   occupation, mailing address (street, apt, city, state, ZIP), and
   whether the taxpayer is blind; for mfj/mfs also the spouse's same
   facts. Also ask whether someone can claim the taxpayer as a
   dependent (`can_be_claimed_as_dependent` — it fills the form's
   checkbox; the dependent-filer deduction limit itself is not
   computed in v1). The dates of birth and blindness drive the
   age-65/blind standard-deduction additions, so ask them explicitly
   — "were you (or your spouse) born before January 2, 1961, or
   blind?" is the fact behind them. The same dates of birth (with a
   stored SSN) also drive the **Schedule 1-A Part V enhanced
   deduction for seniors** (line 13b: $6,000 per person born before
   January 2, 1961, phased out over $75,000 / $150,000 MFJ of MAGI;
   married filing separately takes none) — with no date of birth
   stored, the engine takes no senior deduction (validate reports
   `senior_bonus_age_unknown`). The taxpayer's (and spouse's)
   dates of birth also decide the **no-children earned income
   credit** (age 25–64 at year-end): with no dependents and no date
   of birth stored, the engine cannot determine it and takes no
   childless EIC (validate reports `eic_childless_age_unknown`).
   Identity is stored locally only
   and every CLI output shows SSNs masked; say so once, plainly.
5. If the user can state interest and dividend totals now, pass them at
   create time (`--interest-cents`, `--dividends-cents`, integer cents).
   If the return already exists without them, record the amounts with
   `tax return set-other-income --return <id> --interest-cents <n>
   --dividends-cents <n>` — the return cannot complete until they are
   recorded, even as 0. Otherwise note them as outstanding — the return
   will honestly show dependent lines as incomplete until they exist as
   documents or recorded facts.
6. Hand the user a document checklist derived from their income sources:
   one W-2 per job; 1099-INT per bank; 1099-DIV per fund/broker;
   1099-B for brokerage sales; 1099-NEC per side-income payer **plus
   their own expense records for each engagement** (the expenses total
   is an entered fact, not a form box); prior-year return if available.
7. Confirm a short facts summary back to the user and stop. Next step is
   tax-ingest (documents) or tax-preparer (facts already entered).

## Iron rules

The router's six iron rules apply in full (no arithmetic, verbatim
transcription, validator before trust, no credentials, out-of-scope stops,
privacy warning before reading documents).

## Authority limits

Scope is TY2025 Form 1040 with W-2 wages, interest/dividends/capital
gains from 1099s, 1099-NEC side income (simplified Schedule C/SE),
dependents with the child tax credit / credit for other dependents /
additional child tax credit (simplified Schedule 8812), and the
earned income credit (line 27, v1 scope in `docs/KNOWN_LIMITS.md`)
at the engine level. If the user's situation needs anything in
`docs/KNOWN_LIMITS.md` (Schedule E, other credits, state returns…),
say so now — before
collecting documents — and stop that part.
