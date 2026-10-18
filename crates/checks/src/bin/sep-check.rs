//! One-shot service worker. Exit success means a report was produced, not a production Pass.
use std::{env, fs, process::ExitCode};
fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let first = args.next().ok_or("Usage: sep-check [--enumerate] request.json [exact.pdf]")?;
    if first == "--version" {
        if args.next().is_some() {
            return Err("Unexpected argument".into());
        }
        println!("{}", vectorcraft_checks::ENGINE_VERSION);
        return Ok(());
    }
    #[cfg(not(target_arch = "wasm32"))]
    if first == "--render-plates" {
        let config_path = args.next().ok_or("Missing interpreter configuration")?;
        let pdf_path = args.next().ok_or("Missing exact PDF")?;
        let output = args.next().ok_or("Missing exclusive output directory")?;
        if args.next().is_some() {
            return Err("Unexpected argument".into());
        }
        if fs::metadata(&config_path).map_err(|e| e.to_string())?.len() > 64 * 1024 {
            return Err("Interpreter configuration exceeds budget".into());
        }
        if fs::metadata(&pdf_path).map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
            return Err("PDF exceeds 256 MiB budget".into());
        }
        let config = serde_json::from_slice(&fs::read(config_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let render =
            vectorcraft_checks::independent::render(&config, &fs::read(pdf_path).map_err(|e| e.to_string())?, std::path::Path::new(&output))?;
        println!("{}", serde_json::to_string(&render).map_err(|e| e.to_string())?);
        return Ok(());
    }
    #[cfg(not(target_arch = "wasm32"))]
    if first == "--verify-independent" {
        let config_path = args.next().ok_or("Interpreter configuration required")?;
        let request_path = args.next().ok_or("Request required")?;
        let pdf_path = args.next().ok_or("Exact PDF required")?;
        let output = args.next().ok_or("Exclusive evidence directory required")?;
        let original = args.next();
        if args.next().is_some() {
            return Err("Unexpected argument".into());
        }
        for (path, limit) in [(&config_path, 64 * 1024), (&request_path, 16 * 1024 * 1024), (&pdf_path, 256 * 1024 * 1024)] {
            if fs::metadata(path).map_err(|e| e.to_string())?.len() > limit {
                return Err("Independent input exceeds budget".into());
            }
        }
        let config = serde_json::from_slice(&fs::read(config_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let request = serde_json::from_slice(&fs::read(request_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let mut bundle = vectorcraft_checks::independent::verify(
            &request,
            &fs::read(pdf_path).map_err(|e| e.to_string())?,
            &config,
            std::path::Path::new(&output),
        )?;
        if let Some(path) = original {
            if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
                return Err("Original exceeds budget".into());
            }
            vectorcraft_checks::prepare::verify_into_report(&request, &fs::read(path).map_err(|e| e.to_string())?, &mut bundle.report)?;
        }
        println!("{}", serde_json::to_string(&bundle.report).map_err(|e| e.to_string())?);
        return Ok(());
    }
    if first == "--print-order" {
        let path = args.next().ok_or("Exact PDF required")?;
        if args.next().is_some() {
            return Err("Unexpected argument".into());
        }
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
            return Err("PDF exceeds input budget".into());
        }
        let result = vectorcraft_pdf::print_order(&fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        println!("{}", serde_json::to_string(&result).map_err(|e| e.to_string())?);
        return Ok(());
    }
    if first == "--prepare-edits" {
        let capture = args.next().ok_or("Original capture required")?;
        let edits = args.next().ok_or("Attributed edits required")?;
        if args.next().is_some() {
            return Err("Unexpected argument".into());
        }
        for path in [&capture, &edits] {
            if fs::metadata(path).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
                return Err("Preparation input exceeds budget".into());
            }
        }
        let capture = serde_json::from_slice(&fs::read(capture).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let edits: Vec<vectorcraft_checks::preparation::Fact> =
            serde_json::from_slice(&fs::read(edits).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let draft = vectorcraft_checks::prepare_edits::replay(capture, &edits)?;
        println!("{}", serde_json::to_string(&draft).map_err(|e| e.to_string())?);
        return Ok(());
    }
    if first == "--prepare" {
        let path = args.next().ok_or("Original PDF required")?;
        let id = args.next().ok_or("Preparation identity required")?;
        let job = args.next().ok_or("Job revision required")?;
        let edits_path = args.next();
        if args.next().is_some() {
            return Err("Unexpected argument".into());
        }
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
            return Err("Original exceeds budget".into());
        }
        let mut draft = vectorcraft_checks::prepare::capture(&fs::read(path).map_err(|e| e.to_string())?, id, job)?;
        if let Some(path) = edits_path {
            if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
                return Err("Preparation edits exceed budget".into());
            }
            let edits: Vec<vectorcraft_checks::preparation::Fact> =
                serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            draft = vectorcraft_checks::prepare_edits::replay(draft, &edits)?;
        }
        println!("{}", serde_json::to_string(&draft).map_err(|e| e.to_string())?);
        return Ok(());
    }
    if first == "--enumerate" || first == "--digest-json" {
        let path = args.next().ok_or("Missing request file")?;
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
            return Err("Request exceeds 16 MiB budget".into());
        }
        if args.next().is_some() {
            return Err("Unexpected argument".into());
        }
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        if first == "--digest-json" {
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            println!("{}", vectorcraft_checks::digest(&value)?);
            return Ok(());
        }
        let request: vectorcraft_checks::Request = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        println!("{}", serde_json::to_string(&vectorcraft_checks::enumerate(&request)?).map_err(|e| e.to_string())?);
        return Ok(());
    }
    let request_path = first;
    let pdf_path = args.next().ok_or("Usage: sep-check request.json exact.pdf")?;
    let original_path = args.next();
    if args.next().is_some() {
        return Err("Unexpected argument".into());
    }
    if fs::metadata(&request_path).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
        return Err("Request exceeds 16 MiB budget".into());
    }
    if fs::metadata(&pdf_path).map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
        return Err("PDF exceeds 256 MiB budget".into());
    }
    let request: vectorcraft_checks::Request =
        serde_json::from_slice(&fs::read(request_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mut report = vectorcraft_checks::pdf::check(&request, &fs::read(pdf_path).map_err(|e| e.to_string())?)?;
    if let Some(path) = original_path {
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
            return Err("Original exceeds 256 MiB budget".into());
        }
        vectorcraft_checks::prepare::verify_into_report(&request, &fs::read(path).map_err(|e| e.to_string())?, &mut report)?;
    }
    println!("{}", serde_json::to_string(&report).map_err(|e| e.to_string())?);
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
