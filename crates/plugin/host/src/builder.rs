use std::collections::HashMap;

use anyhow::Context;
use anyhow::Result;
use futuresdr::runtime::BlockId;
use futuresdr::runtime::BlockRef;
use futuresdr::runtime::Flowgraph;
use plugin_api::Added;

use crate::connect::Kind;
use crate::description::Description;
use crate::registry::Registry;

/// The blocks of a built flowgraph, by name.
#[derive(Debug, Default)]
pub struct Blocks(HashMap<String, Added>);

impl Blocks {
    /// Id of block `name`.
    pub fn id(&self, name: &str) -> Option<BlockId> {
        self.0.get(name).map(|b| b.id)
    }

    /// Typed reference to block `name`, if its kernel is `K`. Use it with
    /// [`TerminatedFlowgraph::block`](futuresdr::runtime::TerminatedFlowgraph::block)
    /// once the flowgraph has run.
    pub fn block_ref<K: 'static>(&self, name: &str) -> Option<BlockRef<K>> {
        self.0
            .get(name)?
            .block_ref
            .downcast_ref::<BlockRef<K>>()
            .copied()
    }

    /// Block names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

/// A flowgraph built from a [`Description`], not started yet.
pub struct Built {
    /// The flowgraph; more blocks and connections may still be added.
    pub flowgraph: Flowgraph,
    /// Its blocks, by name.
    pub blocks: Blocks,
}

/// Build the flowgraph `desc` describes, with block types from `registry`.
///
/// Plugins listed in `desc` must already be loaded (see
/// [`Registry::load_all`]). Ports are left unconnected; running the result
/// on its own requires a description without inputs.
pub fn build(registry: &Registry, desc: &Description) -> Result<Built> {
    let mut flowgraph = Flowgraph::new();
    let mut blocks = HashMap::new();
    for decl in &desc.blocks {
        let added = registry.add(&mut flowgraph, &decl.type_name, &decl.settings)?;
        blocks.insert(decl.name.clone(), added);
    }
    for link in &desc.links {
        let src = blocks[&link.src].id;
        let dst = blocks[&link.dst].id;
        match link.kind {
            Kind::Stream => {
                flowgraph.stream_dyn(src, link.src_port.as_str(), dst, link.dst_port.as_str())
            }
            Kind::Message => {
                flowgraph.message(src, link.src_port.as_str(), dst, link.dst_port.as_str())
            }
        }
        .with_context(|| format!("connections line {}: {link}", link.line))?;
    }
    Ok(Built {
        flowgraph,
        blocks: Blocks(blocks),
    })
}
