//! Local qualification harness; customer files are passed by path, never embedded.
#[allow(dead_code)]
#[path = "../src/operations.rs"]
mod operations;
#[allow(dead_code)]
#[path = "../src/protocol.rs"]
mod protocol;
use serde_json::json;
use vectorcraft_engine::Session;
use vectorcraft_ui_egui::{Services, VectorcraftApp, io};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).ok_or("input required")?;
    let out = std::path::Path::new(args.get(2).ok_or("output directory required")?);
    let hybrid = args.get(3).is_none_or(|s| s != "df");
    std::fs::create_dir_all(out)?;
    let name = std::path::Path::new(input).file_name().ok_or("filename required")?.to_string_lossy();
    let mut app = VectorcraftApp::new(Session::new(), Services::default());
    let imported =
        io::open_document(&mut app, &name, &std::fs::read(input)?, None, &if hybrid { json!({"printProduction": true}) } else { json!({}) })?;
    let doc = &app.session.active().ok_or("no active document")?.doc;
    let native = vectorcraft_format::save_with(doc, &Default::default())?;
    let reopened = vectorcraft_format::load_file(&native)?;
    if !reopened.depth_notes.is_empty() || reopened.doc != **doc {
        return Err("native save/reopen changed the document".into());
    }
    std::fs::write(out.join("saved.vectorcraft"), native)?;
    let requested = protocol::export(None, Some(true), None, Some(false), None)?;
    let requested = protocol::with_output(requested, hybrid.then_some(r#"{"conversion":"preserveSource"}"#))?;
    let (_, original, _, _) = operations::write_export(&mut app, &requested)?;
    std::fs::write(out.join("saved.pdf"), &original)?;
    app.run("select.all", json!({}))?;
    app.run("object.move", json!({"dx":1,"dy":0}))?;
    let (_, edited, _, _) = operations::write_export(&mut app, &requested)?;
    std::fs::write(out.join("edited.pdf"), &edited)?;
    app.run("edit.undo", json!({}))?;
    let (_, undone, _, _) = operations::write_export(&mut app, &requested)?;
    std::fs::write(out.join("undone.pdf"), &undone)?;
    println!("{}", json!({"import":imported,"savedBytes":original.len(),"editedBytes":edited.len(),"undoneBytes":undone.len(),"hybrid":hybrid}));
    Ok(())
}
