"""Independent structural and rendered checks for draft acceptance tests."""
import json
import subprocess
import tempfile
from pathlib import Path
from pypdf import PdfReader


def verify_packet(directory, result, expected_values):
    directory = Path(directory)
    manifest = json.loads((directory / 'manifest.json').read_text())
    assert manifest['draft'] is True
    assert manifest['readiness'] == result['readiness']
    assert manifest['readiness']['blockers']
    pdfs = list(directory.glob('*.pdf'))
    assert pdfs and set(expected_values).issubset({p.name for p in pdfs})
    for pdf in pdfs:
        reader = PdfReader(pdf)
        assert reader.trailer['/Root'].get('/AcroForm') is None
        assert reader.pages
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run(['gs', '-q', '-dSAFER', '-dBATCH', '-dNOPAUSE', '-sDEVICE=png16m', '-r150', f'-sOutputFile={tmp}/page-%d.png', str(pdf)], check=True)
            text = ''
            for number, page in enumerate(reader.pages, 1):
                assert not page.get('/Annots')
                assert 'DRAFT - NOT FOR FILING' in page.extract_text()
                image = (Path(tmp) / f'page-{number}.png').read_bytes()
                page_text = subprocess.run(['tesseract', 'stdin', 'stdout'], input=image, capture_output=True, check=True).stdout.decode(errors='replace').upper()
                assert 'DRAFT' in page_text and 'NOT FOR FILING' in page_text, (pdf, number)
                text += page_text.replace('S', '5').replace('O', '0').replace(',', '.')
            for value in expected_values.get(pdf.name, []):
                assert value in text, (pdf, value)
    return manifest
