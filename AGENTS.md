# Contributor rules (humans and agents)

- **No floats for money.** Money is integer cents (`tax_core::Money`) end to
  end in the filed path. `f64` may appear only in non-filed analysis tooling.
- **Missing ≠ 0.** An unprovided input is `FactResult::Incomplete`. Never
  zero-fill a fact, a line, or a test expectation to make something pass.
- **Every expected value cites a source.** Golden cases carry a source per
  value (IRS publication, form instruction, or Rev. Proc.). A value without
  a citation does not enter `benchmark/cases/`.
- **Cases are never edited to pass.** If the engine disagrees with a golden
  case, fix the engine — or escalate with the citation that proves the case
  wrong, and record the change in the case's sources.
- **Parameters are data.** Rates, brackets, thresholds, and deductions live
  in `crates/tax-core/params/<year>.json` with a `source` field. No hardcoded
  tax rates in Rust code.
- **The LLM never calculates.** Skills and CLI output quote engine results;
  agents transcribe documents and drive workflow only.
- Before committing: `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`,
  `cargo test --workspace`, and `tax selftest` must all pass.
- Stubs must be honest: return `"status": "not_implemented"` rather than a
  plausible-looking fake result.
