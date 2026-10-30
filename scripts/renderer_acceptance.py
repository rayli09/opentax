#!/usr/bin/env python3
"""Test-only renderer readback using synthetic golden fixtures, below CLI readiness.

The Rust test writes unmarked PDFs and MeF XML only to a private temporary
folder; no production command bypasses readiness. Requires pypdf, xmlschema,
and 2025 form templates. Normal CLI blocking and marked drafts are tested by
the separate acceptance scripts.
"""
import argparse
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path
import packet_acceptance as packet
import ca_packet_acceptance as ca
import nra_packet_acceptance as nra
import mef_acceptance as mef

ROOT = Path(__file__).resolve().parent.parent


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--forms-dir', required=True)
    args = ap.parse_args()
    with tempfile.TemporaryDirectory(prefix='opentax-renderer-acceptance-') as tmp:
        root = Path(tmp)/'fixtures'
        root.mkdir()
        env = dict(os.environ, OPENTAX_RENDER_TEST_OUTPUT=str(root), OPENTAX_RENDER_TEST_FORMS=str(Path(args.forms_dir).resolve()))
        subprocess.run(['cargo', 'test', '-p', 'tax-cli', 'render_mapping_fixtures', '--', '--nocapture'], cwd=ROOT, env=env, check=True)
        def copy_packet(_bin, _store, rid, _forms, out):
            src = root/rid
            shutil.copytree(src, out)
            return json.loads((src/'result.json').read_text())
        packet.build_return = lambda _bin, _store, case: case
        packet.export_packet = copy_packet
        for case in (packet.case_002, packet.case_005, packet.case_009):
            case(None, None, args.forms_dir, tmp)
        ca.build_return = lambda _bin, _store, _cases, case: case
        ca.export_packet = copy_packet
        ca.case_ca_002(None, None, args.forms_dir, tmp)
        nra.build_nr002 = lambda _bin, _store, _tmp: 'nr-002-cn-split-1042s'
        nra.export_packet = copy_packet
        nra.case_nr002_packet(None, None, args.forms_dir, tmp)
        def build_mef_fixture(_bin, _store, case, add_identity=False):
            data = json.loads((ROOT/'benchmark/cases/f1040/2025'/case/'input.json').read_text())
            return case, data
        def export_mef_fixture(_bin, _store, rid, out):
            src = root/rid
            shutil.copyfile(src/'mef.xml', out)
            return json.loads((src/'mef-result.json').read_text())
        mef.build_return = build_mef_fixture
        mef.export_mef = export_mef_fixture
        forms = mef.build_harness(tmp)
        mef.case_005(None, None, tmp, forms)
        mef.case_009(None, None, tmp, forms)
        # Preserve the remaining original field/schema assertions using the
        # same unmarked synthetic outputs produced only by the Rust test.
        sparse_ca = ca.manifest_statuses(root/'ca-sparse')
        ca.check(sparse_ca['f540']['status'] == 'missing_template', 'CA sparse: missing Form 540 template is explicit')
        ca.check(sparse_ca['f540ca']['status'] == 'not_applicable', 'CA sparse: Schedule CA remains not applicable')
        sparse_nr = nra.manifest_statuses(root/'nr-sparse')
        for form in ('f1040soi', 'f8843'):
            nra.check(sparse_nr[form]['status'] == 'missing_template', f'NR sparse: {form} missing template is explicit')
        nr_single = nra.field_values(root/'nr-single.pdf')
        nra.expect(nr_single, nra.P1+'f1_42[0]', '3000.00', 'single NR line 1a')
        nra.expect(nr_single, nra.P2+'f2_36[0]', '197.00', 'single NR refund')
        no_treaty = nra.manifest_statuses(root/'nr-no-treaty')
        nra.check(no_treaty['f1040soi']['status'] == 'not_applicable', 'no treaty: Schedule OI not applicable')
        nra.check('no treaty claim' in ' '.join(no_treaty['f1040soi']['notes']), 'no treaty: reason is explicit')
        nra.check(no_treaty['f8843']['status'] == 'filled', 'no treaty: Form 8843 filled')
        nra.check('no Form 8843 facts' in ' '.join(no_treaty['f8843']['notes']), 'no treaty: missing facts explicit')
        f8843 = nra.field_values(root/'nr-no-treaty/f8843.pdf')
        nra.expect(f8843, nra.S1+'f1_09[0]', 'F-1', 'no treaty visa')
        nra.expect(f8843, nra.S1+'f1_11[0]', 'DE', 'no treaty citizenship')
        nra.expect(f8843, nra.S1+'f1_33[0]', 'F-1', 'no treaty year 2024')
        nra.expect_blank(f8843, nra.S1+'f1_32[0]', 'no treaty year 2023')
        nra.expect_blank(f8843, nra.S1+'f1_14[0]', 'no treaty missing days')
        import xml.etree.ElementTree as ET
        root002 = ET.parse(root/'002-w2-1099s/mef.xml').getroot()
        mef.validate_documents(root002, forms, '002')
        step = ET.parse(root/'stepchild/mef.xml').getroot()
        report = json.loads((root/'stepchild/result.json').read_text())
        mef.check_envelope(step, report, 'stepchild')
        eic = mef.doc_element(step, 'IRS1040ScheduleEIC')
        info = eic.find(mef.q('QualifyingChildInformation'))
        mef.check(info.find(mef.q('ChildRelationshipCd')).text == 'STEPCHILD', 'stepchild relationship in MeF')
        mef.check(info.find(mef.q('ChildBirthYr')).text == '2015', 'stepchild birth year in MeF')
        mef.check(info.find(mef.q('MonthsChildLivedWithYouCnt')).text == '12', 'stepchild months in MeF')
        mef.validate_documents(step, forms, 'stepchild')
        failures = packet.FAILURES + ca.FAILURES + nra.FAILURES + mef.FAILURES
        if failures:
            raise AssertionError(f'{len(failures)} renderer assertions failed')
    print('renderer acceptance: all preserved field and schema checks passed')


if __name__ == '__main__':
    main()
