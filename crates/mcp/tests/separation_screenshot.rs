//! Screenshots show the active separation view; ordinary file exports retain the artwork.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use serde_json::json;
use vectorcraft_mcp::{Backend, Headless};

#[test]
fn screenshot_uses_selected_inks() {
    let mut h = Headless::new();
    h.session.execute("file.new", &json!({"width":40,"height":40})).unwrap();
    let id = h.session.execute("shape.rectangle", &json!({"x":0,"y":0,"width":40,"height":40})).unwrap()["id"].clone();
    h.session.execute("paint.setFill", &json!({"ids":[id],"color":{"c":100,"m":0,"y":0,"k":0}})).unwrap();
    let mut pixel = |plate: &str| {
        h.session.execute("view.separationsPreview", &json!({"on":true,"only":plate})).unwrap();
        let reply = h.call("ui.screenshot", json!({})).unwrap();
        let bytes = vectorcraft_format::base64_decode(reply["pngBase64"].as_str().unwrap()).unwrap();
        image::load_from_memory(&bytes).unwrap().to_rgb8().get_pixel(20, 20).0
    };
    let cyan = pixel("Cyan");
    let magenta = pixel("Magenta");
    assert_ne!(cyan, magenta);
    assert_eq!(magenta, [255, 255, 255]);
    vectorcraft_render::proof::set_view(Default::default());
}
