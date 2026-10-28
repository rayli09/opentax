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
tax selftest                     # 13 golden cases — the trust anchor

tax return create --year 2025 --status single
tax doc extract --file w2.pdf    # born-digital W-2 / 1099-INT / DIV / NEC
tax form add --return <id> --node-type w2 --file w2.json
tax return set-identity --return <id> --file identity.json
tax return set-state --return <id> --state CA --file state.json   # optional

tax return get --return <id>     # every line, with completeness
tax return validate --return <id>
tax return export --return <id> --type packet --forms-dir <dir> --out-dir <dir>
```

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
