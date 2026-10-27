#!/usr/bin/env python3
"""Generate targets.json for the PDF fill spike.

Semantic mapping is derived from the XFA template embedded in each IRS PDF:
every XFA <field> carries an <assist><speak> accessibility description that
names the form line ("1a. Total amount from Form(s) W-2, box 1 ..."), comb
fields carry <comb numberOfCells>, and 1040/Schedule D line labels are also
bound to fields by <draw name="Ln.."> traversal refs. Qualified AcroForm names
mirror the XFA subform path. Checkbox on-states are read from the widget
appearance dictionaries (/AP /N keys other than /Off).

Usage: make_targets.py <fixtures_dir> <output_json>
Requires: pypdf (pip install pypdf).
"""
import json, re, sys
import xml.etree.ElementTree as ET
from pypdf import PdfReader

def strip(t): return t.split('}')[-1]

def load(form, fx):
    r = PdfReader(f"{fx}/{form}.pdf")
    xfa = r.trailer["/Root"]["/AcroForm"]["/XFA"]
    tpl = None
    for i in range(0, len(xfa), 2):
        if str(xfa[i]) == "template":
            tpl = xfa[i + 1].get_object().get_data().decode("utf8", "ignore")
    return r, ET.fromstring(tpl)

def walk(el, path, out):
    counts = {}
    for ch in el:
        tag = strip(ch.tag)
        if tag in ("subform", "field", "draw", "area", "exclGroup"):
            nm = ch.get("name")
            if nm:
                idx = counts.get(nm, 0); counts[nm] = idx + 1
                q = f"{path}.{nm}[{idx}]" if path else f"{nm}[{idx}]"
                if tag == "field":
                    speak = ui = comb = None
                    for d in ch.iter():
                        if strip(d.tag) == "speak" and d.text and d.text.strip():
                            speak = " ".join(d.text.split())
                        if strip(d.tag) in ("textEdit", "numericEdit", "checkButton", "choiceList", "dateTimeEdit"):
                            ui = strip(d.tag)
                        if strip(d.tag) == "comb":
                            comb = d.get("numberOfCells")
                    out.append({"qname": q, "leaf": nm, "speak": speak, "ui": ui, "comb": comb})
                walk(ch, q, out); continue
        walk(ch, path, out)

def lnmap(root):
    m = {}
    for el in root.iter():
        if strip(el.tag) == "draw" and (el.get("name") or "").startswith("Ln"):
            txt = ref = None
            for d in el.iter():
                if strip(d.tag) == "text" and txt is None and d.text: txt = d.text.strip()
                if strip(d.tag) == "traverse": ref = d.get("ref")
            if txt and ref: m[txt] = ref.rsplit("[", 1)[0]
    return m

def widget_states(reader):
    """qname -> list of (rect, on_state) for button widgets."""
    out = {}
    for page in reader.pages:
        for a in (page.get("/Annots") or []):
            o = a.get_object()
            names, node = [], o
            while node is not None:
                t = node.get("/T")
                if t: names.append(str(t))
                node = node.get("/Parent")
                if node is not None: node = node.get_object()
            q = ".".join(reversed(names))
            ap = o.get("/AP")
            if ap and "/N" in ap:
                states = [str(k) for k in ap["/N"].keys() if str(k) != "/Off"]
                rect = [float(x) for x in o["/Rect"]] if o.get("/Rect") else None
                out.setdefault(q, []).append((rect, states))
    return out

def main():
    fx, out_path = sys.argv[1], sys.argv[2]
    targets = []
    def add(form, semantic, qname, kind, value, on_state=None, note=None):
        t = {"form": form, "semantic": semantic, "qname": qname, "kind": kind, "value": value}
        if on_state: t["on_state"] = on_state
        if note: t["note"] = note
        targets.append(t)

    # ---------------- f1040 ----------------
    form = "f1040"; r, root = load(form, fx); fields = []; walk(root, "", fields)
    byq = {f["qname"]: f for f in fields}
    ln = lnmap(root)
    P1 = "topmostSubform[0].Page1[0]"; P2 = "topmostSubform[0].Page2[0]"
    def L(line, page):  # qualified name for a 1040 line amount field
        leaf = ln[line]
        return f"{P1}.{leaf}[0]" if page == 1 else f"{P2}.{leaf}[0]"
    add(form, "taxpayer first name and middle initial", f"{P1}.f1_14[0]", "text", "Spike T")
    add(form, "taxpayer last name", f"{P1}.f1_15[0]", "text", "Tester")
    add(form, "taxpayer SSN (comb, 9 cells)", f"{P1}.f1_16[0]", "comb", "123456789")
    add(form, "spouse first name and middle initial", f"{P1}.f1_17[0]", "text", "Partner")
    add(form, "spouse last name", f"{P1}.f1_18[0]", "text", "Tester")
    add(form, "spouse SSN (comb, 9 cells)", f"{P1}.f1_19[0]", "comb", "987654321")
    add(form, "home address", f"{P1}.Address_ReadOrder[0].f1_20[0]", "text", "123 Main St")
    add(form, "apartment no.", f"{P1}.Address_ReadOrder[0].f1_21[0]", "text", "4B")
    add(form, "city", f"{P1}.Address_ReadOrder[0].f1_22[0]", "text", "Springfield")
    add(form, "state", f"{P1}.Address_ReadOrder[0].f1_23[0]", "text", "IL")
    add(form, "ZIP code", f"{P1}.Address_ReadOrder[0].f1_24[0]", "text", "62704")
    # Filing status: one Btn field, 5 widgets, on-states /1../5 in form order
    # (Single, MFJ, MFS, HOH, QSS); Single is /1 (first widget, top-left).
    add(form, "filing status = Single", f"{P1}.Checkbox_ReadOrder[0].c1_8[0]", "checkbox", "/1", on_state="/1",
        note="radio-style Btn field with 5 widgets; states /1..../5 in printed order Single/MFJ/MFS/HOH/QSS")
    for line, val in [("1a","50000"),("1z","50000"),("2b","100"),("3b","200"),("9","50300"),("11a","50300")]:
        add(form, f"line {line}", L(line, 1), "text", val)
    for line, val in [("11b","50300"),("12e","15750"),("15","34550"),("16","3900"),("24","3900"),
                      ("25a","4000"),("25d","4000"),("33","4000"),("34","100"),("35a","100")]:
        add(form, f"line {line}", L(line, 2), "text", val)
    add(form, "routing number (comb, 9 cells)", f"{P2}.RoutingNo[0].f2_32[0]", "comb", "123456789")
    add(form, "account number (comb, 17 cells)", f"{P2}.AccountNo[0].f2_33[0]", "comb", "12345678901234567")
    add(form, "taxpayer occupation", f"{P2}.f2_40[0]", "text", "Engineer")

    # ---------------- Schedule D ----------------
    form = "f1040sd"; r, root = load(form, fx); fields = []; walk(root, "", fields)
    ln = lnmap(root)
    ws = widget_states(r)
    add(form, "name(s) shown on return", f"{P1}.f1_1[0]", "text", "Spike Tester")
    add(form, "SSN", f"{P1}.f1_2[0]", "text", "123456789")
    qof_no = f"{P1}.c1_1[1]"
    add(form, "qualified opportunity fund question = No", qof_no, "checkbox", ws[qof_no][0][1][0], on_state=ws[qof_no][0][1][0])
    def SDL(line, page):
        leaf = ln[line]
        return f"{P1}.{leaf}[0]" if page == 1 else f"{P2}.{leaf}[0]"
    for line, val, pg in [("4","1000",1),("5","2000",1),("7","3000",1),("11","4000",1),("15","5000",1),("16","3900",2),("18","100",2)]:
        add(form, f"line {line}", SDL(line, pg), "text", val)
    l17 = f"{P2}.c2_1[0]"
    add(form, "line 17 (both gains) = Yes", l17, "checkbox", ws[l17][0][1][0], on_state=ws[l17][0][1][0])
    # Part I row 1a totals, column (h) gain/loss:
    for f in fields:
        if f["speak"] and "Row: 1a." in f["speak"] and "Column: (h)" in f["speak"]:
            add(form, "Part I row 1a totals, column (h)", f["qname"], "text", "6000"); break

    # ---------------- Form 8949 ----------------
    form = "f8949"; r, root = load(form, fx); fields = []; walk(root, "", fields)
    ws = widget_states(r)
    add(form, "name(s) shown on return", f"{P1}.f1_01[0]", "text", "Spike Tester")
    add(form, "SSN/TIN", f"{P1}.f1_02[0]", "text", "123456789")
    boxA = f"{P1}.c1_1[0]"
    add(form, "Part I Box A (basis reported to IRS)", boxA, "checkbox", ws[boxA][0][1][0], on_state=ws[boxA][0][1][0])
    row1 = [f for f in fields if ".Table_Line1_Part1[0].Row1[0]." in f["qname"]]
    row1_vals = ["100 sh. XYZ Co.", "01/15/2025", "06/15/2025", "10000", "7000", None, "500", "3500"]
    # Row1 columns in template order: (a) desc, (b) acquired, (c) sold, (d) proceeds,
    # (e) cost, (f) code, (g) adjustment amount, (h) gain/loss. Skip (f) code (left blank on purpose).
    for f, v in zip(row1, row1_vals):
        if v is None: continue
        col = "abcdefgh"[row1.index(f)]
        add(form, f"row 1 column ({col})", f["qname"], "text", v)
    for f in fields:
        if f["speak"] and f["speak"].startswith("Row: 2. Column: (d)"):
            add(form, "row 2 totals, column (d) proceeds", f["qname"], "text", "10000")
        if f["speak"] and f["speak"].startswith("Row: 2. Column: (h)"):
            add(form, "row 2 totals, column (h) gain/loss", f["qname"], "text", "3500")

    json.dump(targets, open(out_path, "w"), indent=1)
    from collections import Counter
    print("targets:", len(targets), Counter(t["form"] for t in targets))

if __name__ == "__main__":
    main()
