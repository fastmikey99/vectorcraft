//! Content identity, shared by native/WASM/service builds from the same source tree.
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
fn files(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") || path.file_name().is_some_and(|f| f == "Cargo.toml") {
            out.push(path);
        }
    }
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let root = here.parent().and_then(Path::parent).ok_or("Workspace root missing")?;
    println!("cargo:rerun-if-changed={}", root.join("crates").display());
    let mut list = vec![root.join("Cargo.toml"), root.join("Cargo.lock")];
    files(&root.join("crates"), &mut list)?;
    list.sort();
    let mut hash = Sha256::new();
    hash.update(b"vectorcraft-check-engine-source-v1\0");
    for path in list {
        let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(&path)?;
        println!("cargo:rerun-if-changed={}", path.display());
        hash.update((relative.len() as u64).to_be_bytes());
        hash.update(relative.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    println!("cargo:rustc-env=CHECK_ENGINE_SOURCE_SHA256={:x}", hash.finalize());
    Ok(())
}
