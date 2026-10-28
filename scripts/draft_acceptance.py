#!/usr/bin/env python3
"""Exercise explicit rasterized PDF drafts with synthetic data and official 2025 templates.
Requires pypdf, Ghostscript and Tesseract; use --forms-dir for the templates.
"""
import argparse
import json
import os
import subprocess
import tempfile
from pathlib import Path
from pypdf import PdfReader

ROOT = Path(__file__).resolve().parent.parent

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--bin', default=str(ROOT / 'target/debug/tax'))
    ap.add_argument('--forms-dir', required=True)
    args = ap.parse_args()
    with tempfile.TemporaryDirectory(prefix='opentax-draft-acceptance-') as tmp:
        tmp = Path(tmp)
        def run(*argv, ok=True, env=None):
            p = subprocess.run([args.bin, '--output', 'json', '--store', str(tmp / 'store'), *map(str, argv)], capture_output=True, text=True, env=env)
            data = json.loads(p.stdout if p.returncode == 0 else p.stderr)
            assert (p.returncode == 0) == ok, data
            return data
        def create():
            return run('return', 'create', '--year', '2025', '--status', 'single', '--interest-cents', '0', '--dividends-cents', '0')['returnId']
        def add(rid, kind, data):
            run('form', 'add', '--return', rid, '--node-type', kind, '--data', json.dumps(data))
        def inspect(pdf):
            reader = PdfReader(pdf)
            assert reader.trailer['/Root'].get('/AcroForm') is None
            assert len(reader.pages) > 0
            with tempfile.TemporaryDirectory(dir=tmp) as image_dir:
                subprocess.run(['gs', '-q', '-dSAFER', '-dBATCH', '-dNOPAUSE', '-sDEVICE=png16m', '-r150', f'-sOutputFile={image_dir}/page-%d.png', str(pdf)], check=True)
                for number, page in enumerate(reader.pages, 1):
                    assert 'DRAFT - NOT FOR FILING' in page.extract_text()
                    assert not page.get('/Annots')
                    raster = (Path(image_dir)/f'page-{number}.png').read_bytes()
                    text = subprocess.run(['tesseract', 'stdin', 'stdout'], input=raster, capture_output=True, check=True).stdout.decode(errors='replace').upper()
                    assert 'DRAFT' in text and 'NOT FOR FILING' in text, (pdf, number, text)
        rid = create()
        add(rid, 'w2', {'employer_ein':'12-3456789', 'wages':'5000000', 'federal_withholding':'400000'})
        result = run('return', 'export', '--return', rid, '--type', 'pdf', '--template', Path(args.forms_dir)/'f1040.pdf', '--out', tmp/'draft.pdf', ok=False)
        assert result['code'] == 'return_not_ready'
        empty_path = tmp/'empty-path'
        empty_path.mkdir()
        no_gs = dict(os.environ, PATH=str(empty_path))
        failed = run('return', 'export', '--return', rid, '--type', 'pdf', '--draft', '--template', Path(args.forms_dir)/'f1040.pdf', '--out', tmp/'missing-gs.pdf', ok=False, env=no_gs)
        assert failed['code'] == 'draft_failed'
        assert not (tmp/'missing-gs.pdf').exists() and not (tmp/'missing-gs.pdf.manifest.json').exists()
        fake = empty_path/'gs'
        fake.write_text('#!/bin/sh\necho noisy-stdout\necho noisy-stderr >&2\nexit 17\n')
        fake.chmod(0o755)
        failed = run('return', 'export', '--return', rid, '--type', 'packet', '--draft', '--forms-dir', args.forms_dir, '--out-dir', tmp/'failed-packet', ok=False, env=no_gs)
        assert failed['code'] == 'draft_failed' and not (tmp/'failed-packet').exists()
        result = run('return', 'export', '--return', rid, '--type', 'pdf', '--draft', '--template', Path(args.forms_dir)/'f1040.pdf', '--out', tmp/'draft.pdf')
        assert result['draft'] and result['readiness']['status'] == 'needs_input'
        inspect(tmp/'draft.pdf')
        manifest = json.loads((tmp/'draft.pdf.manifest.json').read_text())
        assert manifest['draft'] and manifest['readiness']['blockers']
        # Independently render the final flattened PDF, then OCR the synthetic
        # W-2 figure. This checks actual rendered pixels, not just a field /V.
        subprocess.run(['gs', '-q', '-dSAFER', '-dBATCH', '-dNOPAUSE', '-sDEVICE=png16m', '-r150', '-dFirstPage=1', '-dLastPage=1', f'-sOutputFile={tmp}/page.png', str(tmp/'draft.pdf')], check=True)
        ocr = subprocess.run(['tesseract', 'stdin', 'stdout'], input=(tmp/'page.png').read_bytes(), capture_output=True, check=True).stdout.decode(errors='replace')
        assert '50000.00' in ocr.lower().replace('s', '5'), ocr
        for answer in (False, True):
            identity = {'first_name':'Synthetic', 'last_name':'Example', 'ssn':'123456789', 'date_of_birth':'1980-01-01', 'blind':False, 'can_be_claimed_as_dependent':False, 'digital_assets':answer, 'address':{'street':'1 Main St', 'city':'Example', 'state':'CA', 'zip':'90001'}}
            identity_path = tmp/f'identity-{answer}.json'
            identity_path.write_text(json.dumps(identity))
            run('return', 'set-identity', '--return', rid, '--file', identity_path)
            state = run('return', 'get', '--return', rid)
            assert state['readiness']['status'] == 'ready_for_review', state['readiness']
            normal_pdf = tmp/f'normal-{answer}.pdf'
            run('return', 'export', '--return', rid, '--type', 'pdf', '--template', Path(args.forms_dir)/'f1040.pdf', '--out', normal_pdf)
            fields = PdfReader(normal_pdf).get_fields()
            field = f'topmostSubform[0].Page1[0].c1_10[{0 if answer else 1}]'
            assert str(fields[field]['/V']) == ('/1' if answer else '/2'), fields[field]
            xml = tmp/f'mef-{answer}.xml'
            run('return', 'export', '--return', rid, '--type', 'mef', '--out', xml)
            assert f'<VirtualCurAcquiredDurTYInd>{str(answer).lower()}</VirtualCurAcquiredDurTYInd>' in xml.read_text()
        # Reset to an explicit negative answer before testing an unsupported dependent.
        add(rid, 'dependent', {'name':'Synthetic Child', 'relationship':'child', 'age':10, 'months_lived_with_taxpayer':12})
        result = run('return', 'export', '--return', rid, '--type', 'packet', '--draft', '--forms-dir', args.forms_dir, '--out-dir', tmp/'packet')
        assert result['readiness']['status'] == 'unsupported'
        manifest = json.loads((tmp/'packet/manifest.json').read_text())
        assert any(b['code'] == 'dependent_qualification_not_fully_modeled' for b in manifest['readiness']['blockers'])
        for pdf in (tmp/'packet').glob('*.pdf'): inspect(pdf)
        nr = create()
        add(nr, 'w2', {'employer_ein':'12-3456789', 'wages':'300000', 'federal_withholding':'50000'})
        nra = {'visa_type':'F-1', 'citizenship':'CN', 'first_calendar_year_in_status':2025, 'calendar_years_in_status_including_current':1}
        (tmp/'nra.json').write_text(json.dumps(nra))
        run('return', 'set-nra', '--return', nr, '--file', tmp/'nra.json')
        result = run('return', 'export', '--return', nr, '--type', 'packet', '--draft', '--forms-dir', args.forms_dir, '--out-dir', tmp/'nr')
        assert result['readiness']['status'] == 'unsupported'
        manifest = json.loads((tmp/'nr/manifest.json').read_text())
        assert any(b['code'] == 'nra_required_facts_not_modeled' for b in manifest['readiness']['blockers'])
        for pdf in (tmp/'nr').glob('*.pdf'): inspect(pdf)
    print('draft acceptance: passed')

if __name__ == '__main__': main()
