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
fn disk_bytes(dir: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir).map_err(io)? {
        total = total.saturating_add(entry.map_err(io)?.metadata().map_err(io)?.len());
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
    if !config.binary.is_absolute() || sha256(&fs::read(&config.binary).map_err(io)?) != config.binary_sha256 {
        return Err("Independent interpreter binary identity mismatch".into());
    }
    // Caller creates no content at this path: exclusive creation prevents stale plane reuse.
    fs::create_dir(output).map_err(io)?;
    let started = Instant::now();
    let exact = output.join("input.pdf");
    fs::write(&exact, pdf).map_err(io)?;
    let stdout = output.join("stdout.log");
    let stderr = output.join("stderr.log");
    let version_path = output.join("version.log");
    let mut version = Command::new(&config.binary)
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
        if started.elapsed() > Duration::from_millis(config.timeout_ms.min(5000)) || !disk_bytes(output).is_ok_and(|n| n <= config.max_output_bytes) {
            let killed = version.kill();
            let waited = version.wait();
            return Err(format!("Interpreter version probe exceeded resource budget; kill={killed:?}; wait={waited:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if fs::metadata(&version_path).map_err(io)?.len() > 1024 {
        return Err("Invalid interpreter version response".into());
    }
    if !version_status.success() || fs::read_to_string(version_path).map_err(io)?.trim() != config.version {
        return Err("Independent interpreter version mismatch".into());
    }
    let mut child = Command::new(&config.binary)
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
        if started.elapsed() > Duration::from_millis(config.timeout_ms) || !size.as_ref().is_ok_and(|n| *n <= config.max_output_bytes) {
            let killed = child.kill();
            let waited = child.wait();
            return Err(format!("Independent renderer resource limit; kill={killed:?}; wait={waited:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if disk_bytes(output)? > config.max_output_bytes {
        return Err("Independent output exceeded budget".into());
    }
    let log = format!("{}\n{}", fs::read_to_string(stdout).map_err(io)?, fs::read_to_string(stderr).map_err(io)?);
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
    census.unresolved.extend(crate::pdf::import_messages(imported.warnings).0);
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
            crate::raster::compare(&census, &rendering, tolerance)
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
