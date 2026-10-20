//! Bounded PAL XML reader. Action names and parameter strings retain their spelling.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const MAX_XML_BYTES: u64 = 4 * 1024 * 1024;
const MAX_NESTING: usize = 32;
const MAX_ITEMS: usize = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Element {
    pub name: String,
    pub attributes: BTreeMap<String, String>,
    pub text: String,
    pub children: Vec<Element>,
}

impl Element {
    pub fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|e| e.name == name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.child(name).map(|e| e.text.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Action {
    pub kind: String,
    /// One-based position among the immediate container's actions.
    pub position: usize,
    pub params: Element,
    pub children: Vec<Action>,
    pub then_actions: Vec<Action>,
    pub else_actions: Vec<Action>,
}

fn element(node: roxmltree::Node<'_, '_>, depth: usize) -> Result<Element, String> {
    if depth > MAX_NESTING {
        return Err("PAL XML nesting exceeds 32 levels".into());
    }
    Ok(Element {
        name: node.tag_name().name().into(),
        attributes: node.attributes().map(|a| (a.name().into(), a.value().into())).collect(),
        text: node.children().filter(|n| n.is_text()).filter_map(|n| n.text()).collect(),
        children: node.children().filter(|n| n.is_element()).map(|n| element(n, depth + 1)).collect::<Result<_, _>>()?,
    })
}

fn actions(container: &Element, depth: usize, count: &mut usize) -> Result<Vec<Action>, String> {
    if depth > MAX_NESTING {
        return Err("PAL action nesting exceeds 32 levels".into());
    }
    let mut out = Vec::new();
    for item in container.children.iter().filter(|e| e.name == "ActionListItem") {
        *count += 1;
        if *count > MAX_ITEMS {
            return Err("PAL has more than 10000 actions".into());
        }
        let kind = item.attributes.get("Type").filter(|v| !v.is_empty()).ok_or("PAL action has no Type")?.clone();
        let params = item.child("Parameters").cloned().unwrap_or_else(|| Element {
            name: "Parameters".into(),
            attributes: BTreeMap::new(),
            text: String::new(),
            children: Vec::new(),
        });
        let mut branch = |name| match params.child(name) {
            Some(e) => actions(e, depth + 1, count),
            None => Ok(Vec::new()),
        };
        let children = branch("Actions")?;
        let then_actions = branch("Then")?;
        let else_actions = branch("Else")?;
        out.push(Action { kind, position: out.len() + 1, params, children, then_actions, else_actions });
    }
    Ok(out)
}

pub(super) fn parse(xml: &str) -> Result<Vec<Action>, String> {
    if xml.len() as u64 > MAX_XML_BYTES {
        return Err("PAL exceeds 4 MiB".into());
    }
    let parsed = roxmltree::Document::parse(xml).map_err(|e| format!("invalid PAL XML: {e}"))?;
    let root = element(parsed.root_element(), 0)?;
    if root.name != "ActionList" || root.attributes.get("Version").map(String::as_str) != Some("2.0") {
        return Err("expected PAL ActionList Version 2.0".into());
    }
    actions(&root, 0, &mut 0)
}

/// Folder-qualified calls are rooted at the library; bare calls use the caller's folder.
/// Canonicalization also prevents symlinks from escaping the supplied library root.
pub(super) fn resolve(root: &Path, caller: Option<&Path>, name: &str) -> Result<PathBuf, String> {
    let normalized = name.replace('\\', "/");
    let relative = Path::new(&normalized);
    if normalized.is_empty() || normalized.contains(':') || relative.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("PAL list paths must be relative names without parent traversal".into());
    }
    let folder = if relative.components().count() == 1 { caller.and_then(Path::parent).unwrap_or(root) } else { root };
    let mut path = folder.join(relative);
    if !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pal")) {
        let mut name = path.into_os_string();
        name.push(".pal");
        path = PathBuf::from(name);
    }
    crate::file_access::check_read(&path.to_string_lossy())?;
    let path = path.canonicalize().map_err(|e| format!("cannot resolve PAL list: {e}"))?;
    if !path.starts_with(root) {
        return Err("PAL list resolves outside the action-list root".into());
    }
    Ok(path)
}

pub(super) fn read(path: &Path) -> Result<Vec<Action>, String> {
    crate::file_access::check_read(&path.to_string_lossy())?;
    let file = std::fs::File::open(path).map_err(|e| format!("cannot open PAL list: {e}"))?;
    let mut xml = String::new();
    file.take(MAX_XML_BYTES + 1).read_to_string(&mut xml).map_err(|e| format!("cannot read PAL list: {e}"))?;
    parse(&xml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_rules_branches_names_and_positions() {
        let xml = r#"<ActionList Version="2.0">
          <ActionListItem Type="Group"><Parameters><Actions>
            <ActionListItem Type="Conditional"><Parameters><Operator>All</Operator>
              <Rule Type="ObjectSelectionRule"><ObjectRelation>OneOrMoreObjectsSelection</ObjectRelation></Rule>
              <Then><ActionListItem Type="RenameObjects"><Parameters><ObjectName>A &amp; B</ObjectName></Parameters></ActionListItem></Then>
              <Else><ActionListItem Type="ShowMessage"/></Else>
            </Parameters></ActionListItem>
          </Actions></Parameters></ActionListItem>
          <ActionListItem Type="SelectAllObjects"/>
        </ActionList>"#;
        let list = parse(xml).unwrap();
        assert_eq!(list[1].position, 2);
        let conditional = &list[0].children[0];
        assert_eq!(conditional.kind, "Conditional");
        assert_eq!(conditional.params.child("Rule").unwrap().attributes["Type"], "ObjectSelectionRule");
        assert_eq!(conditional.then_actions[0].params.value("ObjectName"), Some("A & B"));
        assert_eq!(conditional.else_actions[0].kind, "ShowMessage");
    }

    #[test]
    fn rejects_invalid_envelopes_and_entities() {
        for xml in [
            "<ActionList>",
            "<ActionList Version=\"3.0\"/>",
            "<ActionList Version=\"2.0\"><ActionListItem/></ActionList>",
            "<!DOCTYPE ActionList [<!ENTITY x 'text'>]><ActionList Version=\"2.0\">&x;</ActionList>",
        ] {
            assert!(parse(xml).is_err());
        }
    }

    #[test]
    fn rejects_traversal_before_reading() {
        for name in ["../secret", "a/../../secret", "C:\\secret", "/secret", ""] {
            assert!(resolve(Path::new("/synthetic"), None, name).is_err());
        }
    }
}
