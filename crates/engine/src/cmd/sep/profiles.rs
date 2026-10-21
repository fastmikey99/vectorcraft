//! Local ICC discovery by description; no profile bytes are shipped with the player.
use super::{Player, Result, error};
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use vectorcraft_color::cms::IccProfile;
use vectorcraft_color::{
    Color,
    cms::{self, Cms, ProfileKind},
};
use vectorcraft_doc::Document;

pub(super) fn folder(params: &serde_json::Value) -> Result<Option<PathBuf>> {
    match params.get("profiles") {
        Some(serde_json::Value::String(v)) if !v.is_empty() => Ok(Some(PathBuf::from(v))),
        Some(_) => Err(error("profiles must be a nonempty folder path")),
        None => Ok(std::env::var_os("VECTORCRAFT_PROFILES").filter(|v| !v.is_empty()).map(PathBuf::from)),
    }
}

fn normalized(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Exact description wins; tolerate case/spacing/punctuation and an omitted parenthetical
/// qualifier. Never use arbitrary prefixes (e.g. a different numbered printing condition).
fn rank(want: &str, description: &str) -> Option<u8> {
    let want = normalized(want);
    if want.is_empty() {
        return None;
    }
    if want == normalized(description) {
        return Some(0);
    }
    description.split_once('(').filter(|(base, _)| want == normalized(base)).map(|_| 1)
}

fn choose(want: &str, candidates: Vec<(String, PathBuf)>) -> Result<Option<(String, PathBuf)>> {
    let mut matches: Vec<_> = candidates.into_iter().filter_map(|(name, path)| rank(want, &name).map(|r| (r, name, path))).collect();
    matches.sort();
    let Some((r, name, path)) = matches.first() else { return Ok(None) };
    if matches.get(1).is_some_and(|other| other.0 == *r) {
        return Err(error(format!("ambiguous document profile: {want}")));
    }
    Ok(Some((name.clone(), path.clone())))
}

#[cfg(not(target_arch = "wasm32"))]
fn discover(folder: &Path, want: &str) -> Result<Option<(String, PathBuf)>> {
    crate::file_access::check_read(&folder.to_string_lossy()).map_err(error)?;
    let mut candidates = Vec::new();
    // One folder, bounded entries and individually capped reads. Unrelated/malformed ICC files
    // do not prevent finding a usable profile. Only the chosen file is registered/remembered.
    for (i, entry) in std::fs::read_dir(folder).map_err(error)?.enumerate() {
        if i >= 4096 {
            return Err(error("profile folder exceeds 4096 entries"));
        }
        let path = entry.map_err(error)?.path();
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("icc") || e.eq_ignore_ascii_case("icm")) {
            continue;
        }
        crate::file_access::check_read(&path.to_string_lossy()).map_err(error)?;
        // Skip directories, devices and FIFOs before opening an entry as a profile.
        if !std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= crate::store::MAX_PROFILE_FILE) {
            continue;
        }
        let Ok(bytes) = crate::store::read_capped(&path, crate::store::MAX_PROFILE_FILE) else { continue };
        let Ok(profile) = IccProfile::from_bytes(None, &bytes) else { continue };
        if profile.kind == ProfileKind::Cmyk && rank(want, &profile.name).is_some() {
            candidates.push((profile.name, path));
        }
    }
    choose(want, candidates)
}
#[cfg(target_arch = "wasm32")]
fn discover(_: &Path, _: &str) -> Result<Option<(String, PathBuf)>> {
    Err(error("PAL profile folders are unavailable on the web"))
}

impl Player<'_> {
    pub(super) fn resolve_profile(&mut self, want: &str) -> Result<Option<String>> {
        if let Some(folder) = &self.profiles {
            if let Some((_, path)) = discover(folder, want)? {
                let loaded = self.session.execute("color.loadProfile", &serde_json::json!({"path":path}))?;
                return Ok(loaded["name"].as_str().map(str::to_owned));
            }
            // An explicitly configured folder is authoritative, including when it has no match.
            return Ok(None);
        }
        choose(want, cms::profiles().into_iter().filter(|p| p.kind == ProfileKind::Cmyk).map(|p| (p.name, PathBuf::new())).collect())
            .map(|p| p.map(|(name, _)| name))
    }
}

pub(in crate::cmd) fn document_cms(doc: &Document) -> Option<Cms> {
    let mut settings = cms::active_settings();
    settings.cmyk = doc.color_profiles.cmyk.clone()?;
    if let Some(rgb) = &doc.color_profiles.rgb {
        settings.rgb = rgb.clone();
    }
    // An unavailable imported profile keeps the player's previous profile-free fallback.
    // AssignDocumentProfile reports lookup/load failures at the PAL's requested severity.
    Cms::new(&settings).ok()
}

pub(in crate::cmd) fn cmyk(color: Color, cms: Option<&Cms>) -> [f32; 4] {
    let Some(cms) = cms else { return color.to_cmyk() };
    let intent = cms.settings().intent;
    match color {
        Color::Rgb { r, g, b } => cms.rgb_into_cmyk(cms, [r, g, b], intent).unwrap_or_else(|| cms.to_cmyk(&color, intent)),
        _ => cms.to_cmyk(&color, intent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn description_matching_is_tolerant_but_not_a_prefix_guess() {
        assert_eq!(rank("Test Coated42", "Test Coated 42 (ISO 123:2004)"), Some(1));
        assert_eq!(rank("TEST-Coated42 (iso 123 2004)", "Test Coated 42 (ISO 123:2004)"), Some(0));
        assert_eq!(rank("Test Coated4", "Test Coated 42 (ISO 123:2004)"), None);
        assert_eq!(rank("", "Test"), None);
        let candidates = vec![("Test (A)".into(), "a.icc".into()), ("Test (B)".into(), "b.icc".into())];
        assert!(choose("Test", candidates.clone()).is_err());
        assert_eq!(choose("Test (B)", candidates).unwrap().unwrap().1, PathBuf::from("b.icc"));
    }
}
