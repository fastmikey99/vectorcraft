use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use vectorcraft_testkit::pdf::{PdfPage, pdf};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("vectorcraft-pal-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn pal(&self, name: &str, body: &str) {
        let p = self.0.join(format!("{name}.pal"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("<ActionList Version=\"2.0\">{body}</ActionList>")).unwrap();
    }
    fn input(&self) {
        std::fs::write(self.0.join("input.pdf"), pdf(&[PdfPage::new(100.0, 100.0, "q 10 10 30 30 re W n 0 1 0 0 k 0 0 70 70 re f Q")], None))
            .unwrap();
    }
    fn run(&self, s: &mut Session) -> Value {
        s.execute("sep.runActionList", &json!({"root":self.0,"lists":["main"],"input":self.0.join("input.pdf"),"output":self.0.join("output.pdf")}))
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn a(kind: &str, params: &str) -> String {
    format!("<ActionListItem Type=\"{kind}\"><Parameters>{params}</Parameters></ActionListItem>")
}
fn player(s: &mut Session) -> Player<'_> {
    Player {
        session: s,
        root: PathBuf::new(),
        stack: vec![],
        layers: vec![],
        inks: vec![],
        boxes: BTreeMap::new(),
        trace: vec![],
        stopped: None,
        clipboard_empty: true,
        messages: vec![],
    }
}
fn step(p: &mut Player<'_>, kind: &str, params: &str) -> Result<usize> {
    let actions = reader::parse(&format!("<ActionList Version=\"2.0\">{}</ActionList>", a(kind, params))).unwrap();
    p.action(&actions[0])
}

#[test]
fn command_clips_moves_creates_ink_and_stops_without_running_later_actions() {
    let f = Fixture::new();
    f.input();
    f.pal(
        "main",
        &[
            a("SelectAllObjects", ""),
            a("CleanObjects", "<ExecuteClip>1</ExecuteClip>"),
            a("CreateLayerOnTop", "<LayerName>Artwork</LayerName>"),
            a("GroupObjects", ""),
            a("MoveObjectsToLayer", ""),
            a("SelectEmptyLayers", ""),
            a("DeleteLayers", ""),
            a("SelectAllObjects", ""),
            a("UngroupObjects", ""),
            a("CallActionList", "<Path>nested\\prepare</Path>"),
            a("DeleteObjects", ""),
        ]
        .concat(),
    );
    f.pal(
        "nested/prepare",
        &[
            a(
                "CreateSeparation",
                "<SeparationName>Base ink</SeparationName><PreviewRed>1</PreviewRed><PreviewGreen>0</PreviewGreen><PreviewBlue>1</PreviewBlue>",
            ),
            a("AddWhiteUnderPrint", ""),
            a("DeleteObjects", ""),
        ]
        .concat(),
    );
    let mut s = Session::new();
    let result = f.run(&mut s);
    assert_eq!(result["status"], "stopped");
    assert_eq!(result["stoppedAt"]["list"], "nested/prepare");
    assert_eq!(result["stoppedAt"]["position"], 2);
    assert_eq!(result["traceLength"], 12);
    assert_eq!(result["artwork"]["allArt"], true);
    assert_eq!(result["spots"], json!(["Base ink"]));
    let d = &s.doc().unwrap().doc;
    let b = d.bounds_of(&selection::all_objects(d, false), true).unwrap();
    assert!((b.width() - 30.0).abs() < 0.01 && (b.height() - 30.0).abs() < 0.01);
    let bytes = std::fs::read(f.0.join("output.pdf")).unwrap();
    assert!(vectorcraft_pdf::import(&bytes).is_ok());
    assert!(vectorcraft_pdf::editing(&bytes).unwrap().intact, "preparation output must retain editable object names");
    let reopened = s.execute("document.open", &json!({"path":f.0.join("output.pdf")})).unwrap();
    assert_eq!(reopened["restored"], true);
    assert!(s.doc().unwrap().doc.swatches.iter().any(|w| w.spot && w.name == "Base ink"));
    assert!(String::from_utf8_lossy(&bytes).contains("/PALInk0 [/Separation /#42#61#73#65#20#69#6E#6B"));
}
#[test]
fn missing_and_recursive_calls_stop_at_the_call_without_executing_following_steps() {
    for target in ["missing", "main"] {
        let f = Fixture::new();
        f.input();
        f.pal("main", &[a("CallActionList", &format!("<Path>{target}</Path>")), a("SelectAllObjects", "")].concat());
        let v = f.run(&mut Session::new());
        assert_eq!(v["status"], "stopped");
        assert_eq!(v["traceLength"], 1);
        assert_eq!(v["stoppedAt"]["action"], "CallActionList");
        assert!(f.0.join("output.pdf").exists());
    }
}
#[test]
fn selections_are_independent_and_locked_layers_are_excluded() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("shape.rectangle", &json!({"x":0,"y":0,"width":20,"height":30})).unwrap();
    let mut p = player(&mut s);
    step(&mut p, "SelectAllObjects", "").unwrap();
    step(&mut p, "RenameObjects", "<ObjectName>Red star</ObjectName>").unwrap();
    step(&mut p, "CreateLayerOnTop", "<LayerName>Destination</LayerName>").unwrap();
    assert_eq!(p.ids().unwrap().len(), 1);
    step(&mut p, "MoveObjectsToLayer", "").unwrap();
    step(&mut p, "LockUnlockLayers", "<ChangeLockState>Lock</ChangeLockState>").unwrap();
    step(&mut p, "SelectAllObjects", "").unwrap();
    assert!(p.ids().unwrap().is_empty());
    step(&mut p, "LockUnlockLayers", "<ChangeLockState>Unlock</ChangeLockState>").unwrap();
    step(&mut p,"SelectObjectsByAttributes","<SelectionMode>NewSelection</SelectionMode><Rule Type=\"NameRule\"><NameRelation>Contains</NameRelation><SearchString>star</SearchString></Rule>").unwrap();
    assert_eq!(p.ids().unwrap().len(), 1);
    step(&mut p, "SelectLayerByName", "<LayerName>Absent</LayerName>").unwrap();
    assert_eq!(p.ids().unwrap().len(), 1);
    assert!(p.layers.is_empty());
}
#[test]
fn empty_selection_is_a_noop_but_ink_creation_is_not() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    let mut p = player(&mut s);
    for kind in ["GroupObjects", "UngroupObjects", "Copy", "PasteInFront", "MoveObjectsToLayer", "DeleteObjects"] {
        assert_eq!(step(&mut p, kind, "").unwrap(), 0);
    }
    step(
        &mut p,
        "CreateSeparation",
        "<SeparationName>New spot</SeparationName><PreviewRed>0</PreviewRed><PreviewGreen>1</PreviewGreen><PreviewBlue>0</PreviewBlue>",
    )
    .unwrap();
    step(
        &mut p,
        "ConvertToCMYK",
        "<Option>Custom</Option><CyanValue>0</CyanValue><MagentaValue>0</MagentaValue><YellowValue>0</YellowValue><BlackValue>50</BlackValue>",
    )
    .unwrap();
    assert!(!p.doc().unwrap().swatches.iter().any(|w| w.name == "New spot"));
}
#[test]
fn groups_and_conditional_branches_execute_in_order() {
    let f = Fixture::new();
    f.input();
    let rename = a("RenameObjects", "<ObjectName>Selected</ObjectName>");
    let condition = format!(
        "<Operator>All</Operator><Rule Type=\"ObjectSelectionRule\"><ObjectRelation>OneOrMoreObjectsSelection</ObjectRelation></Rule><Then>{rename}</Then><Else>{}</Else>",
        a("DeleteObjects", "")
    );
    f.pal("main", &a("Group", &format!("<Actions>{}{}</Actions>", a("SelectAllObjects", ""), a("Conditional", &condition))));
    let mut s = Session::new();
    let v = f.run(&mut s);
    assert_eq!(v["status"], "completed");
    assert_eq!(v["traceLength"], 4);
    assert!(s.doc().unwrap().selection.objects.iter().all(|id| s.doc().unwrap().doc.node(*id).unwrap().name.as_deref() == Some("Selected")));
}
#[test]
fn malformed_xml_and_unknown_actions_do_not_panic() {
    use proptest::prelude::*;
    proptest!(|(text in ".{0,4096}")| { let _result=reader::parse(&text); });
    let f = Fixture::new();
    f.input();
    f.pal("main", &a("FutureAction", ""));
    let v = f.run(&mut Session::new());
    assert_eq!(v["stoppedAt"]["action"], "FutureAction");
    assert_eq!(v["traceLength"], 1);
}

#[test]
fn ink_conversion_is_document_wide_and_applies_a_spot_tint_once() {
    use vectorcraft_color::Color;
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("shape.rectangle", &json!({"x":0,"y":0,"width":20,"height":30})).unwrap();
    s.execute("shape.rectangle", &json!({"x":25,"y":0,"width":20,"height":30})).unwrap();
    let mut p = player(&mut s);
    step(&mut p, "SelectAllObjects", "").unwrap();
    let before = p.doc().unwrap().clone();
    assert_eq!(step(&mut p, "DeleteInk", "").unwrap(), 0);
    assert_eq!(*p.doc().unwrap(), before, "an empty ink selection must not convert RGB paint");
    step(
        &mut p,
        "CreateSeparation",
        "<SeparationName>Synthetic ink</SeparationName><PreviewRed>0</PreviewRed><PreviewGreen>0</PreviewGreen><PreviewBlue>0</PreviewBlue>",
    )
    .unwrap();
    step(&mut p, "SetFillFlatPaint", "<Percentage>50</Percentage>").unwrap();
    step(&mut p, "SetFillOverprint", "").unwrap();
    step(&mut p, "SetStrokeWidth", "<Width>0.4</Width>").unwrap();
    step(&mut p, "SetStrokeCorners", "<JoinType>Round</JoinType>").unwrap();
    let objects = p.ids().unwrap();
    p.session.doc_mut().unwrap().selection.clear();
    let affected = step(
        &mut p,
        "ConvertToCMYK",
        "<Option>Custom</Option><CyanValue>0</CyanValue><MagentaValue>0</MagentaValue><YellowValue>0</YellowValue><BlackValue>80</BlackValue>",
    )
    .unwrap();
    assert_eq!(affected, objects.len(), "conversion counts changed objects even without an object selection");
    for id in objects {
        let n = p.doc().unwrap().node(id).unwrap();
        assert_eq!(n.appearance.fill_paint().color(), Some(Color::cmyk(0.0, 0.0, 0.0, 0.4)));
        assert!(n.appearance.fill().unwrap().overprint);
        assert!((n.appearance.stroke_width() - 0.4 * 72.0 / 25.4).abs() < 1e-8);
        assert_eq!(n.appearance.stroke().unwrap().join, vectorcraft_doc::LineJoin::Round);
    }
}

#[test]
fn content_boxes_use_selection_and_relative_offsets_are_millimetres() {
    let mut s = Session::new();
    s.execute("file.new", &json!({})).unwrap();
    s.execute("shape.rectangle", &json!({"x":100,"y":100,"width":20,"height":30})).unwrap();
    let chosen = s.execute("shape.rectangle", &json!({"x":10,"y":20,"width":20,"height":30})).unwrap()["id"].as_u64().unwrap();
    s.execute("paint.setStroke", &json!({"ids":[chosen],"none":true})).unwrap();
    s.execute("select.set", &json!({"ids":[chosen]})).unwrap();
    let mut p = player(&mut s);
    p.boxes.insert("ArtBox".into(), Rect::new(0.0, 0.0, 100.0, 100.0));
    p.boxes.insert("MediaBox".into(), Rect::new(0.0, 0.0, 100.0, 100.0));
    step(&mut p, "SetPageBoxSizeByContent", "<PageBox>ArtBox</PageBox><Mode>FitAll</Mode><AdjustType>Both</AdjustType>").unwrap();
    assert_eq!(p.boxes["ArtBox"], Rect::new(10.0, 20.0, 30.0, 50.0));
    step(
        &mut p,
        "SetPageBoxSizeRelative",
        "<PageBox>MediaBox</PageBox><RefPageBox>ArtBox</RefPageBox><Top>3.2</Top><Left>3.2</Left><Right>3.2</Right><Bottom>3.2</Bottom>",
    )
    .unwrap();
    assert!((p.boxes["MediaBox"].width() - (20.0 + 6.4 * 72.0 / 25.4)).abs() < 1e-8);
}

#[test]
fn all_separation_geometry_boundaries_stop_even_with_no_selection() {
    for geometry in GEOMETRY {
        let f = Fixture::new();
        f.input();
        f.pal("main", &[a(geometry, ""), a("CreateLayerOnTop", "<LayerName>Must not exist</LayerName>")].concat());
        let mut s = Session::new();
        let v = f.run(&mut s);
        assert_eq!(v["status"], "stopped");
        assert_eq!(v["stoppedAt"]["action"], *geometry);
        assert_eq!(v["traceLength"], 1);
        assert!(!s.doc().unwrap().doc.layers.iter().any(|n| n.name.as_deref() == Some("Must not exist")));
    }
}
