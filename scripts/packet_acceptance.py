#!/usr/bin/env python3
"""Acceptance test for `tax return export --type packet`.

Builds stored returns from the golden benchmark inputs (cases 002, 005,
009, 010) through the real CLI, runs the packet export against the blank
TY2025 templates in spikes/pdf-fill/fixtures/, then reads the filled
PDFs back with pypdf and asserts the engine values landed in the
geometrically verified fields, and that manifest.json reports the right
status for every form. A fifth scenario runs case 002 against a
forms-dir holding only f1040.pdf and asserts the applicable schedules
come back as loud `missing_template` entries, never silent skips.

Usage:
    python3 scripts/packet_acceptance.py [--bin PATH] [--forms-dir DIR]

The `tax` binary defaults to $CARGO_TARGET_DIR/debug/tax, then
target/debug/tax. Requires pypdf (pip install pypdf).
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile

try:
    from pypdf import PdfReader
except ImportError:
    sys.exit("packet_acceptance: pypdf is required (pip install pypdf)")

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CASES = os.path.join(REPO, "benchmark", "cases", "f1040", "2025")

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


def build_return(bin_path, store, case):
    """Recreate a golden case's return through the CLI."""
    with open(os.path.join(CASES, case, "input.json")) as fh:
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
    if "identity" in data:
        with tempfile.NamedTemporaryFile(
            "w", suffix=".json", delete=False
        ) as fh:
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
    check(
        actual == expected,
        f"{label}: {qname.split('.')[-1]} == {expected!r} (got {actual!r})",
    )


def expect_blank(fields, qname, label):
    actual = fields.get(qname)
    check(
        actual in (None, "", "/Off"),
        f"{label}: {qname.split('.')[-1]} stays blank (got {actual!r})",
    )


def manifest_statuses(out_dir):
    with open(os.path.join(out_dir, "manifest.json")) as fh:
        manifest = json.load(fh)
    return {f["form"]: f for f in manifest["forms"]}


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


SD = "topmostSubform[0].Page1[0]."
SD2 = "topmostSubform[0].Page2[0]."
F89 = "topmostSubform[0].Page1[0].Table_Line1_Part1[0].Row1[0]."
F89B = "topmostSubform[0].Page2[0].Table_Line1_Part2[0].Row1[0]."


def case_002(bin_path, store, forms_dir, tmp):
    print("case 002-w2-1099s: Schedule D + Form 8949")
    rid = build_return(bin_path, store, "002-w2-1099s")
    out_dir = os.path.join(tmp, "pkt-002")
    result = export_packet(bin_path, store, rid, forms_dir, out_dir)
    statuses = manifest_statuses(out_dir)
    for form, want in [
        ("f1040", "filled"),
        ("f1040sd", "filled"),
        ("f8949", "filled"),
        ("f1040s1", "not_applicable"),
        ("f1040s2", "not_applicable"),
        ("f1040sc", "not_applicable"),
        ("f1040sse", "not_applicable"),
        ("f10408812", "not_applicable"),
        ("f1040s1a", "not_applicable"),
    ]:
        check(
            statuses[form]["status"] == want,
            f"002 manifest: {form} is {want} (got {statuses[form]['status']})",
        )
    check(
        result["schedules"]["schedule_d"]["combined_cents"] == 150000,
        "002 schedules view: schedule_d combined is $1,500",
    )

    sd = field_values(os.path.join(out_dir, "f1040sd.pdf"))
    row1b = SD + "Table_PartI[0].Row1b[0]."
    row8b = SD + "Table_PartII[0].Row8b[0]."
    expect(sd, row1b + "f1_7[0]", "8000.00", "Sch D 1b proceeds")
    expect(sd, row1b + "f1_8[0]", "6000.00", "Sch D 1b basis")
    expect(sd, row1b + "f1_10[0]", "2000.00", "Sch D 1b gain")
    expect(sd, SD + "f1_22[0]", "2000.00", "Sch D line 7")
    expect(sd, row8b + "f1_27[0]", "3000.00", "Sch D 8b proceeds")
    expect(sd, row8b + "f1_28[0]", "3500.00", "Sch D 8b basis")
    expect(sd, row8b + "f1_30[0]", "-500.00", "Sch D 8b gain")
    expect(sd, SD + "f1_43[0]", "-500.00", "Sch D line 15")
    expect(sd, SD2 + "f2_1[0]", "1500.00", "Sch D line 16")
    # Line 15 is a loss, so line 17 is "No"; line 22 is "Yes" (3a = $600).
    expect(sd, SD2 + "c2_1[1]", "/2", "Sch D line 17 No")
    expect(sd, SD2 + "c2_3[0]", "/1", "Sch D line 22 Yes")
    expect_blank(sd, SD + "Table_PartI[0].Row1a[0].f1_3[0]", "Sch D line 1a")

    f = field_values(os.path.join(out_dir, "f8949.pdf"))
    expect(f, "topmostSubform[0].Page1[0].c1_1[0]", "/1", "8949 Box A")
    expect(f, "topmostSubform[0].Page2[0].c2_1[0]", "/1", "8949 Box D")
    expect(f, F89 + "f1_03[0]", "100 sh EXAMPLE", "8949 ST row 1 description")
    expect(f, F89 + "f1_06[0]", "8000.00", "8949 ST row 1 proceeds")
    expect(f, F89 + "f1_07[0]", "6000.00", "8949 ST row 1 basis")
    expect(f, F89 + "f1_10[0]", "2000.00", "8949 ST row 1 gain")
    # The golden input carries no dates: columns (b)/(c) stay blank and
    # the manifest says so.
    expect_blank(f, F89 + "f1_04[0]", "8949 ST row 1 date acquired")
    expect_blank(f, F89 + "f1_05[0]", "8949 ST row 1 date sold")
    notes = " ".join(statuses["f8949"]["notes"])
    check("no date acquired" in notes, "002 manifest: 8949 notes report missing dates")
    expect(f, SD + "f1_91[0]", "8000.00", "8949 ST totals proceeds")
    expect(f, SD + "f1_95[0]", "2000.00", "8949 ST totals gain")
    expect(f, F89B + "f2_03[0]", "50 sh EXAMPLE", "8949 LT row 1 description")
    expect(f, F89B + "f2_10[0]", "-500.00", "8949 LT row 1 gain")
    expect(f, SD2.replace("Page2", "Page2") + "f2_95[0]", "-500.00", "8949 LT totals gain")


def case_005(bin_path, store, forms_dir, tmp):
    print("case 005-w2-nec: Schedules 1, 2, C, SE")
    rid = build_return(bin_path, store, "005-w2-nec")
    out_dir = os.path.join(tmp, "pkt-005")
    export_packet(bin_path, store, rid, forms_dir, out_dir)
    statuses = manifest_statuses(out_dir)
    for form in ["f1040s1", "f1040s2", "f1040sc", "f1040sse"]:
        check(
            statuses[form]["status"] == "filled",
            f"005 manifest: {form} is filled (got {statuses[form]['status']})",
        )
    for form in ["f1040sd", "f8949", "f10408812", "f1040s1a"]:
        check(
            statuses[form]["status"] == "not_applicable",
            f"005 manifest: {form} is not_applicable",
        )

    s1 = field_values(os.path.join(out_dir, "f1040s1.pdf"))
    expect(s1, SD + "f1_07[0]", "18000.00", "Sch 1 line 3 (Sch C net)")
    expect(s1, SD + "f1_38[0]", "18000.00", "Sch 1 line 10 total")
    expect(s1, SD2 + "f2_05[0]", "1271.66", "Sch 1 line 15 (half SE tax)")
    expect(s1, SD2 + "f2_30[0]", "1271.66", "Sch 1 line 26 total adjustments")

    s2 = field_values(os.path.join(out_dir, "f1040s2.pdf"))
    expect(s2, "form1[0].Page1[0].f1_15[0]", "2543.32", "Sch 2 line 4 (SE tax)")
    expect(s2, "form1[0].Page2[0].f2_24[0]", "2543.32", "Sch 2 line 21 total")

    sc = field_values(os.path.join(out_dir, "f1040sc.pdf"))
    expect(sc, SD + "f1_5[0]", "Example Client LLC", "Sch C business name")
    expect(sc, SD + "f1_10[0]", "20000.00", "Sch C line 1 gross receipts")
    expect(sc, SD + "f1_16[0]", "20000.00", "Sch C line 7 gross income")
    partv = SD2 + "PartVTable[0].Item1[0]."
    expect(sc, partv + "f2_16[0]", "2000.00", "Sch C Part V aggregate expenses")
    expect(sc, SD2 + "f2_33[0]", "2000.00", "Sch C line 48")
    expect(sc, SD + "Lines18-27[0].f1_39[0]", "2000.00", "Sch C line 27b")
    expect(sc, SD + "f1_41[0]", "2000.00", "Sch C line 28 total expenses")
    expect(sc, SD + "f1_42[0]", "18000.00", "Sch C line 29 tentative profit")
    expect(sc, SD + "f1_46[0]", "18000.00", "Sch C line 31 net profit")
    # The golden NEC entry stores no payer EIN: box D stays blank, noted.
    expect_blank(sc, SD + "DComb[0].f1_6[0]", "Sch C box D EIN")
    notes = " ".join(statuses["f1040sc"]["notes"])
    check("27b" in notes, "005 manifest: Sch C notes name the 27b aggregate treatment")

    se = field_values(os.path.join(out_dir, "f1040sse.pdf"))
    expect(se, SD + "f1_5[0]", "18000.00", "Sch SE line 2")
    expect(se, SD + "f1_6[0]", "18000.00", "Sch SE line 3")
    expect(se, SD + "f1_7[0]", "16623.00", "Sch SE line 4a (92.35%)")
    expect(se, SD + "f1_9[0]", "16623.00", "Sch SE line 4c")
    expect(se, SD + "f1_12[0]", "16623.00", "Sch SE line 6")
    expect(se, SD + "Line8a_ReadOrder[0].f1_14[0]", "50000.00", "Sch SE line 8a (W-2 proxy)")
    expect(se, SD + "f1_17[0]", "50000.00", "Sch SE line 8d")
    expect(se, SD + "f1_18[0]", "126100.00", "Sch SE line 9 (wage-base room)")
    expect(se, SD + "f1_19[0]", "2061.25", "Sch SE line 10 (SS part)")
    expect(se, SD + "f1_20[0]", "482.07", "Sch SE line 11 (Medicare part)")
    expect(se, SD + "f1_21[0]", "2543.32", "Sch SE line 12 (SE tax)")
    expect(se, SD + "f1_22[0]", "1271.66", "Sch SE line 13 (deductible half)")
    notes = " ".join(statuses["f1040sse"]["notes"])
    check("Box 1 proxy" in notes, "005 manifest: Sch SE notes flag the Box 1 proxy on line 8a")


def case_009(bin_path, store, forms_dir, tmp):
    print("case 009-eic-two-kids: Schedule 8812")
    rid = build_return(bin_path, store, "009-eic-two-kids")
    out_dir = os.path.join(tmp, "pkt-009")
    export_packet(bin_path, store, rid, forms_dir, out_dir)
    statuses = manifest_statuses(out_dir)
    check(
        statuses["f10408812"]["status"] == "filled",
        "009 manifest: f10408812 is filled",
    )
    s = field_values(os.path.join(out_dir, "f10408812.pdf"))
    expect(s, SD + "f1_3[0]", "38000.00", "8812 line 1 (AGI)")
    expect(s, SD + "f1_8[0]", "38000.00", "8812 line 3 (MAGI)")
    expect(s, SD + "f1_9[0]", "2", "8812 line 4 (CTC children count)")
    expect(s, SD + "f1_10[0]", "4400.00", "8812 line 5")
    expect(s, SD + "Line6ReadOrder[0].f1_11[0]", "0", "8812 line 6 (ODC count)")
    expect(s, SD + "f1_13[0]", "4400.00", "8812 line 8 (tentative credit)")
    expect(s, SD + "f1_14[0]", "200000.00", "8812 line 9 (threshold)")
    expect(s, SD + "f1_15[0]", "0.00", "8812 line 10")
    expect(s, SD + "f1_16[0]", "0.00", "8812 line 11 (phaseout reduction)")
    expect(s, SD + "c1_1[1]", "/2", "8812 line 12 Yes")
    expect(s, SD + "f1_17[0]", "4400.00", "8812 line 12 amount")
    expect(s, SD + "f1_18[0]", "2435.00", "8812 line 13 (credit limit = 1040 line 16)")
    expect(s, SD + "f1_19[0]", "2435.00", "8812 line 14 (nonrefundable credit)")
    expect(s, SD2 + "f2_2[0]", "1965.00", "8812 line 16a (unused credit)")
    expect(s, SD2 + "f2_3[0]", "2", "8812 line 16b count")
    expect(s, SD2 + "f2_4[0]", "3400.00", "8812 line 16b (ACTC cap)")
    expect(s, SD2 + "f2_5[0]", "1965.00", "8812 line 17")
    expect(s, SD2 + "f2_6[0]", "38000.00", "8812 line 18a (earned income)")
    expect(s, SD2 + "c2_1[1]", "/2", "8812 line 19 Yes")
    expect(s, SD2 + "f2_8[0]", "35500.00", "8812 line 19 amount")
    expect(s, SD2 + "f2_9[0]", "5325.00", "8812 line 20 (15%)")
    expect(s, SD2 + "c2_2[0]", "/1", "8812 16b-under-$5,100 No (skip Part II-B)")
    expect(s, SD2 + "f2_16[0]", "1965.00", "8812 line 27 (ACTC = 1040 line 28)")


def case_010(bin_path, store, forms_dir, tmp):
    print("case 010-senior-phaseout: Schedule 1-A")
    rid = build_return(bin_path, store, "010-senior-phaseout")
    out_dir = os.path.join(tmp, "pkt-010")
    export_packet(bin_path, store, rid, forms_dir, out_dir)
    statuses = manifest_statuses(out_dir)
    check(
        statuses["f1040s1a"]["status"] == "filled",
        "010 manifest: f1040s1a is filled",
    )
    a = field_values(os.path.join(out_dir, "f1040s1a.pdf"))
    p1 = "form1[0].Page1[0]."
    p2 = "form1[0].Page2[0]."
    expect(a, p1 + "f1_01[0]", "Spike Tester", "1-A header name")
    expect(a, p1 + "f1_02[0]", "123456789", "1-A header SSN (digits in PDF)")
    expect(a, p1 + "f1_03[0]", "200000.00", "1-A line 1 (AGI)")
    expect(a, p1 + "f1_09[0]", "200000.00", "1-A line 3 (MAGI)")
    expect(a, p2 + "f2_15[0]", "200000.00", "1-A line 31")
    expect(a, p2 + "f2_16[0]", "150000.00", "1-A line 32 (MFJ threshold)")
    expect(a, p2 + "f2_17[0]", "50000.00", "1-A line 33 (excess)")
    expect(a, p2 + "f2_18[0]", "3000.00", "1-A line 34 (6% of excess)")
    expect(a, p2 + "f2_19[0]", "3000.00", "1-A line 35 (per-person amount)")
    expect(a, p2 + "f2_20[0]", "3000.00", "1-A line 36a (taxpayer)")
    expect(a, p2 + "f2_21[0]", "3000.00", "1-A line 36b (spouse)")
    expect(a, p2 + "f2_22[0]", "6000.00", "1-A line 37 (senior deduction)")
    expect(a, p2 + "f2_23[0]", "6000.00", "1-A line 38 (total = 1040 line 13b)")


def case_missing_templates(bin_path, store, tmp):
    print("scenario: missing templates are loud manifest entries")
    sparse = os.path.join(tmp, "sparse-forms")
    os.makedirs(sparse)
    src = os.path.join(REPO, "spikes", "pdf-fill", "fixtures", "f1040.pdf")
    with open(src, "rb") as fh, open(os.path.join(sparse, "f1040.pdf"), "wb") as out:
        out.write(fh.read())
    rid = build_return(bin_path, store, "002-w2-1099s")
    out_dir = os.path.join(tmp, "pkt-sparse")
    export_packet(bin_path, store, rid, sparse, out_dir)
    statuses = manifest_statuses(out_dir)
    check(statuses["f1040"]["status"] == "filled", "sparse: f1040 still filled")
    for form in ["f1040sd", "f8949"]:
        check(
            statuses[form]["status"] == "missing_template",
            f"sparse: {form} is missing_template (got {statuses[form]['status']})",
        )
        check(
            statuses[form]["file"] is None,
            f"sparse: {form} manifest file is null",
        )
        check(
            not os.path.exists(os.path.join(out_dir, f"{form}.pdf")),
            f"sparse: no {form}.pdf written",
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
        for cand in [
            os.path.join(os.environ.get("CARGO_TARGET_DIR", ""), "debug", "tax"),
            os.path.join(REPO, "target", "debug", "tax"),
        ]:
            if cand and os.path.exists(cand):
                bin_path = cand
                break
    if bin_path is None:
        sys.exit("packet_acceptance: tax binary not found (pass --bin)")
    needed = [
        "f1040.pdf",
        "f1040sd.pdf",
        "f8949.pdf",
        "f1040s1.pdf",
        "f1040s2.pdf",
        "f1040sc.pdf",
        "f1040sse.pdf",
        "f10408812.pdf",
        "f1040s1a.pdf",
    ]
    missing = [n for n in needed if not os.path.exists(os.path.join(args.forms_dir, n))]
    if missing:
        sys.exit(f"packet_acceptance: templates missing in {args.forms_dir}: {missing}")

    with tempfile.TemporaryDirectory() as tmp:
        case_002(bin_path, os.path.join(tmp, "store"), args.forms_dir, tmp)
        case_005(bin_path, os.path.join(tmp, "store"), args.forms_dir, tmp)
        case_009(bin_path, os.path.join(tmp, "store"), args.forms_dir, tmp)
        case_010(bin_path, os.path.join(tmp, "store"), args.forms_dir, tmp)
        case_missing_templates(bin_path, os.path.join(tmp, "store"), tmp)

    if FAILURES:
        print(f"\npacket_acceptance: {len(FAILURES)} FAILURE(S)")
        return 1
    print("\npacket_acceptance: all checks pass")
    return 0


if __name__ == "__main__":
    sys.exit(main())
