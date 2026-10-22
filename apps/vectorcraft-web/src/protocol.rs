//! The rules of the embedding protocol (`host`, `docs/web-embed.md`), kept free of browser types so
//! they are tested natively: which messages the bridge reads, and what a `vectorcraft:open`, a
//! `vectorcraft:export`, a `vectorcraft:place` or a `vectorcraft:solidify` must carry.

/// What a `vectorcraft:open` delivers: a file to open like one picked with File › Open.
#[derive(Debug, PartialEq, Eq)]
pub struct Open {
    /// The file name: the document's name, and the name it is saved under.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// The messages the bridge reads.
pub const HEARD: [&str; 10] = [
    "vectorcraft:sep.mark.set",
    "vectorcraft:sep.mark.clear",
    "vectorcraft:sep.mark.list",
    "vectorcraft:open",
    "vectorcraft:export",
    "vectorcraft:place",
    "vectorcraft:solidify",
    "vectorcraft:prefs",
    "vectorcraft:hello",
    "vectorcraft:colorSettings",
];

/// The longest preferences text a page may hand over (bytes).
pub const MAX_PREFS: usize = 1 << 20;

/// What a `vectorcraft:export` asks for: the active document written in a format, sent back as a
/// `vectorcraft:save` that carries the request's `id`.
#[derive(Debug, PartialEq, Eq)]
pub struct Export {
    /// The format's id (`pdf`, `svg`, `png`…; any format File › Export writes).
    pub format: &'static str,
    /// The format's extension, for the file's name.
    pub extension: &'static str,
    /// Record the document as saved once written: the parent stores it as the saved file.
    pub mark_saved: bool,
    /// Only the selection: one selected image in a raster format comes as its own pixels, as
    /// embedded (not drawn again); anything else as the selected objects written in `format`.
    pub selected_only: bool,
    /// PDF only: keep (`true`) or leave out (`false`) the native document the PDF carries for
    /// editing it again; `None` is the format's default (kept).
    pub preserve_editing: Option<bool>,
    /// Only one selected image's own pixels will do (a raster format; implies `selected_only`):
    /// anything else selected is refused rather than drawn, so a page paying a tracing service per
    /// image never sends it a render of a group or a page.
    pub image_only: bool,
    pub preserve_source: bool,
}

/// What a selected object is, for [`image_only_refusal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picked {
    Image,
    Group,
    Path,
    Type,
    Other,
}

/// Why an `imageOnly` export can't go ahead with `selected` (the selected objects), if it can't:
/// it needs exactly one image. The text of the `vectorcraft:failed` reply.
pub fn image_only_refusal(selected: &[Picked]) -> Option<String> {
    let what = match selected {
        [Picked::Image] => return None,
        [] => "nothing is selected".to_string(),
        [Picked::Group] => "a group is selected".to_string(),
        [Picked::Path] => "a path is selected".to_string(),
        [Picked::Type] => "type is selected".to_string(),
        [_] => "the selected object isn't an image".to_string(),
        many => format!("{} objects are selected", many.len()),
    };
    Some(format!("vectorcraft:export: select one image ({what})"))
}

/// A raster format, whose file an image's own pixels can be.
pub fn is_raster(format: &str) -> bool {
    matches!(format, "png" | "jpeg" | "jpg" | "webp" | "gif" | "tiff" | "bmp")
}

/// What a `vectorcraft:place` delivers: a file placed into the open document in place of the one
/// selected object (or, with nothing selected, at `at`): fitted inside `at` when given, else onto the
/// selected object's own frame through its transform (an image's pixel area), aspect kept.
#[derive(Debug, PartialEq)]
pub struct Place {
    pub name: String,
    pub bytes: Vec<u8>,
    /// `[x, y, width, height]` in document points (as `bounds` in a `vectorcraft:save` gives them).
    pub at: Option<[f64; 4]>,
    /// Also every other copy of the selected image: the same pixels at the same printed size, each
    /// onto its own frame, all in the same undo step.
    pub copies: bool,
}

/// What the parent is told is selected (`vectorcraft:selection`): whether it is exactly one image and,
/// if so, how many other copies of it the document holds at the same size and at other sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub image: bool,
    pub copies: usize,
    pub other_sizes: usize,
}

/// Whether the bridge reads a message at all: a `vectorcraft:open` or `vectorcraft:export` from the
/// parent window, on the page's own origin. Anything else is ignored without a reply, so another page learns nothing.
///
/// `own_origin` is the page's origin and `origin` the event's; `from_parent` says whether the
/// event's source is the parent window; `kind` is the message's `type`, when it is a string. An
/// opaque origin (`"null"`: a sandboxed frame, a `file:` page) can't be told apart from another
/// page's, so the bridge hears nothing there.
pub fn accepts(own_origin: &str, origin: &str, from_parent: bool, kind: Option<&str>) -> bool {
    from_parent && !is_opaque(own_origin) && origin == own_origin && kind.is_some_and(|k| HEARD.contains(&k))
}

/// An origin that names no site (`"null"`).
pub fn is_opaque(origin: &str) -> bool {
    origin.is_empty() || origin == "null"
}

/// Check a `vectorcraft:open` the bridge accepted: it needs a file name and the file's bytes (an
/// empty file is passed on: opening it reports the problem like any unreadable file). The error is
/// the text of the `vectorcraft:failed` reply.
pub fn open(name: Option<String>, bytes: Option<Vec<u8>>) -> Result<Open, String> {
    let Some(name) = name.filter(|n| !n.trim().is_empty()) else {
        return Err("vectorcraft:open needs the file's name (`name`, a non-empty string)".to_string());
    };
    let Some(bytes) = bytes else {
        return Err(format!("vectorcraft:open of {name} needs the file's contents (`bytes`, an ArrayBuffer or Uint8Array)"));
    };
    Ok(Open { name, bytes })
}

/// Check a `vectorcraft:export` the bridge accepted: `format` is optional (PDF without it) but must
/// be one the app writes. The error is the text of the `vectorcraft:failed` reply.
pub fn export(
    format: Option<String>,
    mark_saved: Option<bool>,
    selected_only: Option<bool>,
    preserve_editing: Option<bool>,
    image_only: Option<bool>,
) -> Result<Export, String> {
    let asked = format.unwrap_or_else(|| "pdf".to_string());
    let f = vectorcraft_engine::cmd::fileio::writable_format(Some(asked.trim()), None)
        .map_err(|e| format!("vectorcraft:export can't write `{asked}`: {e}"))?;
    let extension = f.extensions.first().copied().unwrap_or(f.id);
    let image_only = image_only.unwrap_or(false);
    if image_only && !is_raster(f.id) {
        return Err(format!("vectorcraft:export: imageOnly gives an image's own pixels: ask for a raster format such as png, not `{}`", f.id));
    }
    let (mark_saved, selected_only) = (mark_saved.unwrap_or(false), selected_only.unwrap_or(false) || image_only);
    // A part of the document isn't the document: it can't count as saving it.
    if mark_saved && selected_only {
        return Err("vectorcraft:export: markSaved can't go with selectedOnly".to_string());
    }
    // Only a PDF carries the native document: refused elsewhere rather than ignored.
    if preserve_editing.is_some() && f.id != "pdf" {
        return Err(format!("vectorcraft:export: preserveEditing is for pdf, not `{}`", f.id));
    }
    Ok(Export { format: f.id, extension, mark_saved, selected_only, preserve_editing, image_only, preserve_source: false })
}

/// The host may explicitly require source-colour preservation. Unknown options fail closed.
pub fn with_output(mut export: Export, output: Option<&str>) -> Result<Export, String> {
    let Some(text) = output else { return Ok(export) };
    let value = serde_json::from_str::<serde_json::Value>(text).map_err(|_| "vectorcraft:export: invalid output policy")?;
    if export.format != "pdf" || value != serde_json::json!({"conversion": "preserveSource"}) {
        return Err("vectorcraft:export: output must be {conversion: preserveSource} for PDF".into());
    }
    export.preserve_source = true;
    Ok(export)
}

/// Check a `vectorcraft:prefs` the bridge accepted: `prefs` is the text a `vectorcraft:prefs` from
/// the editor gave (a JSON object), handed back as it was. The error is the text of the
/// `vectorcraft:failed` reply.
pub fn prefs(text: Option<String>) -> Result<String, String> {
    let Some(text) = text else {
        return Err("vectorcraft:prefs needs `prefs`, the text a vectorcraft:prefs from the editor gave".to_string());
    };
    if text.len() > MAX_PREFS {
        return Err(format!("vectorcraft:prefs: `prefs` is longer than {MAX_PREFS} bytes"));
    }
    if !serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|v| v.is_object()) {
        return Err("vectorcraft:prefs: `prefs` isn't the text a vectorcraft:prefs from the editor gave".to_string());
    }
    Ok(text)
}

/// Check a `vectorcraft:place` the bridge accepted: a file name, its bytes, and `at` (when given) as
/// four finite numbers with a positive width and height. The error is the text of the
/// `vectorcraft:failed` reply.
#[cfg(test)]
pub fn place(name: Option<String>, bytes: Option<Vec<u8>>, at: Option<Vec<f64>>) -> Result<Place, String> {
    place_copies(name, bytes, at, None)
}

/// [`place`], and `copies` (a boolean; with it, no `at`: copies replace images where they are).
pub fn place_copies(name: Option<String>, bytes: Option<Vec<u8>>, at: Option<Vec<f64>>, copies: Option<bool>) -> Result<Place, String> {
    let Some(name) = name.filter(|n| !n.trim().is_empty()) else {
        return Err("vectorcraft:place needs the file's name (`name`, a non-empty string)".to_string());
    };
    let Some(bytes) = bytes else {
        return Err(format!("vectorcraft:place of {name} needs the file's contents (`bytes`, an ArrayBuffer or Uint8Array)"));
    };
    let at = match at.as_deref() {
        None => None,
        Some(&[x, y, w, h]) if [x, y, w, h].iter().all(|v| v.is_finite()) && w > 0.0 && h > 0.0 => Some([x, y, w, h]),
        Some(_) => return Err("vectorcraft:place: `at` must be {x, y, width, height} with a positive width and height".to_string()),
    };
    let copies = copies.unwrap_or(false);
    if copies && at.is_some() {
        return Err("vectorcraft:place: `copies` replaces the selected image's copies where they are: no `at`".to_string());
    }
    Ok(Place { name, bytes, at, copies })
}

/// What a `vectorcraft:solidify` asks for: the one selected image's soft pixels removed (every
/// alpha at or above `level` made opaque, below it clear; `object.image.solidifyAlpha`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Solidify {
    /// 1–255 on the 8-bit alpha scale (128 when the message leaves it out).
    pub level: u8,
    /// Also every other copy of the image: the same pixels and transparency at the same printed
    /// size, in the same undo step.
    pub copies: bool,
}

/// Check a `vectorcraft:solidify` the bridge accepted. Each field is `None` when the message
/// leaves it out (or gives `undefined`), `Some(None)` when it is there but of the wrong type. The
/// error is the text of the `vectorcraft:failed` reply.
pub fn solidify(level: Option<Option<f64>>, copies: Option<Option<bool>>) -> Result<Solidify, String> {
    let level = match level {
        None => 128,
        Some(Some(l)) if l.is_finite() && l.fract() == 0.0 && (1.0..=255.0).contains(&l) => l as u8,
        Some(_) => return Err("vectorcraft:solidify: level must be a whole number from 1 to 255".to_string()),
    };
    let copies = match copies {
        None => false,
        Some(Some(c)) => c,
        Some(None) => return Err("vectorcraft:solidify: copies must be a boolean".to_string()),
    };
    Ok(Solidify { level, copies })
}

/// The most ICC profiles one `vectorcraft:colorSettings` loads, and the largest (bytes; as
/// `color.loadProfile` reads).
pub const MAX_PROFILES: usize = 16;
pub const MAX_PROFILE_BYTES: usize = 32 << 20;
/// The most bytes the profiles of one `vectorcraft:colorSettings` take together.
pub const MAX_PROFILES_BYTES: usize = 64 << 20;

/// Whether a `vectorcraft:colorSettings`'s profiles fit the limits, told from their count and
/// sizes alone (`None`: an entry without bytes, which [`color_settings`] refuses), so the bridge
/// can refuse them before copying any out of the page. The error is the text of the
/// `vectorcraft:failed` reply.
pub fn profiles_fit(count: usize, sizes: impl IntoIterator<Item = Option<usize>>) -> Result<(), String> {
    const C: &str = "vectorcraft:colorSettings";
    if count > MAX_PROFILES {
        return Err(format!("{C}: at most {MAX_PROFILES} profiles"));
    }
    let mut total = 0usize;
    for (i, size) in sizes.into_iter().enumerate() {
        let size = size.unwrap_or(0);
        if size > MAX_PROFILE_BYTES {
            return Err(format!("{C}: profiles[{i}] is larger than {MAX_PROFILE_BYTES} bytes"));
        }
        total = total.saturating_add(size);
        if total > MAX_PROFILES_BYTES {
            return Err(format!("{C}: the profiles take more than {MAX_PROFILES_BYTES} bytes together"));
        }
    }
    Ok(())
}

/// What a `vectorcraft:colorSettings` asks for: ICC profiles registered (each under `name`, else
/// its own description), then the working spaces, intent and black-point compensation set, as
/// Edit › Color Settings sets them. A field left out stays as it is.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorSettings {
    pub profiles: Vec<(Option<String>, Vec<u8>)>,
    pub rgb: Option<String>,
    pub cmyk: Option<String>,
    pub intent: Option<vectorcraft_color::cms::Intent>,
    pub bpc: Option<bool>,
}

impl ColorSettings {
    /// The `edit.colorSettings` params for the fields given.
    pub fn params(&self) -> serde_json::Value {
        let mut p = serde_json::Map::new();
        if let Some(v) = &self.rgb {
            p.insert("rgb".into(), v.clone().into());
        }
        if let Some(v) = &self.cmyk {
            p.insert("cmyk".into(), v.clone().into());
        }
        if let Some(i) = self.intent {
            p.insert("intent".into(), i.id().into());
        }
        if let Some(b) = self.bpc {
            p.insert("bpc".into(), b.into());
        }
        p.into()
    }
}

/// Check a `vectorcraft:colorSettings` the bridge accepted. `profiles`: each entry's `name` (`None`
/// left out, `Some(None)` not a string) and `bytes` (`None`: missing or not bytes); `None` for no
/// `profiles` field, `Some(None)` for one that isn't an array. The other fields as for
/// [`solidify`]: `None` left out, `Some(None)` of the wrong type. The error is the text of the
/// `vectorcraft:failed` reply.
#[allow(clippy::type_complexity)]
pub fn color_settings(
    profiles: Option<Option<Vec<(Option<Option<String>>, Option<Vec<u8>>)>>>,
    rgb: Option<Option<String>>,
    cmyk: Option<Option<String>>,
    intent: Option<Option<String>>,
    bpc: Option<Option<bool>>,
) -> Result<ColorSettings, String> {
    const C: &str = "vectorcraft:colorSettings";
    let profiles = match profiles {
        None => vec![],
        Some(None) => return Err(format!("{C}: profiles must be an array of {{name?, bytes}}")),
        Some(Some(list)) => {
            profiles_fit(list.len(), list.iter().map(|(_, b)| b.as_ref().map(Vec::len)))?;
            let mut out = vec![];
            for (i, (name, bytes)) in list.into_iter().enumerate() {
                let name = match name {
                    None => None,
                    Some(Some(n)) if !n.trim().is_empty() => Some(n.trim().to_string()),
                    Some(_) => return Err(format!("{C}: profiles[{i}].name must be a non-empty string")),
                };
                let Some(bytes) = bytes else {
                    return Err(format!("{C}: profiles[{i}] needs the profile's contents (`bytes`, an ArrayBuffer or Uint8Array)"));
                };
                out.push((name, bytes));
            }
            out
        }
    };
    let text = |key: &str, v: Option<Option<String>>| match v {
        None => Ok(None),
        Some(Some(t)) if !t.trim().is_empty() => Ok(Some(t)),
        Some(_) => Err(format!("{C}: {key} must name a profile")),
    };
    let (rgb, cmyk) = (text("rgb", rgb)?, text("cmyk", cmyk)?);
    let intent = match intent {
        None => None,
        Some(Some(i)) if vectorcraft_color::cms::Intent::parse(&i).is_some() => vectorcraft_color::cms::Intent::parse(&i),
        Some(_) => return Err(format!("{C}: intent must be perceptual, relative, saturation or absolute")),
    };
    let bpc = match bpc {
        None => None,
        Some(Some(b)) => Some(b),
        Some(None) => return Err(format!("{C}: bpc must be a boolean")),
    };
    if profiles.is_empty() && rgb.is_none() && cmyk.is_none() && intent.is_none() && bpc.is_none() {
        return Err(format!("{C}: nothing to set (profiles, rgb, cmyk, intent or bpc)"));
    }
    Ok(ColorSettings { profiles, rgb, cmyk, intent, bpc })
}

/// Check a `vectorcraft:open`'s `units` or `strokeUnits` (`field`): left out (`None`), or a string
/// naming a unit as Preferences ▸ Units name them: its key, label or suffix in any case
/// (`inches`, `Millimeters`, `mm`, `in`; [`vectorcraft_doc::Unit::named`]). `Some(None)` is a value
/// that isn't a string. The error is the text of the `vectorcraft:failed` reply.
pub fn open_unit(field: &str, given: Option<Option<String>>) -> Result<Option<vectorcraft_doc::Unit>, String> {
    let Some(given) = given else { return Ok(None) };
    match given.as_deref().and_then(vectorcraft_doc::Unit::named) {
        Some(unit) => Ok(Some(unit)),
        None => Err(format!("vectorcraft:open: {field} must name a unit, such as inches or millimeters")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE: &str = "https://cloud.example.com";
    const OPEN: Option<&str> = Some("vectorcraft:open");

    #[test]
    fn accepts_an_open_from_the_parent_on_the_same_origin() {
        assert!(accepts(SITE, SITE, true, OPEN));
    }

    #[test]
    fn ignores_messages_from_another_origin() {
        for origin in [
            "https://evil.example",
            "http://cloud.example.com",
            "https://cloud.example.com:8443",
            "https://cloud.example.com.evil.example",
            "https://sub.cloud.example.com",
            "null",
            "",
        ] {
            assert!(!accepts(SITE, origin, true, OPEN), "{origin}");
        }
    }

    #[test]
    fn ignores_another_window_on_the_same_origin() {
        // Such as another tab of the same site, or a frame the parent page holds.
        assert!(!accepts(SITE, SITE, false, OPEN));
    }

    #[test]
    fn accepts_an_export_from_the_parent_on_the_same_origin() {
        assert!(accepts(SITE, SITE, true, Some("vectorcraft:export")));
        assert!(!accepts(SITE, "https://evil.example", true, Some("vectorcraft:export")));
        assert!(!accepts(SITE, SITE, false, Some("vectorcraft:export")));
    }

    #[test]
    fn exports_pdf_unless_asked_for_another_format() {
        assert_eq!(
            export(None, None, None, None, None),
            Ok(Export {
                format: "pdf",
                extension: "pdf",
                mark_saved: false,
                selected_only: false,
                preserve_editing: None,
                image_only: false,
                preserve_source: false
            })
        );
        assert_eq!(
            export(Some("svg".into()), Some(true), None, None, None),
            Ok(Export {
                format: "svg",
                extension: "svg",
                mark_saved: true,
                selected_only: false,
                preserve_editing: None,
                image_only: false,
                preserve_source: false
            })
        );
        // An extension names its format too.
        assert_eq!(export(Some("png".into()), None, None, None, None).map(|e| e.format), Ok("png"));
    }

    #[test]
    fn refuses_an_export_in_a_format_the_app_doesnt_write() {
        for format in ["", "docx", "PDF but not really", "vectorcraft:pdf"] {
            let error = export(Some(format.to_string()), None, None, None, None).err();
            assert!(error.as_deref().is_some_and(|e| e.starts_with("vectorcraft:export can't write")), "{format:?}: {error:?}");
        }
    }

    #[test]
    fn accepts_a_place_from_the_parent_on_the_same_origin() {
        assert!(accepts(SITE, SITE, true, Some("vectorcraft:place")));
        assert!(!accepts(SITE, "https://evil.example", true, Some("vectorcraft:place")));
        assert!(!accepts(SITE, SITE, false, Some("vectorcraft:place")));
    }

    #[test]
    fn an_image_only_export_needs_one_image_and_says_what_else_is_selected() {
        let e = export(Some("png".into()), None, None, None, Some(true)).unwrap();
        assert!(e.image_only && e.selected_only, "imageOnly is about the selection");
        assert!(export(Some("pdf".into()), None, None, None, Some(true)).is_err_and(|e| e.contains("raster")));
        assert!(export(Some("png".into()), Some(true), None, None, Some(true)).is_err_and(|e| e.contains("markSaved")));
        assert_eq!(image_only_refusal(&[Picked::Image]), None);
        for (picked, why) in [
            (vec![], "nothing is selected"),
            (vec![Picked::Group], "a group is selected"),
            (vec![Picked::Path], "a path is selected"),
            (vec![Picked::Type], "type is selected"),
            (vec![Picked::Other], "isn't an image"),
            (vec![Picked::Image, Picked::Image], "2 objects are selected"),
        ] {
            let e = image_only_refusal(&picked).unwrap();
            assert!(e.starts_with("vectorcraft:export: select one image (") && e.contains(why), "{picked:?}: {e}");
        }
    }

    #[test]
    fn leaves_the_editing_data_out_of_a_pdf_when_asked() {
        assert_eq!(export(None, Some(true), None, Some(false), None).map(|e| e.preserve_editing), Ok(Some(false)));
        assert_eq!(export(None, None, None, None, None).map(|e| e.preserve_editing), Ok(None));
        assert!(export(Some("svg".into()), None, None, Some(false), None).is_err_and(|e| e.contains("preserveEditing")));
    }

    #[test]
    fn exports_the_selection_but_not_as_the_saved_document() {
        assert_eq!(export(Some("png".into()), None, Some(true), None, None).map(|e| (e.format, e.selected_only)), Ok(("png", true)));
        assert!(export(Some("png".into()), Some(true), Some(true), None, None).is_err_and(|e| e.contains("markSaved")));
        assert_eq!(export(None, Some(true), None, None, None).map(|e| e.selected_only), Ok(false));
    }

    #[test]
    fn places_a_named_file_in_a_rect() {
        let p = place(Some("traced.svg".into()), Some(b"<svg/>".to_vec()), Some(vec![10.0, 20.0, 30.0, 40.0])).unwrap();
        assert_eq!((p.name.as_str(), p.at), ("traced.svg", Some([10.0, 20.0, 30.0, 40.0])));
        assert_eq!(place(Some("t.svg".into()), Some(vec![]), None).map(|p| p.at), Ok(None));
    }

    #[test]
    fn refuses_a_place_without_a_name_bytes_or_a_good_rect() {
        assert!(place(None, Some(vec![1]), None).is_err_and(|e| e.contains("`name`")));
        assert!(place(Some("t.svg".into()), None, None).is_err_and(|e| e.contains("`bytes`")));
        for at in [vec![1.0, 2.0, 3.0], vec![0.0, 0.0, 0.0, 10.0], vec![0.0, 0.0, 10.0, -1.0], vec![f64::NAN, 0.0, 1.0, 1.0]] {
            assert!(place(Some("t.svg".into()), Some(vec![1]), Some(at.clone())).is_err_and(|e| e.contains("`at`")), "{at:?}");
        }
    }

    #[test]
    fn takes_back_the_preferences_it_gave() {
        assert!(accepts(SITE, SITE, true, Some("vectorcraft:prefs")));
        assert_eq!(prefs(Some(r#"{"engine_prefs":{"units":"mm"}}"#.into())), Ok(r#"{"engine_prefs":{"units":"mm"}}"#.into()));
        for bad in [None, Some(String::new()), Some("[]".into()), Some("not json".into()), Some("x".repeat(MAX_PREFS + 1))] {
            assert!(prefs(bad.clone()).is_err(), "{:?}", bad.map(|b| b.chars().take(20).collect::<String>()));
        }
    }

    #[test]
    fn ignores_other_message_types() {
        for kind in [
            None,
            Some(""),
            Some("vectorcraft:save"),
            Some("vectorcraft:ready"),
            Some("VECTORCRAFT:OPEN"),
            Some("photocraft:open"),
            Some("vectorcraft:open "),
        ] {
            assert!(!accepts(SITE, SITE, true, kind), "{kind:?}");
        }
    }

    #[test]
    fn hears_nothing_on_an_opaque_origin() {
        assert!(!accepts("null", "null", true, OPEN));
        assert!(!accepts("", "", true, OPEN));
        assert!(is_opaque("null"));
        assert!(!is_opaque(SITE));
    }

    #[test]
    fn opens_a_named_file() {
        assert_eq!(open(Some("Logo.svg".into()), Some(b"<svg/>".to_vec())), Ok(Open { name: "Logo.svg".into(), bytes: b"<svg/>".to_vec() }));
        // An empty file is the importer's to report.
        assert_eq!(open(Some("Empty.svg".into()), Some(Vec::new())), Ok(Open { name: "Empty.svg".into(), bytes: Vec::new() }));
    }

    #[test]
    fn refuses_an_open_without_a_name() {
        for name in [None, Some(String::new()), Some("   ".to_string()), Some("\t\n".to_string())] {
            let error = open(name.clone(), Some(b"<svg/>".to_vec())).err();
            assert!(error.as_deref().is_some_and(|e| e.contains("`name`")), "{name:?}: {error:?}");
        }
    }

    #[test]
    fn refuses_an_open_without_bytes() {
        let error = open(Some("Logo.svg".into()), None).err();
        assert!(error.as_deref().is_some_and(|e| e.contains("`bytes`") && e.contains("Logo.svg")), "{error:?}");
    }

    #[test]
    fn accepts_a_solidify_from_the_parent_on_the_same_origin() {
        assert!(accepts(SITE, SITE, true, Some("vectorcraft:solidify")));
        assert!(!accepts(SITE, "https://evil.example", true, Some("vectorcraft:solidify")));
        assert!(!accepts(SITE, SITE, false, Some("vectorcraft:solidify")));
        assert!(!accepts(SITE, SITE, true, Some("vectorcraft:solidified")), "the answer isn't heard");
    }

    #[test]
    fn a_solidify_defaults_to_level_128_on_one_image_and_refuses_bad_fields() {
        assert_eq!(solidify(None, None), Ok(Solidify { level: 128, copies: false }));
        assert_eq!(solidify(Some(Some(1.0)), Some(Some(true))), Ok(Solidify { level: 1, copies: true }));
        assert_eq!(solidify(Some(Some(255.0)), Some(Some(false))), Ok(Solidify { level: 255, copies: false }));
        for bad in [Some(0.0), Some(256.0), Some(12.5), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY), None] {
            let e = solidify(Some(bad), None).unwrap_err();
            assert_eq!(e, "vectorcraft:solidify: level must be a whole number from 1 to 255", "{bad:?}");
        }
        assert_eq!(solidify(None, Some(None)).unwrap_err(), "vectorcraft:solidify: copies must be a boolean");
    }

    #[test]
    fn accepts_color_settings_from_the_parent_on_the_same_origin() {
        assert!(accepts(SITE, SITE, true, Some("vectorcraft:colorSettings")));
        assert!(!accepts(SITE, "https://evil.example", true, Some("vectorcraft:colorSettings")));
        assert!(!accepts(SITE, SITE, false, Some("vectorcraft:colorSettings")));
        assert!(!accepts(SITE, SITE, true, Some("vectorcraft:colorSettingsApplied")), "the answer isn't heard");
    }

    #[test]
    fn color_settings_take_profiles_spaces_intent_and_compensation() {
        use vectorcraft_color::cms::Intent;
        let s = color_settings(
            Some(Some(vec![(None, Some(vec![1, 2])), (Some(Some(" Press ".into())), Some(vec![3]))])),
            Some(Some("Adobe RGB (1998)".into())),
            Some(Some("U.S. Web Coated (SWOP) v2".into())),
            Some(Some("perceptual".into())),
            Some(Some(true)),
        )
        .unwrap();
        assert_eq!(s.profiles, vec![(None, vec![1, 2]), (Some("Press".into()), vec![3])]);
        assert_eq!(s.intent, Some(Intent::Perceptual));
        assert_eq!(
            s.params(),
            serde_json::json!({"rgb": "Adobe RGB (1998)", "cmyk": "U.S. Web Coated (SWOP) v2", "intent": "perceptual", "bpc": true})
        );
        // A field left out stays as it is.
        let only = color_settings(None, None, None, None, Some(Some(false))).unwrap();
        assert_eq!((only.profiles.len(), only.params()), (0, serde_json::json!({"bpc": false})));
    }

    #[test]
    fn color_settings_refuse_bad_fields_and_an_empty_message() {
        let e = |r: Result<ColorSettings, String>| r.unwrap_err();
        assert!(e(color_settings(None, None, None, None, None)).contains("nothing to set"));
        assert!(e(color_settings(Some(None), None, None, None, None)).contains("profiles must be an array"));
        assert!(e(color_settings(Some(Some(vec![(None, None)])), None, None, None, None)).contains("profiles[0] needs"));
        assert!(e(color_settings(Some(Some(vec![(Some(None), Some(vec![1]))])), None, None, None, None)).contains("name must be"));
        assert!(e(color_settings(Some(Some(vec![(None, Some(vec![0; MAX_PROFILE_BYTES + 1]))])), None, None, None, None)).contains("larger"));
        let many = (0..=MAX_PROFILES).map(|_| (None, Some(vec![1]))).collect();
        assert!(e(color_settings(Some(Some(many)), None, None, None, None)).contains("at most"));
        // Sized before being copied out of the page; several large ones together are too much.
        assert!(profiles_fit(MAX_PROFILES + 1, []).is_err_and(|e| e.contains("at most")));
        assert!(profiles_fit(1, [Some(MAX_PROFILE_BYTES + 1)]).is_err_and(|e| e.contains("larger")));
        assert!(profiles_fit(3, [Some(MAX_PROFILE_BYTES); 3]).is_err_and(|e| e.contains("together")));
        assert_eq!(profiles_fit(2, [Some(MAX_PROFILE_BYTES), None]), Ok(()));
        assert!(e(color_settings(None, Some(None), None, None, None)).contains("rgb must name"));
        assert!(e(color_settings(None, None, Some(Some(" ".into())), None, None)).contains("cmyk must name"));
        assert!(e(color_settings(None, None, None, Some(Some("vivid".into())), None)).contains("intent must be"));
        assert!(e(color_settings(None, None, None, None, Some(None))).contains("bpc must be"));
    }

    #[test]
    fn reads_the_units_an_open_names_and_refuses_a_bad_one() {
        use vectorcraft_doc::Unit;
        assert_eq!(open_unit("units", None), Ok(None), "left out: the file's or the person's units");
        for (given, unit) in [
            ("inches", Unit::Inches),
            ("Inches", Unit::Inches),
            ("in", Unit::Inches),
            ("millimeters", Unit::Millimeters),
            ("mm", Unit::Millimeters),
            ("points", Unit::Points),
        ] {
            assert_eq!(open_unit("units", Some(Some(given.into()))), Ok(Some(unit)), "{given}");
        }
        for bad in [Some("furlongs".to_string()), Some(String::new()), None] {
            let e = open_unit("strokeUnits", Some(bad.clone())).unwrap_err();
            assert!(e.starts_with("vectorcraft:open: strokeUnits must name a unit"), "{bad:?}: {e}");
        }
    }
}

/// Only selection-based mark commands are exposed to the parent; arbitrary commands are not.
pub fn mark_command(kind: &str, mark: Option<serde_json::Value>) -> Result<(&'static str, serde_json::Value), String> {
    match kind {
        "vectorcraft:sep.mark.set" => {
            let mark: vectorcraft_doc::SepMark = serde_json::from_value(mark.unwrap_or(serde_json::Value::Null)).map_err(|e| e.to_string())?;
            if !mark.valid() {
                return Err("invalid separation mark list".into());
            }
            Ok(("sep.mark.set", serde_json::json!({"mark":mark})))
        }
        "vectorcraft:sep.mark.clear" => Ok(("sep.mark.clear", serde_json::json!({}))),
        "vectorcraft:sep.mark.list" => Ok(("sep.mark.list", serde_json::json!({}))),
        _ => Err("unknown separation mark message".into()),
    }
}

#[cfg(test)]
mod mark_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn mark_messages_are_parent_only_and_validate_payloads() {
        for kind in ["vectorcraft:sep.mark.set", "vectorcraft:sep.mark.clear", "vectorcraft:sep.mark.list"] {
            assert!(accepts("https://example.test", "https://example.test", true, Some(kind)));
            assert!(!accepts("https://example.test", "https://other.test", true, Some(kind)));
            assert!(!accepts("https://example.test", "https://example.test", false, Some(kind)));
        }
        assert!(mark_command("vectorcraft:sep.mark.set", None).is_err());
        assert!(mark_command("vectorcraft:sep.mark.set", Some(json!({"type":"metallic","list":"../outside"}))).is_err());
        assert_eq!(
            mark_command("vectorcraft:sep.mark.set", Some(json!({"type":"noTrap"}))).unwrap(),
            ("sep.mark.set", json!({"mark":{"type":"noTrap"}}))
        );
        assert_eq!(mark_command("vectorcraft:sep.mark.clear", None).unwrap(), ("sep.mark.clear", json!({})));
    }
}
