#!/usr/bin/env python3
"""Acceptance test for `tax return export --type mef`.

Builds stored returns from the golden benchmark inputs (cases 009 and
005, with a synthetic identity added through `return set-identity` —
neither case stores one; cases 002 and 010 for Schedule D/8949 and
Schedule 1-A validation coverage; plus one hand-built stepchild
scenario that exercises Schedule EIC) through the real CLI, generates
the MeF XML, and asserts:

- the document is well-formed, with the Return / ReturnHeader /
  ReturnData structure and a documentCnt that matches the children;
- IRS1040 carries the engine's values for the key lines (AGI, taxable
  income, tax, EIC and ACTC for 009; the SE-tax lines for 005 via
  IRS1040ScheduleSE / IRS1040Schedule2), and the dependents are
  listed in DependentDetail;
- the stdout report's honesty contract: status "generated",
  validation.xsd_validated == false (the binary performs no XSD
  validation), validation.schema_version == "2025v5.2", the
  not-transmissible disclosure is present, and NO full SSN appears
  anywhere in the report (the XML file itself does carry it — it is
  the filing artifact);
- the actual validation tier: every emitted form document is
  validated against the official TY2025v5.2 per-form XSDs vendored
  under scripts/mef-schemas/2025v5.2/. The per-form schemas include
  `../../../Common/efileTypes.xsd`; the harness resolves that to the
  authentic TY2015 Common file vendored under
  scripts/mef-schemas/common/ (the TY2025 Common file is distributed
  only through the IRS Secure Object Repository), plus three
  harness-supplied leaf type definitions (MilesNNType,
  SpecialConditionTextType, Text2Type) that exist in the TY2025
  package's Common but not in TY2015's, and that gate no emitted
  element. The Return/ReturnHeader envelope is NOT XSD-validated
  (its schemas are not publicly available) — this script asserts the
  report says exactly that.

Usage:
    python3 scripts/mef_acceptance.py [--bin PATH]

The `tax` binary defaults to $CARGO_TARGET_DIR/debug/tax, then
target/debug/tax. Requires the `xmlschema` package (pip install
xmlschema) — the same hard-requirement pattern packet_acceptance.py
uses for pypdf.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

# The xmlschema package may live in the workspace-local pip target.
for cand in [os.path.expanduser("~/pylibs-spike")]:
    if os.path.isdir(cand) and cand not in sys.path:
        sys.path.insert(0, cand)

try:
    import xmlschema
except ImportError:
    sys.exit("mef_acceptance: the xmlschema package is required (pip install xmlschema)")

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CASES = os.path.join(REPO, "benchmark", "cases", "f1040", "2025")
SCHEMAS_2025 = os.path.join(REPO, "scripts", "mef-schemas", "2025v5.2")
COMMON_2015 = os.path.join(REPO, "scripts", "mef-schemas", "common", "efileTypes.xsd")
NS = "http://www.irs.gov/efile"
ET.register_namespace("", NS)

FAILURES = []

# A synthetic identity for cases that store none (the same shape as
# golden case 010's). Synthetic data only — never a real person.
SYNTHETIC_IDENTITY = {
    "first_name": "Spike",
    "last_name": "Tester",
    "ssn": "123-45-6789",
    "date_of_birth": "1990-05-01",
    "occupation": "Engineer",
    "address": {
        "street": "1 Main St",
        "city": "Springfield",
        "state": "IL",
        "zip": "62701",
    },
}
PRIMARY_SSN = "123456789"


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


def build_return(bin_path, store, case, add_identity=False):
    """Recreate a golden case's return through the CLI (the packet
    acceptance construction), optionally adding the synthetic
    identity MeF export requires."""
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
    identity = data.get("identity")
    if identity is None and add_identity:
        identity = SYNTHETIC_IDENTITY
    if identity is not None:
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
            json.dump(identity, fh)
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
    return rid, data


def export_mef(bin_path, store, rid, out_path):
    return run_tax(
        bin_path,
        store,
        "return",
        "export",
        "--return",
        rid,
        "--type",
        "mef",
        "--out",
        out_path,
    )


def q(tag):
    return f"{{{NS}}}{tag}"


def doc_element(root, name):
    for child in root.find(q("ReturnData")):
        if child.tag == q(name):
            return child
    return None


def amount(doc, elem):
    el = doc.find(q(elem))
    return None if el is None else int(el.text)


def check_report(report, label):
    check(report["status"] == "generated", f"{label}: report status is 'generated'")
    validation = report["validation"]
    check(
        validation["xsd_validated"] is False,
        f"{label}: report honestly claims xsd_validated=false for the export itself",
    )
    check(
        validation["schema_version"] == "2025v5.2",
        f"{label}: report schema_version is 2025v5.2",
    )
    notes = "\n".join(validation["notes"])
    check(
        "CANNOT be transmitted" in notes,
        f"{label}: report notes carry the not-transmissible disclosure",
    )
    check(
        "NOT XSD-validated" in notes,
        f"{label}: report notes disclose the envelope is not XSD-validated",
    )
    check(
        PRIMARY_SSN not in json.dumps(report),
        f"{label}: no full SSN anywhere in the stdout report",
    )
    check(
        report["document_count"] == len(report["documents"]),
        f"{label}: document_count matches the documents list",
    )


def check_envelope(root, report, label):
    check(root.tag == q("Return"), f"{label}: root element is Return")
    check(
        root.get("returnVersion") == "2025v5.2",
        f"{label}: returnVersion is 2025v5.2",
    )
    header = root.find(q("ReturnHeader"))
    check(header is not None, f"{label}: ReturnHeader present")
    check(
        header.find(q("ReturnTypeCd")).text == "1040",
        f"{label}: ReturnTypeCd is 1040",
    )
    filer = header.find(q("Filer"))
    check(
        filer.find(q("PrimarySSN")).text == PRIMARY_SSN,
        f"{label}: header Filer carries the taxpayer SSN (the filing artifact does)",
    )
    data = root.find(q("ReturnData"))
    children = list(data)
    check(
        int(data.get("documentCnt")) == len(children),
        f"{label}: ReturnData documentCnt matches its children",
    )
    names = [c.tag.replace(q(""), "") for c in children]
    check(
        names == report["documents"],
        f"{label}: ReturnData order matches the report's documents list",
    )
    ids = [c.get("documentId") for c in children]
    check(
        ids == [str(i + 1) for i in range(len(children))],
        f"{label}: documentIds are sequential from 1",
    )


# ---------------------------------------------------------------------------
# XSD validation harness (the Tier A check)
# ---------------------------------------------------------------------------

HARNESS_TYPES = """
  <!-- Harness-supplied definitions: leaf types referenced by the
       TY2025 forms but absent from the TY2015 Common file (they were
       added to Common after 2015). None of them gates an element the
       exporter emits; they exist so the schema compiles. -->
  <xsd:simpleType name="MilesNNType">
    <xsd:restriction base="xsd:nonNegativeInteger">
      <xsd:totalDigits value="7"/>
    </xsd:restriction>
  </xsd:simpleType>
  <xsd:simpleType name="SpecialConditionTextType">
    <xsd:restriction base="xsd:string"/>
  </xsd:simpleType>
  <xsd:simpleType name="Text2Type">
    <xsd:restriction base="xsd:string"/>
  </xsd:simpleType>
</xsd:schema>
"""


def build_harness(tmp):
    """Lay out the schema package so the per-form XSDs' include of
    ../../../Common/efileTypes.xsd resolves to the (patched) Common
    copy: <tmp>/schemas/IndividualIncomeTax/Ind1040/*.xsd and
    <tmp>/Common/efileTypes.xsd."""
    forms_dir = os.path.join(tmp, "schemas", "IndividualIncomeTax", "Ind1040")
    os.makedirs(forms_dir)
    for name in os.listdir(SCHEMAS_2025):
        if name.endswith(".xsd"):
            shutil.copy(os.path.join(SCHEMAS_2025, name), os.path.join(forms_dir, name))
    common_dir = os.path.join(tmp, "Common")
    os.makedirs(common_dir)
    with open(COMMON_2015) as fh:
        common = fh.read()
    assert "</xsd:schema>" in common
    patched = common.replace("</xsd:schema>", HARNESS_TYPES, 1)
    with open(os.path.join(common_dir, "efileTypes.xsd"), "w") as fh:
        fh.write(patched)
    return forms_dir


def validate_documents(root, forms_dir, label):
    """Validate each emitted form document against its official
    TY2025v5.2 XSD. Returns nothing; records checks."""
    data = root.find(q("ReturnData"))
    for child in data:
        name = child.tag.replace(q(""), "")
        xsd_path = os.path.join(forms_dir, f"{name}.xsd")
        check(os.path.exists(xsd_path), f"{label}: official XSD present for {name}")
        schema = xmlschema.XMLSchema(xsd_path)
        standalone = ET.tostring(child, encoding="unicode")
        errors = list(schema.iter_errors(standalone))
        check(
            not errors,
            f"{label}: {name} validates against the TY2025v5.2 XSD"
            + ("" if not errors else f" — first error: {errors[0].message}"),
        )


# ---------------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------------


def case_009(bin_path, store, tmp, forms_dir):
    print("case 009-eic-two-kids: IRS1040 + Schedule 8812 (+ EIC honesty)")
    rid, data = build_return(bin_path, store, "009-eic-two-kids", add_identity=True)
    out_path = os.path.join(tmp, "mef-009.xml")
    report = export_mef(bin_path, store, rid, out_path)
    check_report(report, "009")
    check(
        report["documents"] == ["IRS1040", "IRS1040Schedule8812"],
        f"009: documents are IRS1040 + IRS1040Schedule8812 (got {report['documents']})",
    )
    notes = "\n".join(report["validation"]["notes"])
    check(
        "Schedule EIC is not emitted" in notes,
        "009: report notes disclose the Schedule EIC omission (relationship enum)",
    )

    root = ET.parse(out_path).getroot()
    check_envelope(root, report, "009")
    f1040 = doc_element(root, "IRS1040")
    for elem, want in [
        ("AdjustedGrossIncomeAmt", 38000),
        ("TaxableIncomeAmt", 22250),
        ("TaxAmt", 2435),
        ("EarnedIncomeCreditAmt", 4061),
        ("AdditionalChildTaxCreditAmt", 1965),
        ("RefundableCreditsAmt", 6026),
        ("TotalPaymentsAmt", 8526),
        ("RefundAmt", 8526),
    ]:
        check(amount(f1040, elem) == want, f"009: IRS1040/{elem} == {want} (got {amount(f1040, elem)})")
    details = f1040.findall(q("DependentDetail"))
    check(len(details) == 2, f"009: two DependentDetail rows (got {len(details)})")
    want_deps = sorted(
        (d["name"].split()[0], d["ssn"].replace("-", "")) for d in data["dependents"]
    )
    got_deps = sorted(
        (d.find(q("DependentFirstNm")).text, d.find(q("DependentSSN")).text)
        for d in details
    )
    check(got_deps == want_deps, f"009: DependentDetail names/SSNs match the input ({got_deps})")
    check(
        all(
            d.find(q("EligibleForChildTaxCreditInd")) is not None for d in details
        ),
        "009: both dependents carry EligibleForChildTaxCreditInd",
    )
    s8812 = doc_element(root, "IRS1040Schedule8812")
    for elem, want in [
        ("AdjustedGrossIncomeAmt", 38000),
        ("QlfyChildUnderAgeSSNLimtAmt", 4400),
        ("InitialCTCODCAmt", 4400),
        ("CTCODCAmt", 2435),
        ("AdditionalChildTaxCreditAmt", 1965),
    ]:
        check(amount(s8812, elem) == want, f"009: IRS1040Schedule8812/{elem} == {want} (got {amount(s8812, elem)})")
    check(
        s8812.find(q("FilingStatusThresholdCd")).text == "200000",
        "009: IRS1040Schedule8812/FilingStatusThresholdCd == 200000",
    )
    check(
        s8812.find(q("CTCODCOverAGILimitInd")).text == "false",
        "009: IRS1040Schedule8812/CTCODCOverAGILimitInd == false",
    )
    validate_documents(root, forms_dir, "009")


def case_005(bin_path, store, tmp, forms_dir):
    print("case 005-w2-nec: Schedules 1, 2, C, SE (whole-dollar rounding)")
    rid, _ = build_return(bin_path, store, "005-w2-nec", add_identity=True)
    out_path = os.path.join(tmp, "mef-005.xml")
    report = export_mef(bin_path, store, rid, out_path)
    check_report(report, "005")
    check(
        report["documents"]
        == [
            "IRS1040",
            "IRS1040Schedule1",
            "IRS1040Schedule2",
            "IRS1040ScheduleC",
            "IRS1040ScheduleSE",
        ],
        f"005: documents are 1040 + Sch 1/2/C/SE (got {report['documents']})",
    )
    notes = "\n".join(report["validation"]["notes"])
    check(
        "IRS1040ScheduleSE/SelfEmploymentTaxAmt" in notes,
        "005: report notes list the rounded SE tax field",
    )

    root = ET.parse(out_path).getroot()
    check_envelope(root, report, "005")
    f1040 = doc_element(root, "IRS1040")
    for elem, want in [
        ("AdjustedGrossIncomeAmt", 66728),  # 66,728.34 rounded
        ("TotalTaxAmt", 8672),  # 8,672.32 rounded
        ("OwedAmt", 4672),
    ]:
        check(amount(f1040, elem) == want, f"005: IRS1040/{elem} == {want} (got {amount(f1040, elem)})")
    se = doc_element(root, "IRS1040ScheduleSE")
    for elem, want in [
        ("NetNonFarmProfitLossAmt", 18000),
        ("MinimumProfitForSETaxAmt", 16623),
        ("SSTWagesRRTCompAmt", 50000),
        ("SelfEmploymentTaxAmt", 2543),  # 2,543.32 rounded
        ("DeductibleSelfEmploymentTaxAmt", 1272),  # 1,271.66 rounded half-up
    ]:
        check(amount(se, elem) == want, f"005: IRS1040ScheduleSE/{elem} == {want} (got {amount(se, elem)})")
    s2 = doc_element(root, "IRS1040Schedule2")
    check(
        amount(s2, "SelfEmploymentTaxAmt") == 2543,
        "005: IRS1040Schedule2/SelfEmploymentTaxAmt == 2543",
    )
    s1 = doc_element(root, "IRS1040Schedule1")
    check(
        amount(s1, "BusinessIncomeLossAmt") == 18000,
        "005: IRS1040Schedule1/BusinessIncomeLossAmt == 18000",
    )
    sc = doc_element(root, "IRS1040ScheduleC")
    check(
        amount(sc, "NetProfitOrLossAmt") == 18000,
        "005: IRS1040ScheduleC/NetProfitOrLossAmt == 18000",
    )
    validate_documents(root, forms_dir, "005")


def case_002_and_010_validation(bin_path, store, tmp, forms_dir):
    print("cases 002/010: Schedule D + 8949, Schedule 1-A (validation coverage)")
    for case, add_identity in [("002-w2-1099s", True), ("010-senior-phaseout", False)]:
        rid, _ = build_return(bin_path, store, case, add_identity=add_identity)
        out_path = os.path.join(tmp, f"mef-{case}.xml")
        report = export_mef(bin_path, store, rid, out_path)
        check_report(report, case)
        root = ET.parse(out_path).getroot()
        check_envelope(root, report, case)
        validate_documents(root, forms_dir, case)


def case_eic_stepchild(bin_path, store, tmp, forms_dir):
    print("scenario: stepchild dependent — Schedule EIC is emitted")
    created = run_tax(
        bin_path,
        store,
        "return",
        "create",
        "--status",
        "single",
        "--interest-cents",
        "0",
        "--dividends-cents",
        "0",
    )
    rid = created["returnId"]
    run_tax(
        bin_path,
        store,
        "form",
        "add",
        "--return",
        rid,
        "--node-type",
        "w2",
        "--data",
        json.dumps(
            {
                "employer_ein": "12-3456789",
                "wages": "3000000",
                "federal_withholding": "100000",
            }
        ),
    )
    run_tax(
        bin_path,
        store,
        "form",
        "add",
        "--return",
        rid,
        "--node-type",
        "dependent",
        "--data",
        json.dumps(
            {
                "name": "Kid Step",
                "ssn": "123456789",
                "relationship": "stepchild",
                "age": 10,
                "months_lived_with_taxpayer": 12,
            }
        ),
    )
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
        json.dump(SYNTHETIC_IDENTITY, fh)
        identity_file = fh.name
    run_tax(
        bin_path, store, "return", "set-identity", "--return", rid, "--file", identity_file
    )
    out_path = os.path.join(tmp, "mef-eic.xml")
    report = export_mef(bin_path, store, rid, out_path)
    check_report(report, "eic-stepchild")
    check(
        "IRS1040ScheduleEIC" in report["documents"],
        f"eic-stepchild: Schedule EIC emitted (got {report['documents']})",
    )
    root = ET.parse(out_path).getroot()
    check_envelope(root, report, "eic-stepchild")
    eic = doc_element(root, "IRS1040ScheduleEIC")
    info = eic.find(q("QualifyingChildInformation"))
    check(
        info.find(q("ChildRelationshipCd")).text == "STEPCHILD",
        "eic-stepchild: ChildRelationshipCd == STEPCHILD",
    )
    check(
        info.find(q("ChildBirthYr")).text == "2015",
        "eic-stepchild: ChildBirthYr == 2015 (2025 - age 10)",
    )
    check(
        info.find(q("MonthsChildLivedWithYouCnt")).text == "12",
        "eic-stepchild: MonthsChildLivedWithYouCnt == 12",
    )
    validate_documents(root, forms_dir, "eic-stepchild")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=None)
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
        sys.exit("mef_acceptance: tax binary not found (pass --bin)")

    with tempfile.TemporaryDirectory() as tmp:
        forms_dir = build_harness(tmp)
        case_009(bin_path, os.path.join(tmp, "store"), tmp, forms_dir)
        case_005(bin_path, os.path.join(tmp, "store"), tmp, forms_dir)
        case_002_and_010_validation(bin_path, os.path.join(tmp, "store"), tmp, forms_dir)
        case_eic_stepchild(bin_path, os.path.join(tmp, "store"), tmp, forms_dir)

    if FAILURES:
        print(f"\nmef_acceptance: {len(FAILURES)} FAILURE(S)")
        return 1
    print("\nmef_acceptance: all checks pass")
    return 0


if __name__ == "__main__":
    sys.exit(main())
