---
name: tax-intake
description: Interview details for opentax stages S1 (route) and S3 (identity & options) of THE RETURN PROCESS (skills/tax). Use to ask the residency question first, establish filing status and dependents, and collect the identity facts the engine keys off — driven by the return's incompleteness, never a generic checklist alone.
---

# tax-intake — the S1 and S3 questions

The process (stages, commands, exit criteria) lives in the router
(skills/tax, "THE RETURN PROCESS"). This skill holds the **questions**
for S1 and S3: what to ask, why each fact matters, and what it
unlocks. Payload shapes are the CLI's, not this file's: assemble
every payload from `tax schema` and never from a copied example.

## S1 — residency FIRST, before anything else

Visa status decides the form: a nonresident alien computed on the
resident Form 1040 is simply wrong (residents get a standard
deduction and credits a nonresident does not). Ask: "Are you a U.S.
citizen or a resident alien for tax purposes — or are you in the
U.S. on a visa?"

**F-1 / J-1 (student)** → ask how many calendar years they have
been in that status *including this year* (any part of a calendar
year counts as a whole year) and their country of citizenship.

- **5 years or fewer** → the **Form 1040-NR student route**. Create
  the return with `--status single` (the only filing status this
  route supports), then store the route with
  `tax return set-nra --return <id>` (`--data` / `--file` payload;
  shape: `tax schema`). Facts to collect for the block: visa type,
  citizenship, first calendar year in status, calendar years in
  status including current. Citizenship **CN / IN** carry treaty
  modeling; any other country computes with **no** treaty benefits
  and a loud `treaty_not_modeled` warning — say that plainly, do not
  paper over it.
- **More than 5 years** → `set-nra` refuses, by design: the filer is
  likely a resident alien under the substantial presence test.
  Continue on the resident route and say that dual-status years are
  out of scope.
- Any other visa, or a married NR filer → the NR route does not
  cover them; say so and stop that part.

**On the NR route, also collect the Form 8843 facts in S3** (below)
and use the NR document checklist in S2: **W-2s and Forms 1042-S**
(income code 20 = wages during study/training, code 15 =
scholarship/fellowship). **Skip** the dependents/credits/EIC
questions entirely (none are computed on this route) and **skip the
California question** (California does not honor federal treaties
and nonresidents file Form 540NR, which is out of scope; a state
block together with an nra block is an explicit error,
`nra_state_conflict`).

## S1 — resident basics

Establish: tax year (v1: 2025 only), filing status
(single / mfj / hoh / mfs), and the income sources present this
year (employment W-2s, bank interest, dividends, brokerage, side
income). Then create the return:
`tax return create --year 2025 --status <status>` — record the
returned `returnId` and keep using exactly that id. (Interest and
dividend totals the filer can already state may go in at create
time via `--interest-cents` / `--dividends-cents`; otherwise they
are an S3 recording — router S3.)

**Dependents** — for each person the filer claims, ask:

- relationship (child / stepchild / foster child / other), age on
  December 31 of the tax year, and how many months they lived with
  the taxpayer — these drive the child tax credit / credit for
  other dependents (simplified Schedule 8812) and the earned
  income credit;
- their **SSN**: it is stored with the entry and prints in the
  Dependents table. The child tax credit legally requires the
  child to have one (the engine warns, but still computes, when
  it is missing); the **earned income credit does not count a
  child without one at all** — collect it whenever the filer
  has it;
- for each child, the two EIC facts: were they a **full-time
  student** (during some part of each of 5 calendar months), and
  are they **permanently and totally disabled**? These decide
  whether a child who is 19 or older can still be an EIC
  qualifying child.

Each dependent becomes one `dependent` entry in S3
(`tax form add --node-type dependent`; shape:
`tax node template --document dependent`).

## S3 — identity facts (every return)

Collect and store with `tax return set-identity --return <id>`
(`--data` / `--file` payload; shape: `tax schema TaxpayerIdentity`):
first/last name, SSN (dashed or 9 digits), date of birth,
occupation, mailing address (street, apt, city, state, ZIP), and
three explicit yes/no answers — **filing readiness refuses export
until all three are recorded**, so collect them here, not at the
export step: whether the taxpayer is blind (`blind`); whether
someone can claim the taxpayer as a dependent
(`can_be_claimed_as_dependent` — it fills the form's checkbox; the
dependent-filer deduction limit itself is not computed in v1);
and the Form 1040 digital-assets question (`digital_assets` —
ask it as printed on the form; the recorded answer is a fact of
its own and does not by itself determine any income treatment).
For mfj/mfs also collect the spouse's same facts. Identity is
stored locally only and every CLI output shows SSNs masked; say
so once, plainly.

Facts hiding inside the date of birth — ask explicitly ("were you
(or your spouse) born before January 2, 1961, or blind?"):

- **Age-65/blind standard-deduction additions** key off DOB +
  blindness.
- The **Schedule 1-A Part V enhanced deduction for seniors** (line
  13b: $6,000 per person born before January 2, 1961, phased out
  over $75,000 / $150,000 MFJ of MAGI; married filing separately
  takes none) — with no date of birth stored, the engine takes no
  senior deduction (validate reports `senior_bonus_age_unknown`).
- The **no-children earned income credit** (age 25–64 at year-end):
  with no dependents and no date of birth stored, the engine
  cannot determine it and takes no childless EIC (validate
  reports `eic_childless_age_unknown`).

## S3 — California branch (resident route, full-year CA residents)

Ask whether the filer was a **full-year California resident** for
the tax year. If yes, store the selection with
`tax return set-state --return <id> --state CA` (`--data` /
`--file` payload; shape: `tax schema`) — only CA + full_year
exists; any other state or a part-year/nonresident year is out of
scope (say so and stop that part). Three transcription facts then
join the S2 checklist, under the same missing-is-not-zero doctrine
as everything else:

- **W-2 Box 16 (state wages)** and **Box 17 (state income tax
  withheld)** for every W-2. Box 17 feeds Form 540 line 71;
  Box 16 — not Box 1 — is the California EITC's wage figure. If a
  W-2 reports no state withholding, record `"0"` explicitly;
  leaving the box unrecorded leaves the California withholding
  (and the refund/amount-owed lines) incomplete, never zero.
- **1099-NEC Box 5 (state tax withheld)** for every 1099-NEC, with
  the same explicit-`"0"` rule.
- Identity dates of birth matter for California too (the 65+/blind
  **exemption credits**, and the childless CalEITC 18-or-older
  test), and dependent ages decide the Young Child Tax Credit (a
  qualifying child **younger than six** as of December 31).
  California accepts ITINs for the CalEITC — a stored 9-digit
  identifier is enough; do not press for more.

## S3 — NR route: the Form 8843 facts

An exempt individual must file a fully completed Form 8843 **every
year, even with no income**. Collect: school name/address/phone;
program director name/address/phone; days present in the current
year and the two prior years. They are stored in the nra block
(`tax return set-nra`; shape: `tax schema`) and printed by the S5
packet onto the 8843 and Schedule OI. Facts not collected stay
blank on the forms and are listed in the packet manifest as
hand-complete items — never invent them.

## Document checklist (hand to tax-ingest for S2)

Derive it from the filer's stated income sources: one W-2 per job;
1099-INT per bank; 1099-DIV per fund/broker; 1099-B for brokerage
sales; 1099-NEC per side-income payer **plus the filer's own
expense records for each engagement** (the expenses total is an
entered fact, not a form box); on the NR route, 1042-S forms
instead of 1099s; prior-year return if available.

## Scope — say it now, not later

If the filer's situation needs anything in `docs/KNOWN_LIMITS.md`
(Schedule E, other credits, other states, 540NR, dual-status,
Schedule NEC…), say so during S1 — before collecting documents —
and stop that part. The router's authority limits and iron rules
apply in full.
