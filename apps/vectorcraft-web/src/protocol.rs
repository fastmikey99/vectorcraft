//! The rules of the embedding protocol (`host`, `docs/web-embed.md`), kept free of browser types so
//! they are tested natively: which messages the bridge reads, and what a `vectorcraft:open`, a
//! `vectorcraft:export` or a `vectorcraft:place` must carry.

/// What a `vectorcraft:open` delivers: a file to open like one picked with File › Open.
#[derive(Debug, PartialEq, Eq)]
pub struct Open {
    /// The file name: the document's name, and the name it is saved under.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// The messages the bridge reads.
pub const HEARD: [&str; 4] = ["vectorcraft:open", "vectorcraft:export", "vectorcraft:place", "vectorcraft:prefs"];

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
}

/// What a `vectorcraft:place` delivers: a file placed into the open document in place of the one
/// selected object (or, with nothing selected, at `at`), fitted inside `at` (default: the selected
/// object's bounds), aspect kept.
#[derive(Debug, PartialEq)]
pub struct Place {
    pub name: String,
    pub bytes: Vec<u8>,
    /// `[x, y, width, height]` in document points (as `bounds` in a `vectorcraft:save` gives them).
    pub at: Option<[f64; 4]>,
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
) -> Result<Export, String> {
    let asked = format.unwrap_or_else(|| "pdf".to_string());
    let f = vectorcraft_engine::cmd::fileio::writable_format(Some(asked.trim()), None)
        .map_err(|e| format!("vectorcraft:export can't write `{asked}`: {e}"))?;
    let extension = f.extensions.first().copied().unwrap_or(f.id);
    let (mark_saved, selected_only) = (mark_saved.unwrap_or(false), selected_only.unwrap_or(false));
    // A part of the document isn't the document: it can't count as saving it.
    if mark_saved && selected_only {
        return Err("vectorcraft:export: markSaved can't go with selectedOnly".to_string());
    }
    // Only a PDF carries the native document: refused elsewhere rather than ignored.
    if preserve_editing.is_some() && f.id != "pdf" {
        return Err(format!("vectorcraft:export: preserveEditing is for pdf, not `{}`", f.id));
    }
    Ok(Export { format: f.id, extension, mark_saved, selected_only, preserve_editing })
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
pub fn place(name: Option<String>, bytes: Option<Vec<u8>>, at: Option<Vec<f64>>) -> Result<Place, String> {
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
    Ok(Place { name, bytes, at })
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
            export(None, None, None, None),
            Ok(Export { format: "pdf", extension: "pdf", mark_saved: false, selected_only: false, preserve_editing: None })
        );
        assert_eq!(
            export(Some("svg".into()), Some(true), None, None),
            Ok(Export { format: "svg", extension: "svg", mark_saved: true, selected_only: false, preserve_editing: None })
        );
        // An extension names its format too.
        assert_eq!(export(Some("png".into()), None, None, None).map(|e| e.format), Ok("png"));
    }

    #[test]
    fn refuses_an_export_in_a_format_the_app_doesnt_write() {
        for format in ["", "docx", "PDF but not really", "vectorcraft:pdf"] {
            let error = export(Some(format.to_string()), None, None, None).err();
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
    fn leaves_the_editing_data_out_of_a_pdf_when_asked() {
        assert_eq!(export(None, Some(true), None, Some(false)).map(|e| e.preserve_editing), Ok(Some(false)));
        assert_eq!(export(None, None, None, None).map(|e| e.preserve_editing), Ok(None));
        assert!(export(Some("svg".into()), None, None, Some(false)).is_err_and(|e| e.contains("preserveEditing")));
    }

    #[test]
    fn exports_the_selection_but_not_as_the_saved_document() {
        assert_eq!(export(Some("png".into()), None, Some(true), None).map(|e| (e.format, e.selected_only)), Ok(("png", true)));
        assert!(export(Some("png".into()), Some(true), Some(true), None).is_err_and(|e| e.contains("markSaved")));
        assert_eq!(export(None, Some(true), None, None).map(|e| e.selected_only), Ok(false));
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
}
