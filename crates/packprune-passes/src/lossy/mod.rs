use std::collections::BTreeMap;

use packprune_graph::atlases::{AtlasFile, VanillaAtlases, ViewAtlases};
use packprune_pack::{FileKind, LogicalPath, PhysPath, ResourceId, View, ViewId};
use packprune_pass::{FileOptimizer, Pass, PassCx, PassError};

mod png;
mod prune;
mod texel;
mod uses;

#[cfg(test)]
pub(crate) mod tests;

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(prune::PruneUnnamedSprites),
        Box::new(texel::TexelDownscale),
    ]
}

pub fn optimisers() -> Vec<Box<dyn FileOptimizer>> {
    vec![
        Box::new(png::Png::Alpha),
        Box::new(png::Png::Quantize),
        Box::new(png::Png::SingleColor),
    ]
}

struct Atlases {
    views: BTreeMap<ViewId, ViewAtlases>,
    vanilla: ViewAtlases,
}

impl Atlases {
    fn build(cx: &PassCx<'_>, pass: &'static str) -> Result<Self, PassError> {
        let failed = |error| PassError::Failed {
            pass,
            why: format!("{error}"),
        };
        let vanilla =
            VanillaAtlases::new(cx.run, cx.vanilla, &cx.layout.meta.filters).map_err(failed)?;
        let files: BTreeMap<PhysPath, AtlasFile> = cx
            .pack
            .files
            .iter()
            .filter(|(path, _)| cx.layout.kinds.get(*path) == Some(&FileKind::Atlas))
            .filter_map(|(path, entry)| {
                AtlasFile::parse(cx.run, &entry.blob).map(|file| (path.clone(), file))
            })
            .collect();
        let views = cx
            .layout
            .views
            .iter()
            .map(|view| {
                ViewAtlases::build(cx.run, &vanilla, view, |path| files.get(path))
                    .map(|atlases| (view.id, atlases))
                    .map_err(failed)
            })
            .collect::<Result<_, _>>()?;
        let mut empty: View = cx.layout.views[0].clone();
        empty.files.clear();
        let vanilla = ViewAtlases::build(cx.run, &vanilla, &empty, |_| None).map_err(failed)?;
        Ok(Self { views, vanilla })
    }
}

fn resource_file(
    cx: &PassCx<'_>,
    id: ResourceId,
    folder: &str,
    suffix: &str,
) -> Option<LogicalPath> {
    LogicalPath::new(&format!(
        "assets/{}/{folder}/{}{suffix}",
        cx.run.interner.resolve(id.ns),
        cx.run.interner.resolve(id.path)
    ))
    .ok()
}
