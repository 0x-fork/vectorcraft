//! Colour fonts through the session: the SVG importer it registers reads OpenType SVG glyphs,
//! which then draw, export and outline as art.

use serde_json::{Value, json};
use vectorcraft_doc::NodeKind;
use vectorcraft_text::test_fonts::{SVG_CHAR, source_sans_gid, svg_font};

use super::*;

fn session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({"width": 800, "height": 600})).unwrap();
    s
}

fn ids(v: &Value) -> Vec<NodeId> {
    v["ids"].as_array().unwrap().iter().map(|x| NodeId(x.as_u64().unwrap())).collect()
}

fn node(s: &Session, id: NodeId) -> vectorcraft_doc::Node {
    s.doc().unwrap().doc.node(id).cloned().unwrap()
}

/// `font` with licence flags (OS/2 `fsType`) that forbid embedding it.
fn not_embeddable(mut font: Vec<u8>) -> Vec<u8> {
    let tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
    let os2 = (0..tables).map(|i| 12 + 16 * i).find(|r| &font[*r..*r + 4] == b"OS/2").unwrap();
    let at = u32::from_be_bytes(font[os2 + 8..os2 + 12].try_into().unwrap()) as usize;
    font[at + 8..at + 10].copy_from_slice(&0x0002u16.to_be_bytes());
    font
}

/// The fills of `pdf`'s art read back, and where each is.
fn fills_of(pdf: &[u8]) -> Vec<(vectorcraft_color::Paint, vectorcraft_geom::Rect)> {
    let back = vectorcraft_pdf::import_with_report(pdf, &vectorcraft_pdf::ImportOptions::default()).unwrap().document;
    let mut fills = vec![];
    for l in &back.layers {
        l.walk(&mut |n| {
            if matches!(n.kind, NodeKind::Path { .. } | NodeKind::Compound { .. })
                && let Some(b) = n.geometric_bounds()
            {
                fills.push((n.appearance.fill_paint(), b));
            }
        });
    }
    fills
}

/// A session with `family`, the SVG test font of `document` for [`SVG_CHAR`]'s glyph, and 32 pt
/// type of `SVG_CHAR` and `F` in it at (10, 50).
fn with_svg_type(family: &str, document: &str) -> Session {
    let g = source_sans_gid(SVG_CHAR).unwrap();
    let mut s = session();
    vectorcraft_text::FontDb::global().add_font(svg_font(family, &[(g, g, document)]).unwrap());
    s.execute("text.create", &json!({"x": 10, "y": 50, "text": format!("{SVG_CHAR}F"), "size": 32, "font": family})).unwrap();
    s
}

/// OpenType SVG glyphs become groups of the art their documents draw, through the SVG importer the
/// session registers.
#[test]
fn create_outlines_turns_svg_glyphs_into_groups() {
    let g = source_sans_gid(SVG_CHAR).unwrap();
    let doc = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="a"><stop offset="0" stop-color="#f00"/><stop offset="1" stop-color="#00f"/></linearGradient></defs><g id="glyph{g}"><path fill="url(#a)" d="M0 0h500v-700h-500z"/></g></svg>"##
    );
    let mut s = with_svg_type("Svgtest Outlines", &doc);
    let r = ids(&s.execute("type.createOutlines", &json!({})).unwrap());
    let ch = node(&s, r[0]).children().unwrap().clone();
    assert!(matches!(ch[0].kind, NodeKind::Group { .. }), "the glyph's art: {:?}", ch[0].kind_label());
    let mut gradient = false;
    ch[0].walk(&mut |n| gradient |= matches!(n.appearance.fill_paint(), vectorcraft_color::Paint::Gradient(_)));
    assert!(gradient, "with its gradient");
    // Half an em wide at 32 pt, standing 0.7 em on the baseline at y 50.
    let b = ch[0].geometric_bounds().unwrap();
    assert!((b.width() - 16.0).abs() < 1e-6 && (b.y1 - 50.0).abs() < 1e-6 && (b.height() - 22.4).abs() < 1e-6, "{b:?}");
    assert!(matches!(ch[1].kind, NodeKind::Path { .. } | NodeKind::Compound { .. }), "the `F` an outline");
}

/// In a PDF with real text, an SVG glyph is text like any other (selectable, searchable), which
/// draws nothing itself, and its art is drawn where it sits: no outline of the font's covers it.
#[test]
fn pdf_real_text_draws_svg_glyphs_as_art_and_keeps_their_characters() {
    let g = source_sans_gid(SVG_CHAR).unwrap();
    let doc = format!(r##"<svg xmlns="http://www.w3.org/2000/svg"><g id="glyph{g}"><path fill="#f00" d="M0 0h500v-700h-500z"/></g></svg>"##);
    let s = with_svg_type("Svgtest Pdf", &doc);
    let d = s.doc().unwrap().doc.clone();
    let pdf = vectorcraft_pdf::export(&d, &vectorcraft_pdf::PdfOptions::uncompressed()).unwrap();
    // The glyphs each text-showing line writes (the font's codes are octal escapes), and whether
    // it is invisible.
    let raw = String::from_utf8_lossy(&pdf);
    let shown: Vec<(bool, usize)> =
        raw.lines().filter(|l| l.contains("Tj") || l.contains("TJ")).map(|l| (l.contains("3 Tr"), l.matches("\\0").count())).collect();
    assert_eq!(shown, [(false, 2)], "both characters once, as text");
    // The glyphs as the font draws them (its Type 3 glyph procedures, `<width> 0 d0` and what
    // they paint): the SVG glyph's draws nothing, rather than the font's plain outline over the
    // art; the `F` its outline.
    let procs: Vec<&str> = raw.split("stream\n").filter(|c| c.contains(" d0")).collect();
    assert_eq!(procs.len(), 2, "{procs:?}");
    assert_eq!(procs.iter().filter(|c| !c.contains(" Do")).count(), 1, "one glyph draws nothing: {procs:?}");
    let back = vectorcraft_pdf::import_with_report(&pdf, &vectorcraft_pdf::ImportOptions::default()).unwrap().document;
    let mut red = false;
    for l in &back.layers {
        l.walk(&mut |n| red |= n.appearance.fill_paint() == vectorcraft_color::Paint::solid(vectorcraft_color::Color::rgb8(255, 0, 0)));
    }
    assert!(red, "the glyph's art");
}

/// An SVG glyph whose document holds no art for it keeps its outline in PDF real text too (its
/// text draws nothing, as SVG glyphs' do).
#[test]
fn pdf_real_text_draws_the_outline_of_an_svg_glyph_without_art() {
    let doc = r##"<svg xmlns="http://www.w3.org/2000/svg"><g id="glyph1"/></svg>"##;
    let s = with_svg_type("Svgtest Bare Pdf", doc);
    let d = s.doc().unwrap().doc.clone();
    let pdf = vectorcraft_pdf::export(&d, &vectorcraft_pdf::PdfOptions::uncompressed()).unwrap();
    // The `E`'s outline: from x 10 (its side bearing past it), 0.66 em tall, on the baseline at 50.
    let e = vectorcraft_geom::Rect::new(12.0, 32.0, 20.0, 48.0);
    assert!(fills_of(&pdf).iter().any(|(_, b)| b.intersect(e).area() > 10.0), "the glyph's outline");
}

/// In a font that may not be embedded, an SVG glyph's art is drawn once.
#[test]
fn pdf_draws_an_svg_glyphs_art_once_when_its_font_cant_be_embedded() {
    let g = source_sans_gid(SVG_CHAR).unwrap();
    let doc = format!(r##"<svg xmlns="http://www.w3.org/2000/svg"><g id="glyph{g}"><path fill="#f00" d="M0 0h500v-700h-500z"/></g></svg>"##);
    let mut s = session();
    vectorcraft_text::FontDb::global().add_font(not_embeddable(svg_font("Svgtest Locked", &[(g, g, &doc)]).unwrap()));
    s.execute("text.create", &json!({"x": 10, "y": 50, "text": format!("{SVG_CHAR}F"), "size": 32, "font": "Svgtest Locked"})).unwrap();
    let d = s.doc().unwrap().doc.clone();
    let pdf = vectorcraft_pdf::export(&d, &vectorcraft_pdf::PdfOptions::uncompressed()).unwrap();
    let red = vectorcraft_color::Paint::solid(vectorcraft_color::Color::rgb8(255, 0, 0));
    assert_eq!(fills_of(&pdf).iter().filter(|(p, _)| *p == red).count(), 1);
}

/// Expand Appearance of type with an effect of its own (drawn reshaped as its outlines) keeps its
/// colour glyphs, as their art.
#[test]
fn expand_appearance_keeps_the_colour_glyphs_of_type_with_effects() {
    use vectorcraft_text::test_fonts::{COLOUR_CHAR, colr_family, colr_font};
    vectorcraft_text::FontDb::global().add_font(colr_font(true).unwrap());
    let mut s = session();
    let r = s.execute("text.create", &json!({"x": 10, "y": 50, "text": format!("{COLOUR_CHAR}F"), "size": 32, "font": colr_family(true)})).unwrap();
    let t = NodeId(r["id"].as_u64().unwrap());
    s.execute("effect.apply", &json!({"ids": [t.0], "effect": "distort.transform", "params": {"moveH": 5}})).unwrap();
    let r = ids(&s.execute("effect.expandAppearance", &json!({})).unwrap());
    let mut gradients = 0;
    for id in r {
        node(&s, id).walk(&mut |n| gradients += usize::from(matches!(n.appearance.fill_paint(), vectorcraft_color::Paint::Gradient(_))));
    }
    assert!(gradients > 0, "the colour glyph's gradient layers");
    vectorcraft_testkit::invariants::check_document(&s.doc().unwrap().doc).unwrap();
}

/// The canvas draws an SVG glyph as its art: its own colours, and the type's fill where it uses
/// `currentColor`.
#[test]
fn the_canvas_draws_svg_glyphs_as_their_art() {
    let g = source_sans_gid(SVG_CHAR).unwrap();
    // The left half of a 500 by 700 box red, the right half in the text's colour.
    let doc = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg"><g id="glyph{g}"><path fill="#f00" d="M0 0h250v-700h-250z"/><path fill="currentColor" d="M250 0h250v-700h-250z"/></g></svg>"##
    );
    let s = with_svg_type("Svgtest Canvas", &doc);
    let opts = vectorcraft_render::RenderOptions { background: Some([255, 255, 255, 255]), ..Default::default() };
    let img = vectorcraft_render::Renderer::new().render(&s.doc().unwrap().doc, 100, 100, vectorcraft_geom::Affine::IDENTITY, &opts);
    // At 32 pt from x 10 on the baseline at y 50: 16 pt wide, 22.4 pt tall.
    assert_eq!(img.pixel(14, 40), [255, 0, 0, 255], "its own red");
    assert_eq!(img.pixel(22, 40), [0, 0, 0, 255], "the type's black fill");
}
