//! PDF fill spike — three fill arms on real IRS forms.
//!
//! Arms:
//!   A  — our own tax-pdf crate (`fill_fields`, lopdf, sets /V only).
//!   A2 — Arm A output post-processed with lopdf to set
//!        AcroForm /NeedAppearances = true (the "one-line flag" hypothesis).
//!   B  — pdfer_forms (regenerates widget appearance streams /AP).
//! Arm C (pdfcpu) is driven from shell/python, not from here.
//!
//! Usage:
//!   pdf-fill-spike enumerate   <pdf> <out.json>            (tax-pdf inspect)
//!   pdf-fill-spike enumerate-b <pdf> <out.json>            (pdfer_forms get_fields)
//!   pdf-fill-spike fill-a      <in.pdf> <targets.json> <form> <out.pdf>
//!   pdf-fill-spike fill-a2     <armA.pdf> <out.pdf>
//!   pdf-fill-spike fill-b      <in.pdf> <targets.json> <form> <out.pdf> [--flatten]

use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Debug, Deserialize)]
struct Target {
    form: String,
    semantic: String,
    qname: String,
    kind: String,
    value: String,
    #[serde(default)]
    on_state: Option<String>,
}

fn load_targets(path: &str, form: &str) -> Vec<Target> {
    let raw = std::fs::read_to_string(path).expect("read targets.json");
    let all: Vec<Target> = serde_json::from_str(&raw).expect("parse targets.json");
    all.into_iter().filter(|t| t.form == form).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
    match cmd {
        "enumerate" => {
            let fields = tax_pdf::inspect_fields(Path::new(&args[2])).expect("inspect");
            let json = serde_json::to_string_pretty(&fields).unwrap();
            std::fs::write(&args[3], json).unwrap();
            println!("tax-pdf inspect: {} fields", fields.len());
        }
        "enumerate-b" => {
            let reader = pdfer_forms::PdfReaderCompat::load(&args[2]).expect("pdfer load");
            let fields = reader.get_fields().expect("pdfer get_fields").unwrap_or_default();
            let mut out = Vec::new();
            for (name, f) in &fields {
                out.push(serde_json::json!({
                    "name": name,
                    "field_type": f.field_type,
                    "value": format!("{:?}", f.value),
                    "flags": f.flags,
                    "states": f.states,
                }));
            }
            std::fs::write(&args[3], serde_json::to_string_pretty(&out).unwrap()).unwrap();
            println!("pdfer_forms get_fields: {} fields", fields.len());
        }
        "fill-a" => {
            let targets = load_targets(&args[3], &args[4]);
            let mut values: HashMap<String, String> = HashMap::new();
            for t in &targets {
                // Arm A passes the canonical value verbatim, exactly as a
                // tax-pdf caller would: text/comb get the string, checkboxes
                // get their on-state string ("/1") — tax-pdf will store it as
                // a *string* /V for every type (that is the behaviour under test).
                values.insert(t.qname.clone(), t.value.clone());
            }
            let filled = tax_pdf::fill_fields(Path::new(&args[2]), &values, Path::new(&args[5]))
                .expect("tax-pdf fill");
            println!(
                "arm A (tax-pdf /V only): filled {} of {} targets",
                filled.len(),
                targets.len()
            );
            for t in &targets {
                if !filled.contains(&t.qname) {
                    println!("  NOT FILLED: {} ({})", t.qname, t.semantic);
                }
            }
        }
        "fill-a-names" => {
            // fill-a <in.pdf> <targets.json> <form> <enum.json> <out.pdf>
            // Like fill-a, but keys are translated through the enum file:
            // targets carry clean names; tax-pdf's inspect returns raw names
            // as decoded by lopdf strings (on IRS forms these are UTF-16BE
            // mangled). We map clean -> raw so fill_fields is exercised on
            // the names it actually produces, isolating the /V-fill question
            // from the name-decoding bug.
            let targets = load_targets(&args[3], &args[4]);
            let enum_raw = std::fs::read_to_string(&args[5]).expect("read enum");
            let enum_fields: Vec<serde_json::Value> =
                serde_json::from_str(&enum_raw).expect("parse enum");
            let clean = |s: &str| -> String {
                s.replace('\u{fffd}', "").replace('\u{0}', "")
            };
            let mut raw_by_clean: HashMap<String, String> = HashMap::new();
            for f in &enum_fields {
                let raw = f["name"].as_str().unwrap().to_string();
                raw_by_clean.insert(clean(&raw), raw);
            }
            let mut values: HashMap<String, String> = HashMap::new();
            let mut missing = 0;
            for t in &targets {
                match raw_by_clean.get(&t.qname) {
                    Some(raw) => {
                        values.insert(raw.clone(), t.value.clone());
                    }
                    None => {
                        missing += 1;
                        println!("  NO RAW NAME FOR: {} ({})", t.qname, t.semantic);
                    }
                }
            }
            let filled =
                tax_pdf::fill_fields(Path::new(&args[2]), &values, Path::new(&args[6]))
                    .expect("tax-pdf fill");
            println!(
                "arm A-names (tax-pdf /V only, raw names): filled {} of {} targets ({} unmapped)",
                filled.len(),
                targets.len(),
                missing
            );
        }
        "fill-a2" => {
            // Post-process Arm A output: set /NeedAppearances = true only.
            let mut doc = lopdf::Document::load(&args[2]).expect("load arm A output");
            let catalog_id = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
            let acro_ref = {
                let catalog = doc.get_dictionary(catalog_id).unwrap().clone();
                catalog.get(b"AcroForm").ok().and_then(|o| o.as_reference().ok())
            };
            match acro_ref {
                Some(acro_id) => {
                    let obj = doc.get_object_mut(acro_id).unwrap();
                    obj.as_dict_mut().unwrap().set("NeedAppearances", lopdf::Object::Boolean(true));
                }
                None => {
                    let catalog = doc.get_object_mut(catalog_id).unwrap();
                    let dict = catalog.as_dict_mut().unwrap();
                    match dict.get_mut(b"AcroForm") {
                        Ok(lopdf::Object::Dictionary(d)) => {
                            d.set("NeedAppearances", lopdf::Object::Boolean(true));
                        }
                        _ => panic!("AcroForm is neither a reference nor a dictionary"),
                    }
                }
            }
            doc.save(&args[3]).unwrap();
            println!("arm A2: NeedAppearances set to true");
        }
        "fill-b" => {
            let flatten = args.iter().any(|a| a == "--flatten");
            let targets = load_targets(&args[3], &args[4]);
            let reader = pdfer_forms::PdfReaderCompat::load(&args[2]).expect("pdfer load");
            let mut writer = pdfer_forms::PdfWriterCompat::from_reader(&reader);
            writer.set_need_appearances_writer(false).expect("set NA false");
            let mut updates: BTreeMap<String, pdfer_forms::FieldInput> = BTreeMap::new();
            for t in &targets {
                updates.insert(t.qname.clone(), pdfer_forms::FieldInput::from(t.value.clone()));
            }
            let report = writer
                .update_page_form_field_values(
                    pdfer_forms::PageSelection::All,
                    &updates,
                    0,
                    Some(false),
                    flatten,
                )
                .expect("pdfer fill");
            writer.save(&args[5]).expect("pdfer save");
            println!(
                "arm B (pdfer_forms, flatten={flatten}): report = {report:?} for {} targets",
                targets.len()
            );
            let _ = targets.iter().map(|t| &t.kind).count();
        }
        _ => {
            eprintln!("usage: enumerate|enumerate-b|fill-a|fill-a2|fill-b ...");
            std::process::exit(2);
        }
    }
}
