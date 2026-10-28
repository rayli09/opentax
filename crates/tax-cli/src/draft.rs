//! Rasterized review copies: no interactive fields or XFA survive.
use lopdf::{dictionary, Document, Object, Stream};
use std::path::Path;
use std::process::Command;

fn ppm(data: &[u8]) -> Result<(usize, usize, &[u8]), String> {
    let mut i = 0;
    fn token<'a>(data: &'a [u8], i: &mut usize) -> Result<&'a [u8], String> {
        while *i < data.len() {
            if data[*i] == b'#' {
                while *i < data.len() && data[*i] != b'\n' {
                    *i += 1;
                }
            }
            if *i < data.len() && data[*i].is_ascii_whitespace() {
                *i += 1;
            } else {
                break;
            }
        }
        let start = *i;
        while *i < data.len() && !data[*i].is_ascii_whitespace() {
            *i += 1;
        }
        if start == *i || *i == data.len() {
            return Err("invalid PPM header".into());
        }
        Ok(&data[start..*i])
    }
    if token(data, &mut i)? != b"P6" {
        return Err("unexpected image format".into());
    }
    let width: usize = std::str::from_utf8(token(data, &mut i)?)
        .map_err(|e| e.to_string())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    let height: usize = std::str::from_utf8(token(data, &mut i)?)
        .map_err(|e| e.to_string())?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    if token(data, &mut i)? != b"255" {
        return Err("unexpected PPM depth".into());
    }
    i += 1;
    let len = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(3))
        .ok_or("image too large")?;
    if width == 0 || height == 0 || data.len().saturating_sub(i) != len {
        return Err("invalid PPM size".into());
    }
    Ok((width, height, &data[i..]))
}

pub fn rasterize(input: &Path, output: &Path, work: &Path) -> Result<(), String> {
    let mut source = Document::load(input).map_err(|e| e.to_string())?;
    let root = source
        .trailer
        .get(b"Root")
        .map_err(|e| e.to_string())?
        .as_reference()
        .map_err(|e| e.to_string())?;
    let acro = source
        .get_dictionary(root)
        .map_err(|e| e.to_string())?
        .get(b"AcroForm")
        .ok()
        .cloned();
    match acro {
        Some(Object::Reference(id)) => {
            source
                .get_dictionary_mut(id)
                .map_err(|e| e.to_string())?
                .remove(b"XFA");
        }
        Some(Object::Dictionary(mut dict)) => {
            dict.remove(b"XFA");
            source
                .get_dictionary_mut(root)
                .map_err(|e| e.to_string())?
                .set("AcroForm", dict);
        }
        _ => {}
    }
    let expected_pages = source.get_pages().len();
    if expected_pages == 0 {
        return Err("PDF has no pages".into());
    }
    let stripped = work.join("source-no-xfa.pdf");
    source.save(&stripped).map_err(|e| e.to_string())?;
    let pattern = work.join("page-%06d.ppm");
    let result = Command::new("gs")
        .args([
            "-q",
            "-dSAFER",
            "-dBATCH",
            "-dNOPAUSE",
            "-sDEVICE=ppmraw",
            "-r100",
        ])
        .arg(format!("-sOutputFile={}", pattern.display()))
        .arg(&stripped)
        .output()
        .map_err(|e| format!("Ghostscript is required for draft export: {e}"))?;
    if !result.status.success() {
        return Err(format!("Ghostscript failed with {}", result.status));
    }
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica-Bold" },
    );
    let mut kids = Vec::new();
    for n in 1..=expected_pages {
        let path = work.join(format!("page-{n:06}.ppm"));
        let bytes = std::fs::read(&path).map_err(|e| format!("missing rendered page {n}: {e}"))?;
        let (width, height, pixels) = ppm(&bytes)?;
        let w = width as f32 * 72.0 / 100.0;
        let h = height as f32 * 72.0 / 100.0;
        let mut image = Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => width as i64, "Height" => height as i64, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 },
            pixels.to_vec(),
        );
        image.compress().map_err(|e| e.to_string())?;
        let image_id = doc.add_object(image);
        // Opaque red banner is part of the only page content and cannot be
        // hidden by form widgets or an alternate XFA presentation.
        let content = format!("q {w} 0 0 {h} 0 0 cm /Im Do Q\nq 0.75 0 0 rg 0 {y} {w} 32 re f 1 1 1 rg BT /F 19 Tf 12 {t} Td (DRAFT - NOT FOR FILING) Tj ET Q\n", y=h-32.0, t=h-25.0);
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page = doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![0.into(), 0.into(), Object::Real(w), Object::Real(h)], "Resources" => dictionary! { "XObject" => dictionary! { "Im" => image_id }, "Font" => dictionary! { "F" => font } }, "Contents" => content_id });
        kids.push(page.into());
    }
    doc.objects.insert(
        pages_id,
        dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => expected_pages as i64 }.into(),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc.save(output).map_err(|e| e.to_string())?;
    Ok(())
}
