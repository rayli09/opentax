# PDF fill spike

Standalone experiment (not a workspace member) behind `docs/PDF_SPIKE.md`.
Question: can the pure-Rust fill path in `crates/tax-pdf` (lopdf, sets
`/V` only) fill real IRS forms so values render — or do we need
pdfer_forms (/AP regeneration) or pdfcpu/pdfium?

Arms: A = tax-pdf as shipped · A1 = tax-pdf /V-only with name-decoding
workaround · A2 = A1 + NeedAppearances · B = pdfer_forms 0.3.4 ·
C = pdfcpu 0.16.3 (external binary) · Bflat = B with flatten=true.

Layout:
- `src/main.rs` — enumerate (tax-pdf + pdfer), fill-a / fill-a-names / fill-a2 / fill-b
- `scripts/make_targets.py` — builds `targets.json` from the XFA template in each IRS PDF
- `scripts/fill_pdfcpu.py` — arm C driver (export → set values → fill)
- `scripts/verify.py` — independent readback (pypdf + pdfcpu), /AP and /AS checks, pdftotext visibility
- `targets.json` — 57 semantic targets with qualified field names and synthetic values
- `fixtures/` — IRS PDFs (git-ignored; download URLs + SHA-256 in docs/PDF_SPIKE.md)
- `results/` — verification JSON + rendered crops (filled PDFs git-ignored)

Full reproduction commands: see `docs/PDF_SPIKE.md`.
