#!/usr/bin/env python3
"""Independent verification for the PDF fill spike.

For every target x arm output:
  * pypdf readback of the field /V (pypdf wrote none of the arms A/B)
  * pdfcpu `form export` readback
  * appearance proxy: for each widget annotation of the target field —
    text/comb: an /AP /N stream exists and its byte length;
    checkbox: some widget /AS equals the field's on-state.
  * text-visibility proxy: the filled value occurs in pdftotext output
    (whitespace-insensitive for digit strings).
  * document-level: /NeedAppearances value, /XFA still present.

Usage: verify.py <pdfcpu> <results_dir> <targets.json> <out.json>
Requires: pypdf, poppler pdftotext on PATH.
"""
import json, subprocess, sys, os
from pypdf import PdfReader

pdfcpu, results_dir, targets_path, out_path = sys.argv[1:5]
targets = json.load(open(targets_path))
ARMS = ["A", "A1", "A2", "B", "C", "Bflat"]

def widgets_by_qname(reader):
    out = {}
    for page in reader.pages:
        for a in (page.get("/Annots") or []):
            o = a.get_object()
            names, node = [], o
            while node is not None:
                t = node.get("/T")
                if t:
                    names.append(str(t))
                node = node.get("/Parent")
                if node is not None:
                    node = node.get_object()
            q = ".".join(reversed(names))
            ap = o.get("/AP")
            ap_n_len = None
            if ap and "/N" in ap:
                n = ap["/N"]
                try:
                    if hasattr(n, "get_data"):
                        ap_n_len = len(n.get_data())
                    elif isinstance(n, dict):
                        # checkbox: /N is a dict of state -> stream
                        ap_n_len = max(
                            (len(v.get_object().get_data()) for v in n.values()
                             if hasattr(v.get_object(), "get_data")),
                            default=0,
                        )
                except Exception:
                    ap_n_len = -1
            out.setdefault(q, []).append({
                "AS": str(o["/AS"]) if o.get("/AS") is not None else None,
                "ap_n_len": ap_n_len,
            })
    return out

def pdfcpu_values(path):
    exp = path + ".export.json"
    subprocess.run([pdfcpu, "form", "export", path, exp],
                   capture_output=True)
    vals = {}
    try:
        d = json.load(open(exp))
        for formobj in d.get("forms", []):
            for tf in formobj.get("textfield", []):
                vals[tf["name"]] = tf.get("value", "")
            for cb in formobj.get("checkbox", []):
                vals[cb["name"]] = cb.get("value", False)
    except Exception as e:
        vals = {"__error__": str(e)}
    if os.path.exists(exp):
        os.remove(exp)
    return vals

def visible(text, value, kind):
    if kind == "checkbox":
        return None  # checkboxes have no text; judged by /AS + render
    flat = "".join(text.split())
    if value.replace(" ", "").isdigit():
        return value in flat
    return value in text

report = {}
for form in sorted({t["form"] for t in targets}):
    form_targets = [t for t in targets if t["form"] == form]
    report[form] = {}
    for arm in ARMS:
        path = os.path.join(results_dir, f"{form}-{arm}.pdf")
        if not os.path.exists(path):
            continue
        reader = PdfReader(path)
        fields = reader.get_fields() or {}
        widgets = widgets_by_qname(reader)
        cpu = pdfcpu_values(path)
        txt = subprocess.run(["pdftotext", path, "-"], capture_output=True,
                             text=True).stdout
        acro = reader.trailer["/Root"].get("/AcroForm")
        doc_info = {
            "need_appearances": (str(acro.get("/NeedAppearances"))
                                 if acro is not None else None),
            "xfa_present": ("/XFA" in acro) if acro is not None else None,
            "field_count_pypdf": len(fields),
        }
        rows = []
        for t in form_targets:
            q = t["qname"]
            f = fields.get(q)
            v = f.get("/V") if f else None
            v_str = str(v) if v is not None else None
            v_type = type(v).__name__ if v is not None else None
            if t["kind"] == "checkbox":
                readback_ok = (v_str == t["on_state"] and v_type == "NameObject")
                cpu_ok = (cpu.get(q) is True)
                w = widgets.get(q, [])
                as_ok = any(x["AS"] == t["on_state"] for x in w)
                ap_ok = as_ok
            else:
                readback_ok = (v_str == t["value"])
                cpu_ok = (str(cpu.get(q, "")) == t["value"])
                w = widgets.get(q, [])
                ap_ok = any((x["ap_n_len"] or 0) > 0 for x in w)
            rows.append({
                "semantic": t["semantic"], "qname": q, "kind": t["kind"],
                "expected": t["value"] if t["kind"] != "checkbox" else t["on_state"],
                "pypdf_V": v_str, "pypdf_V_type": v_type,
                "readback_ok": readback_ok, "pdfcpu_ok": cpu_ok,
                "ap_ok": ap_ok, "visible_text": visible(txt, t["value"], t["kind"]),
                "widgets": widgets.get(q, []),
            })
        report[form][arm] = {"doc": doc_info, "rows": rows}
        n = len(rows)
        print(f"{form} arm {arm}: readback {sum(r['readback_ok'] for r in rows)}/{n} "
              f"pdfcpu {sum(r['pdfcpu_ok'] for r in rows)}/{n} "
              f"ap {sum(r['ap_ok'] for r in rows)}/{n} "
              f"NA={doc_info['need_appearances']} XFA={doc_info['xfa_present']} "
              f"fields={doc_info['field_count_pypdf']}")

json.dump(report, open(out_path, "w"), indent=1)
print("wrote", out_path)
