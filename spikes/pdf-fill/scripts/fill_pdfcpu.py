#!/usr/bin/env python3
"""Arm C driver: pdfcpu form export -> set target values -> form fill.
Usage: fill_pdfcpu.py <pdfcpu> <in.pdf> <targets.json> <form> <out.pdf>"""
import json, subprocess, sys, tempfile, os
pdfcpu, inp, targets_path, form, out = sys.argv[1:6]
targets = [t for t in json.load(open(targets_path)) if t["form"] == form]
with tempfile.TemporaryDirectory() as td:
    exp = os.path.join(td, "exp.json")
    subprocess.run([pdfcpu, "form", "export", inp, exp], check=True, capture_output=True)
    d = json.load(open(exp))
    filled = 0
    by_name = {}
    for t in targets: by_name[t["qname"]] = t
    for formobj in d["forms"]:
        for tf in formobj.get("textfield", []):
            if tf["name"] in by_name:
                tf["value"] = by_name[tf["name"]]["value"]; filled += 1
        for cb in formobj.get("checkbox", []):
            if cb["name"] in by_name:
                cb["value"] = True; filled += 1
    filljson = os.path.join(td, "fill.json")
    json.dump(d, open(filljson, "w"))
    r = subprocess.run([pdfcpu, "form", "fill", inp, filljson, out], capture_output=True, text=True)
    print(f"arm C (pdfcpu): set {filled} of {len(targets)} targets; pdfcpu exit={r.returncode}")
    if r.returncode != 0: print(r.stdout, r.stderr)
