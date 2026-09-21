//! Descriptions with swappable blocks, run as several flowgraphs.
//!
//! A block listed in a description's `swappable` runs as a flowgraph of its
//! own (a *segment*), and the connections between it and the other blocks
//! become links of the controller, which carry items and their tags. The
//! rest of the blocks run as the *main* flowgraph. Replacing one of these
//! blocks is then replacing its segment, which leaves the main flowgraph
//! and its state as they are.

use std::collections::BTreeMap;

use anyhow::Result;
use anyhow::anyhow;

use crate::Description;
use crate::ItemType;
use crate::MessagePortDecl;
use crate::PortDecl;
use crate::connect::Kind;

/// A description cut into flowgraphs.
#[derive(Debug, Clone)]
pub(crate) struct Split {
    /// The blocks that are not swappable, with the description's ports on
    /// them.
    pub main: Description,
    /// Each swappable block, as a description of its own.
    pub segments: Vec<(String, Description)>,
    /// Links between them, `(from part, output, to part, input)`; a part is
    /// `None` for the main flowgraph, else the swappable block's name.
    pub links: Vec<(Option<String>, String, Option<String>, String)>,
    /// The description's ports that are on a segment: `(port, block)`.
    pub moved: Vec<(String, String)>,
}

/// The name of the port of a cut connection `k`, on both sides.
fn cut(k: usize) -> String {
    format!("__cut{k}")
}

/// A description with no blocks, ports or connections, the rest as `desc`.
fn empty_like(desc: &Description) -> Description {
    Description {
        name: desc.name.clone(),
        plugins: desc.plugins.clone(),
        ..Description::default()
    }
}

impl Split {
    pub(crate) fn new(desc: &Description) -> Result<Self> {
        let swappable = |block: &str| desc.swappable.iter().any(|s| s.block == block);
        let part_of = |block: &str| swappable(block).then(|| block.to_string());

        let mut main = empty_like(desc);
        main.radio = desc.radio.clone();
        let mut segments: BTreeMap<String, Description> = desc
            .swappable
            .iter()
            .map(|s| (s.block.clone(), empty_like(desc)))
            .collect();
        macro_rules! part {
            ($p:expr) => {
                pick(&mut main, &mut segments, $p)
            };
        }

        for block in &desc.blocks {
            part!(&part_of(&block.name)).blocks.push(block.clone());
        }

        // The item type of a block's stream port: from the swappable
        // declaration, else from the block's type.
        let item_of = |block: &str, port: &str| -> Option<ItemType> {
            desc.swappable
                .iter()
                .find(|s| s.block == block)
                .and_then(|s| s.items.get(port).copied())
                .or_else(|| {
                    let decl = desc.blocks.iter().find(|b| b.name == block)?;
                    ItemType::of_block_type(&decl.type_name)
                })
        };

        let mut links = Vec::new();
        for link in &desc.links {
            let (from, to) = (part_of(&link.src), part_of(&link.dst));
            if from == to {
                part!(&from).links.push(link.clone());
                continue;
            }
            let name = cut(links.len());
            match link.kind {
                Kind::Stream => {
                    let item = item_of(&link.src, &link.src_port)
                        .or_else(|| item_of(&link.dst, &link.dst_port))
                        .ok_or_else(|| {
                            anyhow!(
                                "swappable: cannot tell the item type of {link}; give it, e.g. \
                                 [swappable] {} = {{ {} = \"u8\" }}",
                                from.as_ref().or(to.as_ref()).unwrap(),
                                if from.is_some() {
                                    &link.src_port
                                } else {
                                    &link.dst_port
                                },
                            )
                        })?;
                    part!(&from).outputs.push(PortDecl {
                        name: name.clone(),
                        block: link.src.clone(),
                        port: link.src_port.clone(),
                        item,
                    });
                    part!(&to).inputs.push(PortDecl {
                        name: name.clone(),
                        block: link.dst.clone(),
                        port: link.dst_port.clone(),
                        item,
                    });
                }
                Kind::Message => {
                    part!(&from).message_outputs.push(MessagePortDecl {
                        name: name.clone(),
                        block: link.src.clone(),
                        port: link.src_port.clone(),
                    });
                    part!(&to).message_inputs.push(MessagePortDecl {
                        name: name.clone(),
                        block: link.dst.clone(),
                        port: link.dst_port.clone(),
                    });
                }
            }
            links.push((from, name.clone(), to, name));
        }

        // The description's own ports, where their blocks are.
        let mut moved = Vec::new();
        for p in &desc.inputs {
            let at = part_of(&p.block);
            if let Some(b) = &at {
                moved.push((p.name.clone(), b.clone()));
            }
            part!(&at).inputs.push(p.clone());
        }
        for p in &desc.outputs {
            let at = part_of(&p.block);
            if let Some(b) = &at {
                moved.push((p.name.clone(), b.clone()));
            }
            part!(&at).outputs.push(p.clone());
        }
        for p in &desc.message_inputs {
            let at = part_of(&p.block);
            if let Some(b) = &at {
                moved.push((p.name.clone(), b.clone()));
            }
            part!(&at).message_inputs.push(p.clone());
        }
        for p in &desc.message_outputs {
            let at = part_of(&p.block);
            if let Some(b) = &at {
                moved.push((p.name.clone(), b.clone()));
            }
            part!(&at).message_outputs.push(p.clone());
        }
        for p in &desc.controls {
            part!(&part_of(&p.block)).controls.push(p.clone());
        }

        Ok(Self {
            main,
            segments: segments.into_iter().collect(),
            links,
            moved,
        })
    }
}

/// The description of part `p`: the main one, or a swappable block's.
fn pick<'a>(
    main: &'a mut Description,
    segments: &'a mut BTreeMap<String, Description>,
    p: &Option<String>,
) -> &'a mut Description {
    match p {
        None => main,
        Some(block) => segments.get_mut(block).unwrap(),
    }
}

/// Whether two block declarations are the same block.
fn same_block(a: &crate::BlockDecl, b: &crate::BlockDecl) -> bool {
    if a.name != b.name || a.type_name != b.type_name {
        return false;
    }
    let mut ka: Vec<&str> = a.settings.keys().collect();
    let mut kb: Vec<&str> = b.settings.keys().collect();
    ka.sort_unstable();
    kb.sort_unstable();
    ka == kb && ka.iter().all(|k| a.settings.raw(k) == b.settings.raw(k))
}

/// The swappable blocks `new` changes from `old`, if they are all it
/// changes (its name aside); `None` if anything else differs.
pub(crate) fn changed_swappable(old: &Description, new: &Description) -> Option<Vec<String>> {
    let same_links = old.links.len() == new.links.len()
        && old.links.iter().zip(&new.links).all(|(a, b)| {
            (a.kind, &a.src, &a.src_port, &a.dst, &a.dst_port)
                == (b.kind, &b.src, &b.src_port, &b.dst, &b.dst_port)
        });
    let same = old.swappable == new.swappable
        && old.plugins == new.plugins
        && same_links
        && old.inputs == new.inputs
        && old.outputs == new.outputs
        && old.message_inputs == new.message_inputs
        && old.message_outputs == new.message_outputs
        && old.controls == new.controls
        && old.radio == new.radio
        && old.blocks.len() == new.blocks.len();
    if !same {
        return None;
    }
    let mut changed = Vec::new();
    for (a, b) in old.blocks.iter().zip(&new.blocks) {
        if same_block(a, b) {
            continue;
        }
        if a.name != b.name || !old.swappable.iter().any(|s| s.block == a.name) {
            return None;
        }
        changed.push(a.name.clone());
    }
    Some(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RX: &str = r#"
        name = "rx"
        connections = """
        src > filt > dec
        dec.out | sink
        """
        swappable = ["filt"]
        [blocks.src]
        type = "Copy<f32>"
        [blocks.filt]
        type = "Copy<f32>"
        [blocks.dec]
        type = "Copy<f32>"
        [blocks.sink]
        type = "MessageSink"
        [inputs]
        samples = "src.input"
        [message_outputs]
        filtered = "filt.out"
    "#;

    #[test]
    fn a_swappable_block_runs_apart_with_its_connections_cut() {
        let desc = Description::from_toml(RX).unwrap();
        let split = Split::new(&desc).unwrap();
        let names: Vec<&str> = split.main.blocks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["src", "dec", "sink"]);
        let (block, seg) = &split.segments[0];
        assert_eq!(block, "filt");
        assert_eq!(seg.blocks.len(), 1);
        // src > filt and filt > dec are cut; dec.out | sink stays.
        assert_eq!(split.main.links.len(), 1);
        assert_eq!(
            split.links,
            [
                (None, "__cut0".into(), Some("filt".into()), "__cut0".into()),
                (Some("filt".into()), "__cut1".into(), None, "__cut1".into()),
            ]
        );
        assert_eq!(seg.inputs[0].item, ItemType::F32);
        assert_eq!(split.moved, [("filtered".to_string(), "filt".to_string())]);
        assert!(split.main.inputs.iter().any(|p| p.name == "samples"));
    }

    #[test]
    fn only_swappable_blocks_may_change() {
        let old = Description::from_toml(RX).unwrap();
        let other =
            |from: &str, to: &str| Description::from_toml(&RX.replacen(from, to, 1)).unwrap();
        // filt is the second Copy<f32>.
        let new = Description::from_toml(&RX.replace(
            "[blocks.filt]\n        type = \"Copy<f32>\"",
            "[blocks.filt]\n        type = \"Delay<f32>\"\n        n = 1",
        ))
        .unwrap();
        assert_eq!(
            changed_swappable(&old, &new),
            Some(vec!["filt".to_string()])
        );
        assert_eq!(changed_swappable(&old, &old), Some(vec![]));
        let name_only = other("name = \"rx\"", "name = \"rx2\"");
        assert_eq!(changed_swappable(&old, &name_only), Some(vec![]));
        let dec = Description::from_toml(&RX.replace(
            "[blocks.dec]\n        type = \"Copy<f32>\"",
            "[blocks.dec]\n        type = \"Delay<f32>\"",
        ))
        .unwrap();
        assert_eq!(changed_swappable(&old, &dec), None, "dec is not swappable");
    }

    #[test]
    fn an_item_type_that_cannot_be_told_is_asked_for() {
        let desc = Description::from_toml(
            r#"
            connections = "a > b"
            swappable = ["b"]
            [blocks.a]
            type = "Mystery"
            [blocks.b]
            type = "Enigma"
            "#,
        )
        .unwrap();
        let err = Split::new(&desc).unwrap_err().to_string();
        assert!(err.contains("[swappable] b = { input = \"u8\" }"), "{err}");
        let desc = Description::from_toml(
            r#"
            connections = "a > b"
            [swappable]
            b = { input = "u8" }
            [blocks.a]
            type = "Mystery"
            [blocks.b]
            type = "Enigma"
            "#,
        )
        .unwrap();
        assert_eq!(
            Split::new(&desc).unwrap().segments[0].1.inputs[0].item,
            ItemType::U8
        );
    }
}
