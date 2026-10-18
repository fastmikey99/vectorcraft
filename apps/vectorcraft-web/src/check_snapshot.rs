//! Report validity follows the exported document even when the panel changes runs.
use std::sync::Arc;
use vectorcraft_doc::Document;
#[derive(Default)]
pub struct Snapshot {
    pub id: String,
    document: Option<Arc<Document>>,
}
impl Snapshot {
    pub fn replace(&mut self, id: String, document: Option<Arc<Document>>) {
        self.id = id;
        self.document = document;
    }
    pub fn validity(&self, current: Option<&Arc<Document>>) -> &'static str {
        if self.document.as_ref().zip(current).is_some_and(|(a, b)| Arc::ptr_eq(a, b)) { "provisional" } else { "stale" }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edit_undo_switch_and_replacement_follow_the_export_not_the_panel() {
        let original = Arc::new(Document::new(100.0, 100.0));
        let edited = Arc::new((*original).clone());
        let mut snapshot = Snapshot::default();
        snapshot.replace("export-1".into(), Some(original.clone()));
        assert_eq!(snapshot.validity(Some(&original)), "provisional");
        assert_eq!(snapshot.validity(Some(&edited)), "stale");
        assert_eq!(snapshot.validity(None), "stale");
        assert_eq!(snapshot.validity(Some(&original)), "provisional");
        snapshot.replace("export-2".into(), Some(edited.clone()));
        assert_eq!(snapshot.id, "export-2");
        assert_eq!(snapshot.validity(Some(&original)), "stale");
        assert_eq!(snapshot.validity(Some(&edited)), "provisional");
    }
}
