#!/usr/bin/env python3
"""Acceptance test for the nonresident-alien forms in
`tax return export --type packet` (and the single-form `--type pdf`).

Builds the NR-002 golden-case facts (CN citizen, F-1, first calendar
year in status; W-2 $3,000 with $500 withheld + treaty-exempt Form
1042-S wages of $5,000) through the real CLI -- create / form add /
set-identity / set-nra (the golden input.json carries no identity or
Form 8843 facts, so those are added exactly as a user's CLI flow
would) -- runs the packet export against the blank TY2025 templates
in spikes/pdf-fill/fixtures/, then reads the filled PDFs back with
pypdf and asserts the NRA-1 engine values landed in the geometrically
verified fields:

  Form 1040-NR       1a 3000.00, 1k 5000.00, 16 303.00, 25a 500.00,
                     25g 0.00, 33 500.00, 34 197.00 (37 stays blank);
                     filing status Single; occupation prints
  Schedule OI        item A CN, item E F-1, item H 2025 = 200 days,
                     item L row 1 = CN / 20(c) / 5000.00, line 1(e)
                     total 5000.00
  Form 8843          Part III line 9/10 carry the stored school and
                     director facts; line 4a 2025 = 200, line 4b = 200;
                     line 12 No; addresses blank by the form's own
                     instruction (it is attached to the 1040-NR)

and that manifest.json reports all three forms filled, with the
route entry stating the resident/CA forms are not part of an NR
packet. The full SSN appears ONLY inside the filled PDFs -- never in
the export output or the manifest.

Further scenarios: a no-treaty NR return (DE citizen, no Form 8843
facts) reports f1040soi as not_applicable while f8843 stays filled
with hand-complete notes; a forms-dir missing f1040soi.pdf /
f8843.pdf reports them as loud missing_template entries; the
single-form `--type pdf` export fills the 1040-NR alone; `--type
mef` stays an explicit not_implemented error; and a return with an
unmodeled 1042-S income code makes the packet export refuse, naming
the incomplete Form 1040-NR lines.

Usage:
    python3 scripts/nra_packet_acceptance.py [--bin PATH] [--forms-dir DIR]

The `tax` binary defaults to $CARGO_TARGET_DIR/debug/tax, then
target/debug/tax. Requires pypdf (pip install pypdf).
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

try:
    from pypdf import PdfReader
except ImportError:
    sys.exit("nra_packet_acceptance: pypdf is required (pip install pypdf)")

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NR_CASES = os.path.join(REPO, "benchmark", "cases", "f1040nr", "2025")

FAILURES = []


def check(cond, msg):
    if cond:
        print(f"  ok: {msg}")
    else:
        FAILURES.append(msg)
        print(f"  FAIL: {msg}")


def run_tax(bin_path, store, *args, expect_ok=True):
    cmd = [bin_path, "--store", store, "--output", "json", *args]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if expect_ok and proc.returncode != 0:
        raise RuntimeError(f"tax {' '.join(args)} failed: {proc.stderr.strip()}")
    if proc.returncode != 0:
        return None
    return json.loads(proc.stdout)


def run_tax_raw(bin_path, store, *args):
    cmd = [bin_path, "--store", store, "--output", "json", *args]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    return proc.returncode, proc.stdout + proc.stderr


def write_json(tmp, name, data):
    path = os.path.join(tmp, name)
    with open(path, "w") as fh:
        json.dump(data, fh)
    return path


NR002_IDENTITY = {
    "first_name": "Case",
    "last_name": "Tester",
    "ssn": "123456789",
    "occupation": "Student",
    "address": {
        "street": "1 College Ave",
        "city": "Berkeley",
        "state": "CA",
        "zip": "94720",
    },
}

NR002_NRA = {
    "visa_type": "F-1",
    "citizenship": "CN",
    "first_calendar_year_in_status": 2025,
    "calendar_years_in_status_including_current": 1,
    "form_8843": {
        "school_name": "State University",
        "school_address": "100 University Dr, Berkeley, CA 94720",
        "school_phone": "510-555-0100",
        "director_name": "Dr. Director",
        "director_address": "200 Program Hall, Berkeley, CA 94720",
        "director_phone": "510-555-0101",
        "days_present_current_year": 200,
        "days_present_prior_year_1": None,
        "days_present_prior_year_2": None,
    },
}


def build_nr002(bin_path, store, tmp):
    """NR-002's income facts (from its golden input.json) plus the
    identity and Form 8843 facts the golden case does not carry."""
    with open(os.path.join(NR_CASES, "nr-002-cn-split-1042s", "input.json")) as fh:
        data = json.load(fh)
    created = run_tax(
        bin_path, store, "return", "create", "--status", data["status"]
    )
    rid = created["returnId"]
    for entry in data.get("w2s", []):
        run_tax(
            bin_path, store, "form", "add", "--return", rid,
            "--node-type", "w2", "--data", json.dumps(entry),
        )
    for entry in data.get("f1042ss", []):
        run_tax(
            bin_path, store, "form", "add", "--return", rid,
            "--node-type", "1042-s", "--data", json.dumps(entry),
        )
    run_tax(
        bin_path, store, "return", "set-identity", "--return", rid,
        "--file", write_json(tmp, "identity.json", NR002_IDENTITY),
    )
    run_tax(
        bin_path, store, "return", "set-nra", "--return", rid,
        "--file", write_json(tmp, "nra.json", NR002_NRA),
    )
    return rid


def field_values(pdf_path):
    """Qualified field name -> string value (or checkbox on-state)."""
    fields = PdfReader(pdf_path).get_fields() or {}
    out = {}
    for name, fld in fields.items():
        v = fld.get("/V")
        if v is not None:
            out[name] = str(v)
    return out


def expect(fields, qname, expected, label):
    actual = fields.get(qname)
    check(actual == expected, f"{label}: {qname} == {expected!r} (got {actual!r})")


def expect_blank(fields, qname, label):
    actual = fields.get(qname)
    check(
        actual in (None, "", "/Off"),
        f"{label}: {qname} stays blank (got {actual!r})",
    )


def manifest(out_dir):
    with open(os.path.join(out_dir, "manifest.json")) as fh:
        return json.load(fh)


def manifest_statuses(out_dir):
    return {f["form"]: f for f in manifest(out_dir)["forms"]}


def export_packet(bin_path, store, rid, forms_dir, out_dir):
    return run_tax(
        bin_path, store, "return", "export", "--return", rid,
        "--type", "packet", "--forms-dir", forms_dir, "--out-dir", out_dir,
    )


P1 = "topmostSubform[0].Page1[0]."
P2 = "topmostSubform[0].Page2[0]."
O1 = "form1040-NR[0].Page1[0]."
S1 = "topmostSubform[0].Page1[0]."
L_ROW1 = O1 + "LineL1_Table[0].BodyRow1[0]."


def case_nr002_packet(bin_path, store, forms_dir, tmp):
    print("case nr-002: 1040-NR + Schedule OI + 8843 filled from engine values")
    rid = build_nr002(bin_path, store, tmp)
    out_dir = os.path.join(tmp, "pkt-nr-002")
    result = export_packet(bin_path, store, rid, forms_dir, out_dir)
    statuses = manifest_statuses(out_dir)

    check(
        manifest(out_dir)["form"] == "1040-NR",
        "nr-002 manifest: packet form is 1040-NR",
    )
    check(
        statuses["route"]["status"] == "not_applicable",
        "nr-002 manifest: route entry states the NR packet contents",
    )
    route_notes = " ".join(statuses["route"]["notes"])
    check(
        "540NR is not implemented" in route_notes,
        "nr-002 manifest: route note keeps the 540NR warning",
    )
    for form in ("f1040nr", "f1040soi", "f8843"):
        check(
            statuses[form]["status"] == "filled",
            f"nr-002 manifest: {form} is filled (got {statuses[form]['status']})",
        )
    check(
        "f1040" not in statuses and "f540" not in statuses,
        "nr-002 manifest: no resident or CA forms in the NR packet",
    )

    # --- Form 1040-NR ---
    f = field_values(os.path.join(out_dir, "f1040nr.pdf"))
    expect(f, P1 + "f1_14[0]", "Case", "1040-NR taxpayer first name")
    expect(f, P1 + "f1_15[0]", "Tester", "1040-NR taxpayer last name")
    expect(f, P1 + "f1_16[0]", "123456789", "1040-NR SSN in PDF")
    expect(f, P1 + "c1_5[0]", "/1", "1040-NR filing status Single")
    expect(f, P1 + "f1_42[0]", "3000.00", "1040-NR line 1a")
    expect(f, P1 + "Line1k_ReadOrder[0].f1_53[0]", "5000.00", "1040-NR line 1k")
    expect(f, P1 + "f1_54[0]", "3000.00", "1040-NR line 1z")
    expect(f, P2 + "f2_09[0]", "303.00", "1040-NR line 16 tax")
    expect(f, P2 + "Line25_ReadOrder[0].f2_21[0]", "500.00", "1040-NR line 25a")
    expect(f, P2 + "f2_27[0]", "0.00", "1040-NR line 25g (stated zero)")
    expect(f, P2 + "f2_35[0]", "500.00", "1040-NR line 33 total payments")
    expect(f, P2 + "f2_36[0]", "197.00", "1040-NR line 34 refund")
    expect_blank(f, P2 + "f2_42[0]", "1040-NR line 37 (refund case)")
    expect(f, P2 + "f2_47[0]", "Student", "1040-NR occupation")
    # Unstored facts stay blank.
    expect_blank(f, P1 + "f1_22[0]", "1040-NR foreign country (not stored)")
    expect_blank(f, P1 + "c1_6[0]", "1040-NR Digital Assets (not stored)")

    # --- Schedule OI ---
    oi = field_values(os.path.join(out_dir, "f1040soi.pdf"))
    expect(oi, O1 + "f1_1[0]", "Case Tester", "OI header name")
    expect(oi, O1 + "f1_3[0]", "CN", "OI item A citizenship")
    expect(oi, O1 + "f1_5[0]", "F-1", "OI item E visa/status")
    expect(oi, O1 + "f1_25[0]", "200", "OI item H days 2025")
    expect_blank(oi, O1 + "f1_24[0]", "OI item H days 2024 (not stored)")
    expect_blank(oi, O1 + "f1_23[0]", "OI item H days 2023 (not stored)")
    expect(oi, L_ROW1 + "f1_27[0]", "CN", "OI item L row 1 country")
    expect(oi, L_ROW1 + "f1_28[0]", "20(c)", "OI item L row 1 article")
    expect(oi, L_ROW1 + "f1_30[0]", "5000.00", "OI item L row 1 amount")
    expect(oi, O1 + "f1_39[0]", "5000.00", "OI item L line 1(e) total")

    # --- Form 8843 ---
    s = field_values(os.path.join(out_dir, "f8843.pdf"))
    expect(s, S1 + "f1_04[0]", "Case", "8843 first name")
    expect(s, S1 + "f1_05[0]", "Tester", "8843 last name")
    expect(s, S1 + "f1_06[0]", "123456789", "8843 TIN in PDF")
    expect(s, S1 + "f1_09[0]", "F-1", "8843 line 1a visa type")
    expect(s, S1 + "f1_11[0]", "CN", "8843 line 2 citizenship")
    expect(s, S1 + "f1_14[0]", "200", "8843 line 4a 2025 days")
    expect(s, S1 + "f1_17[0]", "200", "8843 line 4b excluded days")
    expect(
        s, S1 + "f1_26[0]",
        "State University, 100 University Dr, Berkeley, CA 94720, 510-555-0100",
        "8843 line 9 school",
    )
    expect(
        s, S1 + "f1_27[0]",
        "Dr. Director, 200 Program Hall, Berkeley, CA 94720, 510-555-0101",
        "8843 line 10 director",
    )
    expect(s, S1 + "c1_2[1]", "/2", "8843 line 12 No")
    expect_blank(s, S1 + "f1_07[0]", "8843 country-of-residence address (attached filing)")
    expect_blank(s, S1 + "f1_08[0]", "8843 U.S. address (attached filing)")
    expect_blank(s, S1 + "f1_28[0]", "8843 line 11 year 2019 (before first year in status)")

    # SSN hygiene: full SSN only inside the PDFs.
    blob = json.dumps(result) + open(os.path.join(out_dir, "manifest.json")).read()
    check(
        "123456789" not in blob and "123-45-6789" not in blob,
        "nr-002: full SSN appears only inside the filled PDFs, never in export output or manifest",
    )


def case_no_treaty(bin_path, store, forms_dir, tmp):
    print("case no-treaty: Schedule OI not_applicable, 8843 still filled")
    created = run_tax(bin_path, store, "return", "create", "--status", "single")
    rid = created["returnId"]
    run_tax(
        bin_path, store, "form", "add", "--return", rid, "--node-type", "w2",
        "--data", json.dumps(
            {"employer_ein": "12-3456789", "wages": 800000, "federal_withholding": 30000}
        ),
    )
    nra = {
        "visa_type": "F-1",
        "citizenship": "DE",
        "first_calendar_year_in_status": 2024,
        "calendar_years_in_status_including_current": 2,
    }
    run_tax(
        bin_path, store, "return", "set-nra", "--return", rid,
        "--file", write_json(tmp, "nra-de.json", nra),
    )
    out_dir = os.path.join(tmp, "pkt-de")
    export_packet(bin_path, store, rid, forms_dir, out_dir)
    statuses = manifest_statuses(out_dir)
    check(
        statuses["f1040nr"]["status"] == "filled",
        f"de manifest: f1040nr is filled (got {statuses['f1040nr']['status']})",
    )
    check(
        statuses["f1040soi"]["status"] == "not_applicable",
        f"de manifest: f1040soi is not_applicable (got {statuses['f1040soi']['status']})",
    )
    oi_notes = " ".join(statuses["f1040soi"]["notes"])
    check(
        "no treaty claim" in oi_notes,
        "de manifest: f1040soi note states the no-treaty-claim basis",
    )
    check(
        not os.path.exists(os.path.join(out_dir, "f1040soi.pdf")),
        "de: no f1040soi.pdf is written",
    )
    check(
        statuses["f8843"]["status"] == "filled",
        f"de manifest: f8843 is filled (got {statuses['f8843']['status']})",
    )
    s8843_notes = " ".join(statuses["f8843"]["notes"])
    check(
        "no Form 8843 facts" in s8843_notes,
        "de manifest: f8843 notes list the hand-complete items loudly",
    )
    s = field_values(os.path.join(out_dir, "f8843.pdf"))
    expect(s, S1 + "f1_09[0]", "F-1", "8843 (DE) line 1a visa type")
    expect(s, S1 + "f1_11[0]", "DE", "8843 (DE) line 2 citizenship")
    expect(s, S1 + "f1_33[0]", "F-1", "8843 (DE) line 11 year 2024")
    expect_blank(s, S1 + "f1_32[0]", "8843 (DE) line 11 year 2023 (before first year)")
    expect_blank(s, S1 + "f1_14[0]", "8843 (DE) line 4a 2025 (not stored)")


def case_missing_template(bin_path, store, forms_dir, tmp):
    print("forms-dir without f1040soi.pdf/f8843.pdf: loud missing_template")
    rid = build_nr002(bin_path, store, tmp)
    sparse = os.path.join(tmp, "sparse-forms")
    os.makedirs(sparse)
    shutil.copy(os.path.join(forms_dir, "f1040nr.pdf"), os.path.join(sparse, "f1040nr.pdf"))
    out_dir = os.path.join(tmp, "pkt-sparse")
    export_packet(bin_path, store, rid, sparse, out_dir)
    statuses = manifest_statuses(out_dir)
    check(
        statuses["f1040nr"]["status"] == "filled",
        f"sparse manifest: f1040nr is filled (got {statuses['f1040nr']['status']})",
    )
    for form in ("f1040soi", "f8843"):
        check(
            statuses[form]["status"] == "missing_template",
            f"sparse manifest: {form} is missing_template (got {statuses[form]['status']})",
        )


def case_single_pdf_and_mef(bin_path, store, forms_dir, tmp):
    print("single-form --type pdf fills the 1040-NR; --type mef refuses")
    rid = build_nr002(bin_path, store, tmp)
    out_pdf = os.path.join(tmp, "single-nr.pdf")
    run_tax(
        bin_path, store, "return", "export", "--return", rid, "--type", "pdf",
        "--template", os.path.join(forms_dir, "f1040nr.pdf"), "--out", out_pdf,
    )
    f = field_values(out_pdf)
    expect(f, P1 + "f1_42[0]", "3000.00", "single pdf line 1a")
    expect(f, P2 + "f2_36[0]", "197.00", "single pdf line 34 refund")
    code, out = run_tax_raw(
        bin_path, store, "return", "export", "--return", rid, "--type", "mef"
    )
    check(code != 0 or "not_implemented" in out, "mef export on an NR return fails")
    check(
        "not_implemented" in out and "1040-NR MeF" in out,
        "mef error names the separate 1040-NR MeF package",
    )


def case_incomplete_refused(bin_path, store, forms_dir, tmp):
    print("unmodeled 1042-S code: packet export refuses, naming NR lines")
    created = run_tax(bin_path, store, "return", "create", "--status", "single")
    rid = created["returnId"]
    run_tax(
        bin_path, store, "form", "add", "--return", rid, "--node-type", "w2",
        "--data", json.dumps(
            {"employer_ein": "12-3456789", "wages": 300000, "federal_withholding": 50000}
        ),
    )
    run_tax(
        bin_path, store, "form", "add", "--return", rid, "--node-type", "1042-s",
        "--data", json.dumps(
            {
                "payer_name": "State University",
                "income_code": 50,
                "gross_income": 100000,
                "federal_withholding": 0,
            }
        ),
    )
    nra = {
        "visa_type": "F-1",
        "citizenship": "CN",
        "first_calendar_year_in_status": 2025,
        "calendar_years_in_status_including_current": 1,
    }
    run_tax(
        bin_path, store, "return", "set-nra", "--return", rid,
        "--file", write_json(tmp, "nra-inc.json", nra),
    )
    out_dir = os.path.join(tmp, "pkt-incomplete")
    code, out = run_tax_raw(
        bin_path, store, "return", "export", "--return", rid, "--type", "packet",
        "--forms-dir", forms_dir, "--out-dir", out_dir,
    )
    check(code != 0 or "return_incomplete" in out, "incomplete NR packet export fails")
    check(
        "return_incomplete" in out and "Form 1040-NR lines incomplete" in out,
        "incomplete error names the Form 1040-NR lines",
    )
    check(
        not os.path.exists(os.path.join(out_dir, "manifest.json")),
        "incomplete: no manifest is written",
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=None)
    ap.add_argument(
        "--forms-dir",
        default=os.path.join(REPO, "spikes", "pdf-fill", "fixtures"),
    )
    args = ap.parse_args()
    bin_path = args.bin
    if bin_path is None:
        target = os.environ.get("CARGO_TARGET_DIR", os.path.join(REPO, "target"))
        bin_path = os.path.join(target, "debug", "tax")
    if not os.path.exists(bin_path):
        sys.exit(f"nra_packet_acceptance: tax binary not found at {bin_path}")
    for name in ("f1040nr.pdf", "f1040soi.pdf", "f8843.pdf"):
        if not os.path.exists(os.path.join(args.forms_dir, name)):
            sys.exit(
                f"nra_packet_acceptance: template {name} missing in {args.forms_dir}"
            )
    with tempfile.TemporaryDirectory() as tmp:
        store = os.path.join(tmp, "store")
        case_nr002_packet(bin_path, store, args.forms_dir, tmp)
        case_no_treaty(bin_path, store, args.forms_dir, tmp)
        case_missing_template(bin_path, store, args.forms_dir, tmp)
        case_single_pdf_and_mef(bin_path, store, args.forms_dir, tmp)
        case_incomplete_refused(bin_path, store, args.forms_dir, tmp)
    if FAILURES:
        print(f"\nnra_packet_acceptance: {len(FAILURES)} FAILURE(S)")
        return 1
    print("\nnra_packet_acceptance: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
