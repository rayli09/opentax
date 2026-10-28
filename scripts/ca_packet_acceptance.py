#!/usr/bin/env python3
"""California packet acceptance: check golden 540 calculations and rendered draft output for the dependent case, and normal federal-only packet behavior. Requires pypdf, Ghostscript and Tesseract."""

from draft_checks import verify_packet

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
    sys.exit("ca_packet_acceptance: pypdf is required (pip install pypdf)")

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CA_CASES = os.path.join(REPO, "benchmark", "cases", "ca540", "2025")
FED_CASES = os.path.join(REPO, "benchmark", "cases", "f1040", "2025")

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


def build_return(bin_path, store, cases_dir, case):
    """Recreate a golden case's return through the CLI (federal or CA)."""
    with open(os.path.join(cases_dir, case, "input.json")) as fh:
        data = json.load(fh)
    created = run_tax(
        bin_path,
        store,
        "return",
        "create",
        "--status",
        data["status"],
        "--interest-cents",
        str(int(data.get("interest_income", "0"))),
        "--dividends-cents",
        str(int(data.get("ordinary_dividends", "0"))),
    )
    rid = created["returnId"]
    groups = [
        ("w2s", "w2"),
        ("f1099_ints", "1099-int"),
        ("f1099_divs", "1099-div"),
        ("f1099_bs", "1099-b"),
        ("f1099_necs", "1099-nec"),
        ("dependents", "dependent"),
    ]
    for key, node_type in groups:
        for entry in data.get(key, []):
            run_tax(
                bin_path,
                store,
                "form",
                "add",
                "--return",
                rid,
                "--node-type",
                node_type,
                "--data",
                json.dumps(entry),
            )
    if "identity" not in data:
        data["identity"] = {"first_name":"Synthetic", "last_name":"Tester", "ssn":"123456789", "date_of_birth":"1990-01-01"}
    data["identity"].setdefault("digital_assets", False)
    data["identity"].setdefault("blind", False)
    data["identity"].setdefault("can_be_claimed_as_dependent", False)
    if data["identity"].get("spouse"):
        data["identity"]["spouse"].setdefault("blind", False)
    data["identity"].setdefault("address", {"street":"1 Main St", "city":"Example", "state":"CA", "zip":"90001"})
    if "identity" in data:
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
            json.dump(data["identity"], fh)
            identity_file = fh.name
        run_tax(
            bin_path,
            store,
            "return",
            "set-identity",
            "--return",
            rid,
            "--file",
            identity_file,
        )
    if "state" in data:
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
            json.dump(data["state"], fh)
            state_file = fh.name
        run_tax(
            bin_path,
            store,
            "return",
            "set-state",
            "--return",
            rid,
            "--file",
            state_file,
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
        bin_path,
        store,
        "return",
        "export",
        "--return",
        rid,
        "--type",
        "packet",
        "--forms-dir",
        forms_dir,
        "--out-dir",
        out_dir,
    )


MFJ_ON_STATE = (
    "/2 . Married/R D P filing jointly (even if only one spouse / R D P "
    "had income). See instructions."
)


def case_ca_002(bin_path, store, forms_dir, tmp):
    print("case ca-002-family-caleitc: Form 540 filled, Schedule CA not applicable")
    rid = build_return(bin_path, store, CA_CASES, "ca-002-family-caleitc")
    out_dir = os.path.join(tmp, "pkt-ca-002")
    result = export_packet(bin_path, store, rid, forms_dir, out_dir)
    statuses = manifest_statuses(out_dir)

    check(
        statuses["f540"]["status"] == "filled",
        f"ca-002 manifest: f540 is filled (got {statuses['f540']['status']})",
    )
    check(
        statuses["f540ca"]["status"] == "not_applicable",
        f"ca-002 manifest: f540ca is not_applicable (got {statuses['f540ca']['status']})",
    )
    sch_notes = " ".join(statuses["f540ca"]["notes"])
    check(
        "no California adjustments" in sch_notes,
        "ca-002 manifest: f540ca note states the no-adjustments basis",
    )
    order = [f["form"] for f in manifest(out_dir)["forms"]]
    check(
        order.index("f540") > order.index("f1040s1a"),
        "ca-002 manifest: CA forms come after the federal packet",
    )

    f = field_values(os.path.join(out_dir, "f540.pdf"))
    # Income / tax chain (CA-1 golden values).
    expect(f, "540_form_2019", "28000.00", "540 line 13 federal AGI")
    expect(f, "540_form_2020", "0.00", "540 line 14 adjustments-subtractions")
    expect(f, "540_form_2021", "28000.00", "540 line 15")
    expect(f, "540_form_2023", "28000.00", "540 line 17 CA AGI")
    expect(f, "540_form_2024", "11412.00", "540 line 18 standard deduction")
    expect(f, "540_form_2025", "16588.00", "540 line 19 taxable income")
    expect(f, "540_form_2030", "166.00", "540 line 31 tax")
    expect(f, "540_form_2026 CB", "/Yes", "540 line 31 Tax Table box")
    expect_blank(f, "540_form_2027 CB", "540 line 31 Tax Rate Schedule box")
    expect(f, "540_form_2031", "1256.00", "540 line 32 exemption credits")
    expect(f, "540_form_2032", "0.00", "540 line 33")
    expect(f, "540_form_2036", "0.00", "540 line 35")
    expect(f, "540_form_3006", "0.00", "540 line 48")
    expect(f, "540_form_3010", "0.00", "540 line 64 total tax")
    # Payments: withholding + CalEITC + YCTC.
    expect(f, "540_form_3011", "200.00", "540 line 71 CA withholding")
    expect(f, "540_form_3015", "207.00", "540 line 75 CalEITC")
    expect(f, "540_form_3016", "1064.00", "540 line 76 YCTC")
    expect(f, "540_form_3018", "1471.00", "540 line 78 total payments")
    expect(f, "540_form_3027", "1471.00", "540 line 97 overpaid")
    expect_blank(f, "540_form_4005", "540 line 100 tax due")
    # Stated-zero / unmodeled fields stay blank.
    expect_blank(f, "540_form_3008", "540 line 62 BHST (stated zero)")
    expect_blank(f, "540_form_2018", "540 line 12 state wages (not engine-stated)")
    expect_blank(f, "540_form_1028", "540 county (not stored)")
    expect_blank(f, "540_form_5008", "540 direct-deposit routing (not stored)")
    expect_blank(f, "540_form_6001", "540 signature date (signer signs)")
    # Filing status radio: the form's literal MFJ on-state.
    expect(f, "540_form_1036 RB", MFJ_ON_STATE, "540 filing status")
    # Exemption counts: 2 personal, 2 dependents.
    expect(f, "540_form_1041", "2", "540 line 7 count")
    expect(f, "540_form_1042", "306.00", "540 line 7 amount")
    expect(f, "540_form_2015", "2", "540 line 10 count")
    expect(f, "540_form_2016", "950.00", "540 line 10 amount")
    expect(f, "540_form_2017", "1256.00", "540 line 11 amount")
    # Identity header: names split across the form's fields, DOBs in
    # the form's mm/dd/yyyy, spouse row filled for the joint return.
    expect(f, "540_form_1003", "Case", "540 taxpayer first name")
    expect(f, "540_form_1005", "Tester", "540 taxpayer last name")
    expect(f, "540_form_1008", "Spouse", "540 spouse first name")
    expect(f, "540_form_1024", "04/02/1993", "540 taxpayer DOB")
    expect(f, "540_form_1025", "08/20/1995", "540 spouse DOB")
    expect(f, "540_form_2001", "Case Tester", "540 Side 2 header name")
    # SSNs are inside the PDF (and only there).
    expect(f, "540_form_1007", "123456789", "540 taxpayer SSN in PDF")
    expect(f, "540_form_2005", "123456789", "540 dependent 1 SSN in PDF")
    expect(f, "540_form_2003", "Kid", "540 dependent 1 first name")
    expect(f, "540_form_2004", "One", "540 dependent 1 last name")
    expect(f, "540_form_2006", "Child", "540 dependent 1 relationship")
    blob = json.dumps(result) + open(os.path.join(out_dir, "manifest.json")).read()
    check(
        "123456789" not in blob and "987654321" not in blob,
        "ca-002: full SSNs appear only inside the filled PDFs, never in export output or manifest",
    )


def case_federal_only(bin_path, store, forms_dir, tmp):
    print("federal-only return: CA forms report not_applicable")
    rid = build_return(bin_path, store, FED_CASES, "001-single-w2-minimal")
    out_dir = os.path.join(tmp, "pkt-fed-only")
    export_packet(bin_path, store, rid, forms_dir, out_dir)
    statuses = manifest_statuses(out_dir)
    check(
        statuses["f540"]["status"] == "not_applicable",
        f"federal-only manifest: f540 is not_applicable (got {statuses['f540']['status']})",
    )
    check(
        statuses["f540ca"]["status"] == "not_applicable",
        f"federal-only manifest: f540ca is not_applicable (got {statuses['f540ca']['status']})",
    )
    check(
        not os.path.exists(os.path.join(out_dir, "f540.pdf")),
        "federal-only: no f540.pdf is written",
    )


def case_missing_template(bin_path, store, forms_dir, tmp):
    print("forms-dir without f540.pdf: Form 540 reports missing_template")
    rid = build_return(bin_path, store, CA_CASES, "ca-002-family-caleitc")
    sparse = os.path.join(tmp, "sparse-forms")
    os.makedirs(sparse)
    for name in os.listdir(forms_dir):
        if name != "f540.pdf":
            shutil.copy(os.path.join(forms_dir, name), os.path.join(sparse, name))
    out_dir = os.path.join(tmp, "pkt-ca-sparse")
    export_packet(bin_path, store, rid, sparse, out_dir)
    statuses = manifest_statuses(out_dir)
    check(
        statuses["f540"]["status"] == "missing_template",
        f"sparse manifest: f540 is missing_template (got {statuses['f540']['status']})",
    )
    check(
        statuses["f540ca"]["status"] == "not_applicable",
        "sparse manifest: f540ca stays not_applicable (it needs no template)",
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
        sys.exit(f"ca_packet_acceptance: tax binary not found at {bin_path}")
    for name in ("f1040.pdf", "f540.pdf"):
        if not os.path.exists(os.path.join(args.forms_dir, name)):
            sys.exit(
                f"ca_packet_acceptance: template {name} missing in {args.forms_dir}"
            )
    with tempfile.TemporaryDirectory() as tmp:
        store = os.path.join(tmp, "store")
        rid = build_return(bin_path, store, CA_CASES, "ca-002-family-caleitc")
        result = run_tax(bin_path, store, "return", "get", "--return", rid)
        check(result["readiness"]["status"] == "unsupported", "CA family: qualification is explicitly unsupported")
        with open(os.path.join(CA_CASES, "ca-002-family-caleitc", "correct.json")) as fh:
            expected = json.load(fh)
        for line, cents in expected["state_lines"].items():
            check(result["state"]["lines"][line].get("cents") == cents, f"CA family line {line} matches golden value")
        out_dir = os.path.join(tmp, "draft-ca")
        draft = run_tax(bin_path, store, "return", "export", "--return", rid, "--type", "packet", "--draft", "--forms-dir", args.forms_dir, "--out-dir", out_dir)
        check(draft["draft"] and draft["readiness"]["status"] == "unsupported", "CA family draft reports blockers")
        check(manifest_statuses(out_dir)["f540"]["status"] == "filled", "CA family draft includes Form 540")
        verify_packet(out_dir, draft, {"f540.pdf": ["28000.00"]})
        case_federal_only(bin_path, store, args.forms_dir, tmp)
        sparse = os.path.join(tmp, "ca-sparse-forms")
        os.makedirs(sparse)
        for name in os.listdir(args.forms_dir):
            if name != "f540.pdf":
                shutil.copy(os.path.join(args.forms_dir, name), os.path.join(sparse, name))
        sparse_out = os.path.join(tmp, "ca-sparse-draft")
        sparse_result = run_tax(bin_path, store, "return", "export", "--return", rid, "--type", "packet", "--draft", "--forms-dir", sparse, "--out-dir", sparse_out)
        sparse_manifest = verify_packet(sparse_out, sparse_result, {})
        sparse_status = {f["form"]: f["status"] for f in sparse_manifest["forms"]}
        check(sparse_status["f540"] == "missing_template", "CA draft explicitly reports missing Form 540 template")
        check(sparse_status["f540ca"] == "not_applicable", "CA draft retains Schedule CA applicability")
    if FAILURES:
        print(f"\nca_packet_acceptance: {len(FAILURES)} FAILURE(S)")
        return 1
    print("\nca_packet_acceptance: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
