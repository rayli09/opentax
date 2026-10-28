---
name: tax-intake
description: Interview skill for opentax. Use to start a return and discover what facts and documents are needed, driving the conversation from the engine's incompleteness — never from a generic checklist alone.
---

# tax-intake — interview

Goal: end with a created return (`tax return create`) and a confirmed list
of (a) facts to provide and (b) documents to gather. No computation here.

## Workflow

1. Run the router setup (`tax doctor`, `tax selftest`).
2. **Residency — ask this FIRST, before anything else.** Visa status
   decides the form: a nonresident alien computed on the resident
   Form 1040 is simply wrong (residents get a standard deduction and
   credits a nonresident does not). Ask: "Are you a U.S. citizen or a
   resident alien for tax purposes — or are you in the U.S. on a
   visa?" If the user is on an **F-1 or J-1 visa** (student), ask how
   many calendar years they have been in that status *including this
   year* (any part of a calendar year counts as a whole year) and
   their country of citizenship.
   - **5 years or fewer** → the **Form 1040-NR student route**. Create
     the return with `--status single` (the only filing status this
     route supports), then store the block:
     `tax return set-nra --return <id> --file nra.json`, where
     nra.json is `{"visa_type": "F-1", "citizenship": "CN",
     "first_calendar_year_in_status": 2024,
     "calendar_years_in_status_including_current": 2,
     "form_8843": {...}}`. Citizenship **CN / IN** carry treaty
     modeling; any other country computes with **no** treaty benefits
     and a loud `treaty_not_modeled` warning — say that plainly, do
     not paper over it. Collect the **Form 8843 facts** at the same
     time (school name/address/phone; program director
     name/address/phone; days present in the current year and the
     two prior years): an exempt individual must file a fully
     completed Form 8843 **every year, even with no income**. The
     document checklist for this route is different — **W-2s and
     Forms 1042-S** (income code 20 = wages during study/training,
     code 15 = scholarship/fellowship) — and you **skip** the
     dependents / credits / EIC questions below entirely (none are
     computed on this route) and **skip the California question**
     (California does not honor federal treaties and nonresidents
     file Form 540NR, which is out of scope; a state block together
     with an nra block is an explicit error).
   - **More than 5 years** → `tax return set-nra` refuses, by design:
     the filer is likely a resident alien under the substantial
     presence test. Continue with the resident route below, and say
     that dual-status years are out of scope.
   - Any other visa, or a married NR filer → the NR route does not
     cover them; say so and stop that part (resident aliens continue
     below).
3. Establish the basics with the user: tax year (v1: 2025 only), filing
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
4. Create the return:
   `tax return create --year 2025 --status <status>`
   Record the returned `returnId` and keep using exactly that id.
5. Collect the **taxpayer identity** and store it with
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
6. If the user can state interest and dividend totals now, pass them at
   create time (`--interest-cents`, `--dividends-cents`, integer cents).
   If the return already exists without them, record the amounts with
   `tax return set-other-income --return <id> --interest-cents <n>
   --dividends-cents <n>` — the return cannot complete until they are
   recorded, even as 0. Otherwise note them as outstanding — the return
   will honestly show dependent lines as incomplete until they exist as
   documents or recorded facts.
7. Hand the user a document checklist derived from their income sources:
   one W-2 per job; 1099-INT per bank; 1099-DIV per fund/broker;
   1099-B for brokerage sales; 1099-NEC per side-income payer **plus
   their own expense records for each engagement** (the expenses total
   is an entered fact, not a form box); prior-year return if available.
8. **State return (California only in v1).** Ask whether the user was
   a **full-year California resident** for the tax year. If yes, store
   the selection with
   `tax return set-state --return <id> --state CA --file state.json`,
   where state.json is `{"state": "CA", "residency": "full_year"}` —
   only CA + full_year exists; any other state or a part-year/
   nonresident year is out of scope (say so and stop that part).
   Then three transcription facts join the checklist, with the same
   missing-is-not-zero doctrine as everything else:
   - **W-2 Box 16 (state wages)** and **Box 17 (state income tax
     withheld)** for every W-2. Box 17 feeds Form 540 line 71; Box 16
     — not Box 1 — is the California EITC's wage figure. If a W-2
     reports no state withholding, record `"0"` explicitly; leaving
     the box unrecorded leaves the California withholding (and the
     refund/amount-owed lines) incomplete, never zero.
   - **1099-NEC Box 5 (state tax withheld)** for every 1099-NEC, with
     the same explicit-`"0"` rule.
   - The identity dates of birth now matter for California too (the
     65+/blind **exemption credits**, and the childless CalEITC
     18-or-older test), and dependent ages decide the Young Child Tax
     Credit (a qualifying child **younger than six** as of December
     31). California accepts ITINs for the CalEITC — a stored 9-digit
     identifier is enough; do not press for more.
9. Confirm a short facts summary back to the user and stop. Next step is
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
at the engine level, plus a full-year-resident California Form 540
when the state block is set (CA v1 scope in `docs/KNOWN_LIMITS.md`:
no 540NR, no renter's credit, Schedule CA adjustments scoped to the
engine's facts), plus the **Form 1040-NR student route** when the
nra block is set (F-1 / J-1 students in their exempt-individual
years, single, W-2 and Form 1042-S wages/scholarship, China and
India treaty modeling; NRA v1 scope in `docs/KNOWN_LIMITS.md`: no
Schedule NEC, no credits, no state taxation of nonresidents, no
dual-status). If the user's situation needs anything in
`docs/KNOWN_LIMITS.md` (Schedule E, other credits, other states…),
say so now — before
collecting documents — and stop that part.
