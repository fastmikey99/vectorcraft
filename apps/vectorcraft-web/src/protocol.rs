//! The rules of the embedding protocol (`host`, `docs/web-embed.md`), kept free of browser types so
//! they are tested natively: which messages the bridge reads, and what a `vectorcraft:open` or a
//! `vectorcraft:export` must carry.

/// What a `vectorcraft:open` delivers: a file to open like one picked with File › Open.
#[derive(Debug, PartialEq, Eq)]
pub struct Open {
    /// The file name: the document's name, and the name it is saved under.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// The messages the bridge reads.
pub const HEARD: [&str; 2] = ["vectorcraft:open", "vectorcraft:export"];

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
pub fn export(format: Option<String>, mark_saved: Option<bool>) -> Result<Export, String> {
    let asked = format.unwrap_or_else(|| "pdf".to_string());
    let f = vectorcraft_engine::cmd::fileio::writable_format(Some(asked.trim()), None)
        .map_err(|e| format!("vectorcraft:export can't write `{asked}`: {e}"))?;
    let extension = f.extensions.first().copied().unwrap_or(f.id);
    Ok(Export { format: f.id, extension, mark_saved: mark_saved.unwrap_or(false) })
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
        assert_eq!(export(None, None), Ok(Export { format: "pdf", extension: "pdf", mark_saved: false }));
        assert_eq!(export(Some("svg".into()), Some(true)), Ok(Export { format: "svg", extension: "svg", mark_saved: true }));
        // An extension names its format too.
        assert_eq!(export(Some("png".into()), None).map(|e| e.format), Ok("png"));
    }

    #[test]
    fn refuses_an_export_in_a_format_the_app_doesnt_write() {
        for format in ["", "docx", "PDF but not really", "vectorcraft:pdf"] {
            let error = export(Some(format.to_string()), None).err();
            assert!(error.as_deref().is_some_and(|e| e.starts_with("vectorcraft:export can't write")), "{format:?}: {error:?}");
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
