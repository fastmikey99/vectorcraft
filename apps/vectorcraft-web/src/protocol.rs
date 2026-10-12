//! The rules of the embedding protocol (`host`, `docs/web-embed.md`), kept free of browser types so
//! they are tested natively: which messages the bridge reads, and what a `vectorcraft:open` must
//! carry.

/// What a `vectorcraft:open` delivers: a file to open like one picked with File › Open.
#[derive(Debug, PartialEq, Eq)]
pub struct Open {
    /// The file name: the document's name, and the name it is saved under.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Whether the bridge reads a message at all: a `vectorcraft:open` from the parent window, on the
/// page's own origin. Anything else is ignored without a reply, so another page learns nothing.
///
/// `own_origin` is the page's origin and `origin` the event's; `from_parent` says whether the
/// event's source is the parent window; `kind` is the message's `type`, when it is a string. An
/// opaque origin (`"null"`: a sandboxed frame, a `file:` page) can't be told apart from another
/// page's, so the bridge hears nothing there.
pub fn accepts(own_origin: &str, origin: &str, from_parent: bool, kind: Option<&str>) -> bool {
    from_parent && !is_opaque(own_origin) && origin == own_origin && kind == Some("vectorcraft:open")
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
