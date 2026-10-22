//! Trap ticket actions and the explicit AE step between preparation and recipe.
use super::*;
use vectorcraft_doc::Node;

impl Player<'_> {
    pub(super) fn trap_tags(&mut self, p: &Element, remove: bool) -> Result<usize> {
        let ids = self.ids()?;
        if ids.is_empty() {
            return Ok(0);
        }
        let external = if remove {
            None
        } else {
            for (key, expected) in [
                ("TrapType", "Trapping"),
                ("ExternalPullback", "Never"),
                ("ExternalDirection", "Outwards"),
                ("InternalTrap", "Never"),
                ("InternalPullback", "Never"),
            ] {
                if required(p, key)? != expected {
                    return Err(error(format!("unsupported trap tag {key}")));
                }
            }
            for (key, expected) in [("ExternalDistance", 0.2), ("InternalDistance", 0.0)] {
                if number(p, key)? != expected || p.child(key).and_then(|e| e.attributes.get("Mode")).map(String::as_str) != Some("Set") {
                    return Err(error(format!("unsupported trap tag {key}")));
                }
            }
            Some(match required(p, "ExternalTrap")? {
                "Always" => true,
                "Never" => false,
                v => return Err(error(format!("unsupported ExternalTrap {v}"))),
            })
        };
        let recurse = remove && flag(p, "AllLevels")?;
        fn set(n: &mut Node, value: Option<bool>, recurse: bool) {
            n.extras_mut().trap_external = value;
            if recurse && let Some(cs) = n.children_mut() {
                for c in cs {
                    set(std::sync::Arc::make_mut(c), value, true);
                }
            }
        }
        self.session.edit("Trap", |d, _| {
            for id in &ids {
                if let Some(n) = d.node_mut(*id) {
                    set(n, external, recurse);
                }
            }
            Ok(ids.len())
        })
    }
    pub(super) fn trap_task(&mut self) -> Result<usize> {
        let r = super::super::sep_trap::trap_task(self.session)?;
        // The generated vector runs cover exactly their clip. Retain that proven footprint
        // so underprint need not boolean-union hundreds of thousands of pixel-run edges.
        // The cache is used only while the complete group still equals this snapshot.
        if let Some(layer) = self.doc()?.layers.iter().find(|n| n.name.as_deref() == Some("Trap Layer 1"))
            && let Some(group) = layer.children().and_then(|c| c.first())
            && let Some(clip) = group.children().and_then(|c| c.first())
            && let Some((path, _)) = super::super::pathops::node_path(clip)
        {
            self.trap_footprints.insert(group.id, ((**group).clone(), path));
        }
        self.messages.push(format!("TrapTask: {} pixels; {} seconds", r["pixels"], r["seconds"]));
        self.messages.push(format!(
            "Trap tag conflicts: {} paints excluded by ink rules; {} paints excluded by tags",
            r["tagConflicts"]["taggedPaintsExcludedByInkRules"], r["tagConflicts"]["inkEligiblePaintsExcludedByTags"]
        ));
        Ok(usize::from(r["pixels"].as_u64().unwrap_or(0) > 0))
    }
    pub(super) fn rasterize(&mut self, p: &Element) -> Result<usize> {
        let ids = actions::roots(self.doc()?, &self.ids()?);
        if ids.is_empty() {
            return Ok(0);
        }
        if number(p, "Resolution")? != 600.0 || required(p, "AntiAliasing")? != "None" || flag(p, "ConsiderBackground")? {
            return Err(error("Rasterize supports 600 dpi, AntiAliasing None, ConsiderBackground 0"));
        }
        super::super::sep_trap::rasterize(self.session, &ids)
    }
}
