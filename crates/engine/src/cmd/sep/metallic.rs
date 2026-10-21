//! Object actions used by the metallic PALs.
use super::{Player, Result, actions, error, selection};
use vectorcraft_doc::{Node, NodeKind};

impl Player<'_> {
    pub(super) fn clip_on_shape(&mut self) -> Result<usize> {
        let roots = actions::roots(self.doc()?, &self.ids()?);
        let mut ordered: Vec<_> = selection::all_objects(self.doc()?, false).into_iter().filter(|id| roots.contains(id)).collect();
        let Some(top) = ordered.pop() else { return Ok(0) };
        if ordered.is_empty() {
            return Ok(0);
        }
        let mut mask = self.doc()?.node(top).cloned().ok_or_else(|| error("selected object disappeared"))?;
        super::super::object::as_clipping_path(&mut mask)?;
        // Wrap each contiguous selected sibling run separately. Unselected siblings,
        // groups and layers retain their original painting order and hierarchy.
        self.session.edit("Make Clipping Mask", |d, sel| {
            let mut runs: Vec<Vec<vectorcraft_doc::NodeId>> = Vec::new();
            for id in &ordered {
                let parent = d.parent_of(*id);
                let adjacent = runs.last().and_then(|r| r.last()).is_some_and(|last| {
                    d.parent_of(*last) == parent
                        && d.children(parent).is_some_and(|cs| {
                            cs.windows(2).any(|pair| pair.first().is_some_and(|n| n.id == *last) && pair.get(1).is_some_and(|n| n.id == *id))
                        })
                });
                if adjacent {
                    if let Some(run) = runs.last_mut() {
                        run.push(*id);
                    }
                } else {
                    runs.push(vec![*id]);
                }
            }
            d.remove(top)?;
            let mut selected = Vec::new();
            for run in runs {
                let clip = d.reid(&mask);
                let gid = d.alloc_id();
                d.wrap(&run, |mut children| {
                    children.insert(0, std::sync::Arc::new(clip));
                    Node::new(gid, NodeKind::Group { children, clip: true })
                })?;
                selected.push(gid);
            }
            sel.set(selected);
            Ok(ordered.len())
        })
    }
}
