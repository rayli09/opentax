<p align="center">
  <img src="assets/logo.png" width="128" alt="opentax logo">
</p>

<h1 align="center">opentax</h1>

<p align="center">
  <b>An agent-first, open-source US tax CLI.</b><br>
  The CLI does the math — deterministic, tested, cited.<br>
  Your AI agent drives: interview → documents → compute → review → file.
</p>

<p align="center">
  <a href="https://github.com/rayli09/opentax/actions/workflows/ci.yml"><img src="https://github.com/rayli09/opentax/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/tax%20year-2025-blue" alt="Tax year 2025">
  <img src="https://img.shields.io/badge/built%20with-Rust-orange" alt="Rust">
  <img src="https://img.shields.io/badge/license-MIT-green" alt="MIT license">
</p>

---

## Why

Tax software is a black box that charges rent. opentax is the opposite:
a small Rust engine where **every number is produced by tested code or
transcribed verbatim from a source document** — never calculated by an
LLM — wrapped in skills that let any AI agent prepare a return end to end.

**Iron rules**

- The LLM never calculates. It quotes engine output only.
- Missing input is **incomplete**, never silently zero.
- Every expected value cites its source (IRS / FTB publications).
- Everything runs locally. SSNs are masked in all output and appear
  only inside the filled PDF.

## Quickstart

```sh
cargo build --release            # binary: target/release/tax
tax selftest                     # the golden cases — the trust anchor
tax forms fetch                  # all 14 blank IRS/FTB form templates, verified

tax return create --year 2025 --status single
tax doc extract --file w2.pdf --add-to <id>   # extract + enter in one step
tax return set-identity --return <id> --data '<identity json>'
tax return set-other-income --return <id> --interest-cents 0 --dividends-cents 0

tax return get --return <id>     # every line, with completeness
tax return validate --return <id>
tax return export --return <id> --type packet --out-dir packet/
```

Agents: `tax return status --return <id>` is the compass — it names
the next step and its exact command at any point in the flow. The
full staged process (S0–S5) lives in [`skills/tax`](skills/tax/SKILL.md).

**Filing readiness.** `return get`, `return validate`, and JSON export
include a `readiness` object. Its status is `draft`, `needs_input`,
`unsupported`, or `ready_for_review`, with actionable `blockers`.
`complete` still describes computed lines and `valid` retains its legacy
error-only meaning; neither indicates readiness. `ready_for_review` is a
limited software check, not a legal correctness guarantee, human review,
or evidence of filing. PDF, packet and MeF exports require
`ready_for_review`. JSON remains available as a draft.
For a PDF or packet review copy, add `--draft`: Ghostscript (`gs`) must be
installed. This explicitly removes XFA in a temporary copy, renders every
page, and creates a noninteractive raster PDF with a prominent `DRAFT -
NOT FOR FILING` banner on every page. A manifest records readiness and
blockers; single PDFs receive a `.manifest.json` sidecar. Draft targets
must not already exist. Incomplete computed lines still prevent PDF
generation. This is a deliberate change to the export interface.

New form entries containing unmodeled fields are rejected. Legacy entries
with such fields block readiness. Identity intake must explicitly include
`blind`, `can_be_claimed_as_dependent`, and `digital_assets` as booleans
(and spouse `blind` where applicable); older identities need to be
recorded again because their defaults cannot prove that those facts were
answered. California returns also remain unsupported for readiness until
the missing state filing facts are modeled.

Readiness is conservative: dependent qualification is not fully modeled,
and nonresident forms, 1099-B reporting, 1099-NEC business facts, and
resident 1042-S entries need facts or computations the application does
not yet model, so those returns are marked `unsupported` and can produce
only explicitly labeled PDF drafts through the CLI. Other unresolved
validation findings may require review or input even when computed lines
are complete. See [known limits](docs/KNOWN_LIMITS.md).

Money is **integer cents as a string** — `"5000000"` is $50,000.00.
No floats anywhere in the filed path.

> Sanity check: one W-2, $50,000 wages, $4,000 withheld →
> tax **$3,875**, refund **$125**. That's golden case 001,
> re-verified by `tax selftest` on every run.

## What works today

- **Federal TY2025 Form 1040** — W-2 wages, 1099-INT/DIV/B, 1099-NEC
  side income (Schedule C + SE), dependents with CTC/ACTC (Schedule
  8812), the earned income credit (official EIC Table), and the OBBBA
  senior bonus (Schedule 1-A). Official Tax Table extracted as data.
- **California Form 540** — full-year residents: exemption credits,
  the official CA Tax Table, CalEITC + Young Child Tax Credit.
- **Nonresident students** — Form 1040-NR + Schedule OI + Form 8843
  for F-1/J-1 students (China/India treaties modeled), filled by the
  same packet export.
- **Documents in** — `doc extract` reads born-digital W-2 and 1099
  PDFs with per-field evidence; taxpayer identity is stored locally.
- **Paperwork out** — one packet export fills the real forms (1040 +
  Schedules D/8949/1/2/C/SE/8812/1-A + CA 540) with a per-form
  manifest, plus MeF-shaped XML validated against the genuine TY2025
  XSDs.

## Honest limits

opentax **does not e-file** — no open-source tool can: transmission
requires a firm-level EFIN and IRS acceptance testing. The filled
packet is the mailable artifact. No itemizing, rentals, K-1s, or AMT
yet. The full list lives in [`docs/KNOWN_LIMITS.md`](docs/KNOWN_LIMITS.md);
the build audit in [`docs/V1_AUDIT.md`](docs/V1_AUDIT.md).

## Digging deeper

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) ·
[`docs/PDF_SPIKE.md`](docs/PDF_SPIKE.md) ·
[`skills/`](skills/) (router + intake / ingest / preparer / reviewer / filing guide) ·
[`benchmark/cases/`](benchmark/cases/) (golden cases, every value cited)

Contributions must keep the gates green: `cargo fmt --check`,
`cargo clippy --workspace -- -D warnings`, `cargo test --workspace`,
`tax selftest`. See [`AGENTS.md`](AGENTS.md).

## License

MIT © 2026 Ray Li. Not affiliated with filedcom/opentax.
