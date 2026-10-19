//! Independent runtime adapter. External Ghostscript is not linked or redistributed here.
//! Device contract: https://ghostscript.readthedocs.io/en/gs10.07.0/Devices.html#tiff-file-formats
//! A successful render is evidence, not qualification or an overall Pass.
use crate::{digest, sha256};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub binary: PathBuf,
    pub binary_sha256: String,
    pub version: String,
    pub dpi: u32,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
    /// A private qualification record, never an operator checkbox.
    pub qualification_sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Plane {
    pub page: u32,
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub minimum: u8,
    pub maximum: u8,
}
#[derive(Clone, Debug, Serialize)]
pub struct Render {
    pub pdf_sha256: String,
    pub configuration_sha256: String,
    pub version: String,
    pub planes: Vec<Plane>,
    pub spot_names: Vec<String>,
    pub diagnostics: Vec<String>,
    pub pixel_pitch_mm: f64,
    pub qualified: bool,
}
fn io(e: std::io::Error) -> String {
    e.to_string()
}
/// Process settings are part of the measurement contract, not inherited job input.
/// In particular GS_OPTIONS may contain executable options or additional input files.
fn isolated_command(binary: &Path, output: &Path) -> Command {
    let mut command = Command::new(binary);
    command.env_clear().current_dir(output).env("LC_ALL", "C").env("LANG", "C");
    for name in ["HOME", "USERPROFILE", "TEMP", "TMP", "TMPDIR", "XDG_CACHE_HOME"] {
        command.env(name, output);
    }
    command
}
const MAX_INTERPRETER_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: u64 = 2 * 1024 * 1024;
fn open_regular(path: &Path) -> Result<fs::File, String> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags, open};
        // A replaced executable may be a FIFO. Opening it must never wait for a writer.
        fs::File::from(open(path, OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC, Mode::empty()).map_err(|e| e.to_string())?)
    };
    #[cfg(not(unix))]
    let file = {
        if !fs::metadata(path).map_err(io)?.is_file() {
            return Err("Independent interpreter is not a regular file".into());
        }
        fs::File::open(path).map_err(io)?
    };
    if !file.metadata().map_err(io)?.is_file() {
        return Err("Independent evidence/interpreter is not a regular file".into());
    }
    Ok(file)
}
fn interpreter_hash(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let file = open_regular(path)?;
    let metadata = file.metadata().map_err(io)?;
    if !metadata.is_file() || metadata.len() > MAX_INTERPRETER_BYTES {
        return Err("Independent interpreter is not a bounded regular file".into());
    }
    let mut reader = file.take(MAX_INTERPRETER_BYTES + 1);
    let mut buffer = [0u8; 32 * 1024];
    let mut hash = Sha256::new();
    let mut count = 0u64;
    loop {
        let length = reader.read(&mut buffer).map_err(io)?;
        if length == 0 {
            break;
        }
        count += length as u64;
        if count > MAX_INTERPRETER_BYTES {
            return Err("Independent interpreter exceeds identity budget".into());
        }
        hash.update(&buffer[..length]);
    }
    if count != metadata.len() {
        return Err("Independent interpreter changed during identity read".into());
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn diagnostic_text(path: &Path, limit: u64) -> Result<String, String> {
    use std::io::Read;
    let file = open_regular(path)?;
    if file.metadata().map_err(io)?.len() > limit {
        return Err("Independent diagnostic output exceeded its bounded text budget".into());
    }
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text).map_err(io)?;
    if text.len() as u64 > limit {
        return Err("Independent diagnostic output grew beyond its bounded text budget".into());
    }
    Ok(text)
}
fn diagnostics_within_budget(stdout: &Path, stderr: &Path) -> bool {
    [stdout, stderr].iter().all(|path| fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= MAX_DIAGNOSTIC_BYTES))
}

fn disk_bytes(dir: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    let mut entries = 0usize;
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > 32 {
            return Err("Independent output directory depth exceeds budget".into());
        }
        let children = match fs::read_dir(directory) {
            Ok(children) => children,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io(e)),
        };
        for entry in children {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io(e)),
            };
            entries += 1;
            if entries > 65_536 {
                return Err("Independent output file count exceeds budget".into());
            }
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                // Ghostscript removes its own temporary files while this scan runs.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io(e)),
            };
            if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            } else if metadata.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else {
                return Err("Independent output contains a symbolic link or nonregular file".into());
            }
        }
    }
    Ok(total)
}

pub fn render(config: &Configuration, pdf: &[u8], output: &Path) -> Result<Render, String> {
    if config.version != "10.07.1"
        || config.dpi == 0
        || config.dpi > 9600
        || config.timeout_ms == 0
        || config.timeout_ms > 120_000
        || config.max_output_bytes == 0
        || config.max_output_bytes > 4 * 1024 * 1024 * 1024
    {
        return Err("Unqualified interpreter configuration or invalid resource bounds".into());
    }
    if !config.binary.is_absolute() || interpreter_hash(&config.binary)? != config.binary_sha256 {
        return Err("Independent interpreter binary identity mismatch".into());
    }
    // Caller creates no content at this path: exclusive creation prevents stale plane reuse.
    fs::create_dir(output).map_err(io)?;
    let absolute_output = fs::canonicalize(output).map_err(io)?;
    let output = absolute_output.as_path();
    let started = Instant::now();
    let exact = output.join("input.pdf");
    fs::write(&exact, pdf).map_err(io)?;
    let stdout = output.join("stdout.log");
    let stderr = output.join("stderr.log");
    let version_path = output.join("version.log");
    let mut version = isolated_command(&config.binary, output)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(fs::File::create(&version_path).map_err(io)?)
        .stderr(Stdio::null())
        .spawn()
        .map_err(io)?;
    let version_status = loop {
        if let Some(status) = version.try_wait().map_err(io)? {
            break status;
        }
        if started.elapsed() > Duration::from_millis(config.timeout_ms.min(5000))
            || !disk_bytes(output).is_ok_and(|n| n <= config.max_output_bytes)
            || !fs::metadata(&version_path).is_ok_and(|m| m.len() <= 1024)
        {
            let killed = version.kill();
            let waited = version.wait();
            return Err(format!("Interpreter version probe exceeded resource budget; kill={killed:?}; wait={waited:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if fs::metadata(&version_path).map_err(io)?.len() > 1024 {
        return Err("Invalid interpreter version response".into());
    }
    if !version_status.success() || diagnostic_text(&version_path, 1024)?.trim() != config.version {
        return Err("Independent interpreter version mismatch".into());
    }
    if interpreter_hash(&config.binary)? != config.binary_sha256 {
        return Err("Independent interpreter changed after its version probe".into());
    }
    let mut child = isolated_command(&config.binary, output)
        .args([
            "-dSAFER",
            "-dBATCH",
            "-dNOPAUSE",
            "-dPDFSTOPONERROR",
            "-dPDFSTOPONWARNING",
            "-dPrinted=true",
            "-dUseCropBox",
            "-sDEVICE=tiffsep",
            "-dBitsPerComponent=8",
            "-dPrintSpotCMYK=true",
            "-dMaxSpots=60",
            "-dTextAlphaBits=1",
            "-dGraphicsAlphaBits=1",
        ])
        .arg(format!("-r{}", config.dpi))
        .arg(format!("-sOutputFile={}", output.join("page-%04d.tif").display()))
        .arg("-f")
        .arg(&exact)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout).map_err(io)?)
        .stderr(fs::File::create(&stderr).map_err(io)?)
        .spawn()
        .map_err(io)?;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(io)? {
            break status;
        }
        let size = disk_bytes(output);
        if started.elapsed() > Duration::from_millis(config.timeout_ms)
            || !size.as_ref().is_ok_and(|n| *n <= config.max_output_bytes)
            || !diagnostics_within_budget(&stdout, &stderr)
        {
            let killed = child.kill();
            let waited = child.wait();
            return Err(format!("Independent renderer resource limit; output_scan={size:?}; kill={killed:?}; wait={waited:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if interpreter_hash(&config.binary)? != config.binary_sha256 {
        return Err("Independent interpreter changed during rendering".into());
    }
    if disk_bytes(output)? > config.max_output_bytes {
        return Err("Independent output exceeded budget".into());
    }
    let log = format!("{}\n{}", diagnostic_text(&stdout, MAX_DIAGNOSTIC_BYTES)?, diagnostic_text(&stderr, MAX_DIAGNOSTIC_BYTES)?);
    let mut diagnostics = vec![];
    let mut spot_names = vec![];
    for line in log.lines() {
        if let Some(name) = separation_colour(line) {
            if !spot_names.iter().any(|s| s == name) {
                spot_names.push(name.to_string());
            }
        } else if line.starts_with("%%Separation") {
            diagnostics.push(format!("Unrecognised interpreter colourant diagnostic: {line}"));
        }
        let lower = line.to_ascii_lowercase();
        if ["warning", "error", "substitut", "mapped to", "unable", "repair"].iter().any(|word| lower.contains(word)) {
            diagnostics.push(line.to_string());
        }
    }
    if !status.success() {
        return Err(format!("Independent renderer failed: {status}; {}", diagnostics.join("; ")));
    }
    if spot_names.len() > 60 {
        diagnostics.push("Colourant limit exceeded".into());
    }
    for name in &spot_names {
        if ["Cyan", "Magenta", "Yellow", "Black", "All", "None"].contains(&name.as_str()) {
            diagnostics.push(format!("Colourant {name:?} requires explicit process/special-name reconciliation"));
        }
    }
    let mut planes = vec![];
    for entry in fs::read_dir(output).map_err(io)? {
        let path = entry.map_err(io)?.path();
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((page, name)) = plane_name(file) else {
            continue;
        };
        let reader = image::ImageReader::open(&path).map_err(io)?.with_guessed_format().map_err(io)?;
        let (width, height) = image::image_dimensions(&path).map_err(|e| e.to_string())?;
        let pixels = u64::from(width) * u64::from(height);
        if pixels > config.max_output_bytes || pixels > 512 * 1024 * 1024 {
            return Err("Independent plane exceeds decoded memory budget".into());
        }
        let decoded = reader.decode().map_err(|e| e.to_string())?;
        if decoded.color() != image::ColorType::L8 {
            return Err("Expected an 8-bit tonal separation plane".into());
        }
        let bytes = decoded.as_bytes();
        planes.push(Plane {
            page,
            name,
            path: path.clone(),
            sha256: sha256(&fs::read(path).map_err(io)?),
            width,
            height,
            minimum: bytes.iter().copied().min().unwrap_or(255),
            maximum: bytes.iter().copied().max().unwrap_or(255),
        });
    }
    planes.sort_by(|a, b| (a.page, &a.name).cmp(&(b.page, &b.name)));
    if planes.is_empty() {
        return Err("Independent interpreter produced no separation planes".into());
    }
    // Missing names/colliding filenames must not be mistaken for successful separation.
    for name in &spot_names {
        if !planes.iter().any(|p| &p.name == name) {
            diagnostics.push(format!("No unambiguous output plane for colourant {name:?}"));
        }
    }
    Ok(Render {
        pdf_sha256: sha256(pdf),
        configuration_sha256: digest(config)?,
        version: config.version.clone(),
        planes,
        spot_names,
        diagnostics,
        pixel_pitch_mm: 25.4 / f64::from(config.dpi),
        qualified: false,
    })
}
/// Pinned 10.07.1 tiffsep PrintSpotCMYK record; preserve the name byte-for-byte.
/// A malformed/line-broken record is unresolved, never silently ignored.
fn separation_colour(line: &str) -> Option<&str> {
    let (name, values) = line.strip_prefix("%%SeparationColor: \"")?.rsplit_once("\" 100% ink = ")?;
    let values = values.strip_suffix(" CMYK")?.split_whitespace().map(str::parse::<u16>).collect::<Result<Vec<_>, _>>().ok()?;
    (values.len() == 4 && !name.is_empty()).then_some(name)
}
fn plane_name(file: &str) -> Option<(u32, String)> {
    let rest = file.strip_prefix("page-")?;
    let (page, tail) = rest.split_once('(')?;
    let name = tail.strip_suffix(").tif")?;
    Some((page.parse().ok()?, name.into()))
}
#[derive(Debug, Serialize)]
pub struct ResolutionEvidence {
    pub dpi: u32,
    pub rendering_index: Option<usize>,
    pub comparison_index: usize,
}
/// Two resolutions are retained independently. Agreement is evidence, not a substitute for
/// qualification of the exact runtime configuration and every encountered content feature.
#[derive(Debug, Serialize)]
pub struct Verification {
    pub report: crate::Report,
    pub renderings: Vec<Render>,
    pub comparisons: Vec<crate::raster::Comparison>,
    pub resolutions: Vec<ResolutionEvidence>,
}
pub fn verify(request: &crate::Request, bytes: &[u8], config: &Configuration, output: &Path) -> Result<Verification, String> {
    verify_with_renderer(request, bytes, config, output, render)
}
fn verify_with_renderer(
    request: &crate::Request,
    bytes: &[u8],
    config: &Configuration,
    output: &Path,
    mut renderer: impl FnMut(&Configuration, &[u8], &Path) -> Result<Render, String>,
) -> Result<Verification, String> {
    let mut report = crate::pdf::check(request, bytes)?;
    if crate::sha256(bytes) != request.identity.pdf_sha256 {
        return Err("Independent PDF identity mismatch".into());
    }
    fs::create_dir(output).map_err(io)?;
    let imported = vectorcraft_pdf::import_with_report(
        bytes,
        &vectorcraft_pdf::ImportOptions {
            preserve_clip_paths: true,
            reject_depth_loss: true,
            text_as: vectorcraft_pdf::TextAs::Outlines,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let mut census = crate::pdf::census(&imported.document);
    // The ordinary report above retains all import loss. This census is used
    // only for independent physical prediction: exact raw DeviceN operands
    // replace this one known RGB-preview conversion, never any other warning.
    census.unresolved.extend(crate::pdf::import_messages(imported.warnings).0.into_iter().filter(|w| w != crate::raw_named::PREVIEW_NOTICE));
    crate::ordering::reconcile(bytes, &mut census);
    let tolerance = request.acceptance.get("geometry_tolerance_mm").filter(|f| f.evidenced()).and_then(|f| f.value.as_f64());
    let mut renderings = vec![];
    let mut comparisons = vec![];
    let mut resolutions = vec![];
    if config.dpi > 4800 {
        return Err("Two-resolution verification requires starting resolution at most 4800 DPI".into());
    }
    for multiplier in [1, 2] {
        let mut configuration = config.clone();
        configuration.dpi *= multiplier;
        let rendering = match renderer(&configuration, bytes, &output.join(format!("dpi-{}", configuration.dpi))) {
            Ok(rendering) => rendering,
            Err(reason) => {
                // A later unavailable resolution must not discard already witnessed
                // defects or the complete report denominator. No render is fabricated.
                resolutions.push(ResolutionEvidence { dpi: configuration.dpi, rendering_index: None, comparison_index: comparisons.len() });
                comparisons.push(crate::raster::Comparison {
                    status: crate::Status::Incomplete,
                    uncertainty_mm: 25.4 / f64::from(configuration.dpi.max(1)) * 2.0f64.sqrt()
                        + 0.000002
                        + census.paints.iter().map(|p| p.geometry_error_mm).fold(0.0, f64::max),
                    planes: vec![],
                    issues: vec![format!("Independent render at {} DPI did not complete: {reason}", configuration.dpi)],
                });
                continue;
            }
        };
        let comparison = if census.raw_audit.as_ref().is_some_and(|a| a.image_use_count > 0) {
            crate::raster_images::compare(bytes, &rendering, tolerance)
        } else {
            crate::raster::compare_named(&census, &rendering, tolerance)
        }
        .unwrap_or_else(|reason| crate::raster::Comparison {
            status: crate::Status::Incomplete,
            uncertainty_mm: rendering.pixel_pitch_mm * 2.0f64.sqrt()
                + 0.000002
                + census.paints.iter().map(|p| p.geometry_error_mm).fold(0.0, f64::max),
            planes: vec![],
            issues: vec![format!("Independent comparison could not complete: {reason}")],
        });
        resolutions.push(ResolutionEvidence { dpi: configuration.dpi, rendering_index: Some(renderings.len()), comparison_index: comparisons.len() });
        renderings.push(rendering);
        comparisons.push(comparison);
    }
    let measurements = serde_json::json!({"renderings":renderings,"comparisons":comparisons,"resolutions":resolutions});
    let manifest = serde_json::to_vec(&measurements).map_err(|e| e.to_string())?;
    let hash = crate::sha256(&manifest);
    fs::write(output.join("measurements.json"), manifest).map_err(io)?;
    let failure = comparisons.iter().any(|c| c.status == crate::Status::Fail);
    report.record(
        "global/independent",
        if failure { crate::Status::Fail } else { crate::Status::Incomplete },
        if failure {
            "Independent per-ink tone disagrees with native print prediction"
        } else {
            "Independent render/comparison evidence retained; incomplete resolutions and runtime/content qualification remain unresolved"
        },
        serde_json::json!({"comparisons":comparisons,"qualified":false}),
        vec![crate::Evidence {
            artifact_sha256: hash,
            producer: format!("ghostscript/{}/two-resolution-checks-1", config.version),
            detail: "Exact PDF, configuration, tonal planes and comparison manifest retained".into(),
        }],
    )?;
    Ok(Verification { report, renderings, comparisons, resolutions })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_exact_and_composite_is_excluded() {
        assert_eq!(plane_name("page-0002(Exact ink).tif"), Some((2, "Exact ink".into())));
        assert!(plane_name("page-0002.tif").is_none());
        assert_eq!(plane_name("page-0001(A(B)).tif"), Some((1, "A(B)".into())));
        assert_eq!(separation_colour("%%SeparationColor: \"Exact ink \" 100% ink = 0 32760 0 0 CMYK"), Some("Exact ink "));
        assert_eq!(separation_colour("%%SeparationColor: \"A\"B\" 100% ink = 0 32760 0 0 CMYK"), Some("A\"B"));
        assert!(separation_colour("%%SeparationColor: \"bad\" 100% ink = 0 1 CMYK").is_none());
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    #[test]
    fn independent_planes_agree_with_nested_evenodd_clipping() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        let output = std::env::temp_dir()
            .join(format!("vc-independent-clip-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let cfg = Configuration {
            binary: binary.clone().into(),
            binary_sha256: sha256(&fs::read(&binary).unwrap()),
            version: "10.07.1".into(),
            dpi: 144,
            timeout_ms: 10_000,
            max_output_bytes: 16 * 1024 * 1024,
            qualification_sha256: None,
        };
        let bytes = crate::pdf::tests::fixture(true, "Exact#20ink", "q 0 0 40 40 re 5 5 30 30 re W* n q 0 0 20 40 re W n 0 0 40 40 re f Q Q", None);
        let rendering = render(&cfg, &bytes, &output).unwrap();
        let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        crate::ordering::reconcile(&bytes, &mut census);
        let comparison = crate::raster::compare(&census, &rendering, Some(0.3)).unwrap();
        assert_eq!(comparison.status, crate::Status::Pass, "{comparison:?}");
        fs::remove_dir_all(output).unwrap();
    }
    #[test]
    fn independent_strokes_match_caps_joins_dashes_and_dots() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        for (label, content) in [
            ("round", "1 0 0 0 K 2 w 1 J 1 j 50 55 m 75 55 l 75 80 l S"),
            ("square-miter", "0 1 0 0 K 3 w 2 J 0 j 2 M 50 50 m 70 65 l 50 80 l S"),
            ("dash", "0 0 1 0 K 2 w 0 J 2 j [5 3] 2 d 45 70 m 90 70 l S"),
            ("dots", "0 0 0 1 K 2 w 1 J [0 4] 0 d 45 70 m 90 70 l S"),
            ("point", "0 0 0 1 K 3 w 1 J 70 70 m 70 70 l S"),
        ] {
            let output = std::env::temp_dir()
                .join(format!("vc-stroke-{label}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            let cfg = Configuration {
                binary: binary.clone().into(),
                binary_sha256: sha256(&fs::read(&binary).unwrap()),
                version: "10.07.1".into(),
                dpi: 144,
                timeout_ms: 10_000,
                max_output_bytes: 16 * 1024 * 1024,
                qualification_sha256: None,
            };
            let bytes = crate::pdf::tests::fixture(true, "Exact#20ink", content, None);
            let rendering = render(&cfg, &bytes, &output).unwrap();
            let imported = vectorcraft_pdf::import_with_report(&bytes, &Default::default()).unwrap();
            assert!(imported.warnings.is_empty(), "{label}: {:?}", imported.warnings);
            let mut census = crate::pdf::census(&imported.document);
            crate::ordering::reconcile(&bytes, &mut census);
            let comparison = crate::raster::compare(&census, &rendering, Some(0.3)).unwrap();
            assert_eq!(comparison.status, crate::Status::Pass, "{label}: {comparison:?}");
            fs::remove_dir_all(output).unwrap();
        }
    }
    #[test]
    fn independent_zero_tints_obey_source_space_and_overprint_mode() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        // Original ISO fixtures: grey writes all process components; Separation
        // writes its named component even at zero. OPM changes only direct CMYK.
        for (label, content) in
            [("cmyk", "0 1 0 0 k 10 10 20 20 re f"), ("gray", "0.5 g 10 10 20 20 re f"), ("spot-zero", "/Ink cs 0 scn 10 10 20 20 re f")]
        {
            for mode in [0, 1] {
                let output = std::env::temp_dir().join(format!(
                    "vc-independent-{label}-{mode}-{}",
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
                ));
                let cfg = Configuration {
                    binary: binary.clone().into(),
                    binary_sha256: sha256(&fs::read(&binary).unwrap()),
                    version: "10.07.1".into(),
                    dpi: 144,
                    timeout_ms: 10_000,
                    max_output_bytes: 16 * 1024 * 1024,
                    qualification_sha256: None,
                };
                let bytes = crate::pdf::tests::fixture_mode(true, mode, "Exact#20ink", content, None);
                let rendering = render(&cfg, &bytes, &output).unwrap();
                let imported = vectorcraft_pdf::import_with_report(&bytes, &Default::default()).unwrap();
                assert!(imported.warnings.is_empty(), "{label}/{mode}: {:?}", imported.warnings);
                let mut census = crate::pdf::census(&imported.document);
                crate::ordering::reconcile(&bytes, &mut census);
                let comparison = crate::raster::compare(&census, &rendering, Some(0.3)).unwrap();
                assert_eq!(comparison.status, crate::Status::Pass, "{label}/{mode}: {comparison:?}");
                if label == "cmyk" {
                    let p = census.paints.last_mut().unwrap();
                    p.overprint_zero = !p.overprint_zero;
                    assert_eq!(crate::raster::compare(&census, &rendering, Some(0.3)).unwrap().status, crate::Status::Fail);
                }
                fs::remove_dir_all(output).unwrap();
            }
        }
    }
    #[test]
    fn independent_planes_distinguish_knockout_from_overprint() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        for overprint in [true, false] {
            let output = std::env::temp_dir().join(format!(
                "vc-independent-{}-{}",
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
                if overprint { "overprint" } else { "knockout" }
            ));
            let cfg = Configuration {
                binary: binary.clone().into(),
                binary_sha256: sha256(&fs::read(&binary).unwrap()),
                version: "10.07.1".into(),
                dpi: 72,
                timeout_ms: 10_000,
                max_output_bytes: 16 * 1024 * 1024,
                qualification_sha256: None,
            };
            let result = render(&cfg, &crate::pdf::tests::pdf(overprint), &output).unwrap();
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
            assert_eq!(result.planes.len(), 5, "{:?}", result);
            assert_eq!(result.spot_names, vec!["Exact ink"]);
            let cyan = result.planes.iter().find(|p| p.name == "Cyan").unwrap();
            let image = image::open(&cyan.path).unwrap().to_luma8();
            assert_eq!(image.get_pixel(20, 80).0[0], if overprint { 0 } else { 255 });
            let imported = vectorcraft_pdf::import(&crate::pdf::tests::pdf(overprint)).unwrap();
            let mut census = crate::pdf::census(&imported);
            crate::ordering::reconcile(&crate::pdf::tests::pdf(overprint), &mut census);
            let comparison = crate::raster::compare(&census, &result, Some(1.0)).unwrap();
            assert_eq!(comparison.status, crate::Status::Pass, "{comparison:?}");
            census.paints.last_mut().unwrap().overprint = !overprint;
            let incorrect = crate::raster::compare(&census, &result, Some(1.0)).unwrap();
            assert_eq!(incorrect.status, crate::Status::Fail, "{incorrect:?}");
            assert!(incorrect.planes.iter().any(|p| p.wrong_pixels > 0 && p.first_defect_mm.is_some()));
            assert!(!result.qualified);
            fs::remove_dir_all(output).unwrap();
        }
    }
    #[test]
    fn a_later_render_failure_preserves_an_earlier_witnessed_defect() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else {
            return;
        };
        let bytes = crate::pdf::tests::pdf(true);
        let mut request = crate::tests::request();
        request.identity.pdf_sha256 = crate::sha256(&bytes);
        request.identity.recipe_sha256 = crate::digest(&request.recipe).unwrap();
        request.acceptance.insert(
            "geometry_tolerance_mm".into(),
            crate::preparation::Fact {
                value: serde_json::json!(2.0),
                provenance: crate::preparation::Provenance::Operator {
                    actor: "Synthetic test".into(),
                    at: "2026-10-19".into(),
                    decision_id: "Synthetic tolerance only".into(),
                    evidence_sha256: "e".repeat(64),
                },
            },
        );
        request.identity.measurement_sha256 = crate::digest(&request.acceptance).unwrap();
        let config = Configuration {
            binary: binary.clone().into(),
            binary_sha256: crate::sha256(&fs::read(&binary).unwrap()),
            version: "10.07.1".into(),
            dpi: 72,
            timeout_ms: 10000,
            max_output_bytes: 16 * 1024 * 1024,
            qualification_sha256: None,
        };
        for defect in [false, true] {
            let output = std::env::temp_dir().join(format!(
                "vc-render-retention-{}-{defect}",
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            let mut calls = 0;
            let verification = verify_with_renderer(&request, &bytes, &config, &output, |configuration, bytes, path| {
                calls += 1;
                if calls == 2 {
                    return Err("Synthetic second-resolution timeout".into());
                }
                let mut rendering = render(configuration, bytes, path)?;
                if defect {
                    let plane = rendering.planes.iter_mut().find(|p| p.name == "Cyan").unwrap();
                    let mut image = image::open(&plane.path).unwrap().into_luma8();
                    for y in 76..85 {
                        for x in 16..25 {
                            let p = image.get_pixel_mut(x, y);
                            p.0[0] = 255 - p.0[0];
                        }
                    }
                    plane.path = path.join("changed-cyan.tif");
                    image.save(&plane.path).unwrap();
                    plane.sha256 = crate::sha256(&fs::read(&plane.path).unwrap());
                }
                Ok(rendering)
            })
            .unwrap();
            assert_eq!(calls, 2);
            assert_eq!(verification.renderings.len(), 1);
            assert_eq!(verification.comparisons.len(), 2);
            assert_eq!(
                verification.resolutions.iter().map(|r| (r.dpi, r.rendering_index, r.comparison_index)).collect::<Vec<_>>(),
                vec![(72, Some(0), 0), (144, None, 1)]
            );
            assert_eq!(verification.report.coverage.total, crate::pdf::check(&request, &bytes).unwrap().coverage.total);
            assert_eq!(verification.comparisons[0].status, if defect { crate::Status::Fail } else { crate::Status::Pass });
            assert_eq!(verification.comparisons[1].status, crate::Status::Incomplete);
            let finding = verification.report.findings.iter().find(|f| f.obligation.key == "global/independent").unwrap();
            assert_eq!(finding.status, if defect { crate::Status::Fail } else { crate::Status::Incomplete });
            assert!(verification.comparisons[1].issues.iter().any(|s| s.contains("144 DPI") && s.contains("timeout")));
            assert!(output.join("measurements.json").exists());
            fs::remove_dir_all(output).unwrap();
        }
    }
    #[test]
    fn missing_first_or_both_renderings_keep_explicit_resolution_mapping() {
        let bytes = crate::pdf::tests::pdf(true);
        let mut request = crate::tests::request();
        request.identity.pdf_sha256 = crate::sha256(&bytes);
        let config = Configuration {
            binary: "/synthetic/no-interpreter-used".into(),
            binary_sha256: "a".repeat(64),
            version: "synthetic mapping fixture".into(),
            dpi: 72,
            timeout_ms: 1,
            max_output_bytes: 1,
            qualification_sha256: None,
        };
        for both in [false, true] {
            let output = std::env::temp_dir().join(format!(
                "vc-first-render-mapping-{}-{both}",
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            let mut calls = 0;
            let verification = verify_with_renderer(&request, &bytes, &config, &output, |configuration, bytes, _| {
                calls += 1;
                if calls == 1 || both {
                    return Err("Synthetic unavailable rendering".into());
                }
                Ok(Render {
                    pdf_sha256: crate::sha256(bytes),
                    configuration_sha256: crate::digest(configuration)?,
                    version: configuration.version.clone(),
                    planes: vec![],
                    spot_names: vec![],
                    diagnostics: vec!["Synthetic empty-plane mapping fixture".into()],
                    pixel_pitch_mm: 25.4 / f64::from(configuration.dpi),
                    qualified: false,
                })
            })
            .unwrap();
            assert_eq!(calls, 2);
            assert_eq!(verification.renderings.len(), usize::from(!both));
            assert_eq!(verification.comparisons.len(), 2);
            assert_eq!(
                verification.resolutions.iter().map(|r| (r.dpi, r.rendering_index, r.comparison_index)).collect::<Vec<_>>(),
                vec![(72, None, 0), (144, if both { None } else { Some(0) }, 1)]
            );
            assert!(verification.comparisons.iter().all(|c| c.status == crate::Status::Incomplete));
            assert_eq!(verification.report.coverage.total, crate::pdf::check(&request, &bytes).unwrap().coverage.total);
            assert!(output.join("measurements.json").exists());
            fs::remove_dir_all(output).unwrap();
        }
    }
}
#[cfg(test)]
#[cfg(unix)]
mod isolation_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    fn root(label: &str) -> PathBuf {
        let root = std::env::temp_dir()
            .join(format!("vc-runtime-{label}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&root).unwrap();
        root
    }
    fn script(root: &Path, body: &str) -> Configuration {
        let binary = root.join("interpreter");
        fs::write(&binary, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        Configuration {
            binary_sha256: interpreter_hash(&binary).unwrap(),
            binary,
            version: "10.07.1".into(),
            dpi: 72,
            timeout_ms: 5000,
            max_output_bytes: 32 * 1024 * 1024,
            qualification_sha256: None,
        }
    }
    #[test]
    fn diagnostic_floods_are_refused_before_unbounded_text_loading() {
        for version in [false, true] {
            let root = root("diagnostics");
            let body = if version {
                "/usr/bin/head -c 3145728 /dev/zero"
            } else {
                "if [ \"$1\" = --version ]; then printf '10.07.1\\n'; exit 0; fi\n/usr/bin/head -c 3145728 /dev/zero"
            };
            let config = script(&root, body);
            let reason = render(&config, b"original synthetic input", &root.join("evidence")).unwrap_err();
            assert!(reason.contains("budget") || reason.contains("version response") || reason.contains("resource limit"), "{reason}");
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn interpreter_replacement_between_execution_phases_is_refused() {
        for during_render in [false, true] {
            let root = root("replacement");
            let body = if during_render {
                "if [ \"$1\" = --version ]; then printf '10.07.1\\n'; exit 0; fi\nprintf '#!/bin/sh\\nexit 0\\n' > \"$0\""
            } else {
                "printf '#!/bin/sh\\nexit 0\\n' > \"$0\"\nprintf '10.07.1\\n'"
            };
            let config = script(&root, body);
            let reason = render(&config, b"original synthetic input", &root.join("evidence")).unwrap_err();
            assert!(reason.contains(if during_render { "changed during rendering" } else { "changed after its version probe" }), "{reason}");
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn nested_runtime_output_counts_toward_budget_and_links_are_refused() {
        let root = root("files");
        fs::create_dir_all(root.join("cache/nested")).unwrap();
        fs::write(root.join("top"), b"a").unwrap();
        fs::write(root.join("cache/nested/data"), [0u8; 100]).unwrap();
        assert_eq!(disk_bytes(&root).unwrap(), 101);
        symlink(root.join("top"), root.join("cache/link")).unwrap();
        assert!(disk_bytes(&root).unwrap_err().contains("symbolic link"));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn fifo_executable_identity_is_rejected_without_waiting_for_a_writer() {
        if std::env::var("VC_TEST_FIFO_CHILD").as_deref() != Ok("1") {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "independent::isolation_tests::fifo_executable_identity_is_rejected_without_waiting_for_a_writer", "--nocapture"])
                .env("VC_TEST_FIFO_CHILD", "1")
                .spawn()
                .unwrap();
            let started = Instant::now();
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    return;
                }
                if started.elapsed() > Duration::from_secs(10) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("FIFO identity check blocked");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        for during_render in [false, true] {
            let root = root("fifo");
            let replace = "/bin/rm \"$0\"; /usr/bin/mkfifo \"$0\"";
            let body = if during_render {
                format!("if [ \"$1\" = --version ]; then printf '10.07.1\\n'; exit 0; fi\n{replace}")
            } else {
                format!("{replace}\nprintf '10.07.1\\n'")
            };
            let config = script(&root, &body);
            assert!(render(&config, b"synthetic input", &root.join("evidence")).unwrap_err().contains("regular file"));
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn temporary_file_cleanup_during_output_scan_is_not_a_resource_failure() {
        let root = root("cleanup");
        fs::write(root.join("retained"), b"retained").unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for i in 0..2000 {
                    let dir = root.join(format!("temp-{}", i % 8));
                    fs::create_dir(&dir).unwrap();
                    fs::write(dir.join("data"), [0u8; 64]).unwrap();
                    fs::remove_dir_all(dir).unwrap();
                }
            });
            for _ in 0..2000 {
                assert!(disk_bytes(&root).unwrap() >= 8);
            }
        });
        assert_eq!(disk_bytes(&root).unwrap(), 8);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn ambient_gs_options_cannot_change_measurement() {
        let Ok(binary) = std::env::var("CHECK_GS_BINARY") else { return };
        if std::env::var("VC_TEST_ISOLATED_RENDERER_CHILD").as_deref() != Ok("1") {
            // Inject into only the child test process; never mutate this parallel
            // test runner's process-wide environment.
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "independent::isolation_tests::ambient_gs_options_cannot_change_measurement", "--nocapture"])
                .env("VC_TEST_ISOLATED_RENDERER_CHILD", "1")
                .env("GS_OPTIONS", "--version")
                .env("GS_LIB", "/does-not-exist/runtime-qualification")
                .env("GS_FONTPATH", "/does-not-exist/runtime-qualification")
                .spawn()
                .unwrap();
            let started = Instant::now();
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                if started.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("Isolated environment regression timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            return;
        }
        let root = root("ambient");
        let config = Configuration {
            binary_sha256: interpreter_hash(Path::new(&binary)).unwrap(),
            binary: binary.into(),
            version: "10.07.1".into(),
            dpi: 72,
            timeout_ms: 10000,
            max_output_bytes: 16 * 1024 * 1024,
            qualification_sha256: None,
        };
        let bytes = crate::pdf::tests::fixture(true, "Exact#20ink", "/Ink cs 1 scn 10 10 20 20 re f", None);
        let rendering = render(&config, &bytes, &root.join("evidence")).unwrap();
        let mut census = crate::pdf::census(&vectorcraft_pdf::import(&bytes).unwrap());
        crate::ordering::reconcile(&bytes, &mut census);
        let comparison = crate::raster::compare(&census, &rendering, Some(0.6)).unwrap();
        assert_eq!(comparison.status, crate::Status::Pass, "{comparison:?}");
        assert!(!rendering.qualified);
        fs::remove_dir_all(root).unwrap();
    }
}
