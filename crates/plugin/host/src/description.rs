use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use futuresdr::runtime::Pmt;
use plugin_api::Settings;
use toml::Value;

use crate::connect;
use crate::connect::Kind;
use crate::connect::Link;
use crate::items::ItemType;

/// A flowgraph as written in a TOML file.
///
/// ```toml
/// name = "rx"                                 # optional
/// plugins = ["plugins/libfsdr_blocks_basic.so"]  # relative to this file
///
/// # FutureSDR `connect!` syntax; see `connect`
/// connections = """
/// src > head > snk
/// """
///
/// [blocks.src]
/// type = "VectorSource<f32>"   # a registered block type
/// items = [1.0, 2.0, 3.0]      # everything else is a setting
///
/// [blocks.head]
/// type = "Head<f32>"
/// n_items = 2
///
/// [blocks.snk]
/// type = "VectorSink<f32>"
///
/// # ports other flowgraphs can link to (see `Controller`)
/// [inputs]
/// samples = "head.input"
///
/// [outputs]
/// kept = { port = "head.output", type = "f32" }
/// ```
///
/// A port's item type defaults to the parameter of its block's type
/// (`f32` for `Head<f32>`).
#[derive(Debug, Clone, Default)]
pub struct Description {
    /// Optional name.
    pub name: Option<String>,
    /// Plugin libraries the flowgraph needs.
    pub plugins: Vec<PathBuf>,
    /// Blocks, in file order.
    pub blocks: Vec<BlockDecl>,
    /// Connections, in order.
    pub links: Vec<Link>,
    /// Stream inputs other flowgraphs can feed.
    pub inputs: Vec<PortDecl>,
    /// Stream outputs other flowgraphs can read.
    pub outputs: Vec<PortDecl>,
}

/// One block of a [`Description`].
#[derive(Debug, Clone)]
pub struct BlockDecl {
    /// Name, unique in the flowgraph.
    pub name: String,
    /// Registered block type, e.g. `Head<f32>`.
    pub type_name: String,
    /// Its settings.
    pub settings: Settings,
}

/// A port of a flowgraph, bound to a port of one of its blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortDecl {
    /// Port name, unique in the flowgraph.
    pub name: String,
    /// Block it is bound to.
    pub block: String,
    /// Stream port of that block.
    pub port: String,
    /// Item type carried.
    pub item: ItemType,
}

const KEYS: [&str; 6] = [
    "name",
    "plugins",
    "connections",
    "blocks",
    "inputs",
    "outputs",
];

impl Description {
    /// Parse a description.
    pub fn from_toml(text: &str) -> Result<Self> {
        Self::parse(text, None)
    }

    /// Read a description file; plugin paths are relative to its directory.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text, path.parent()).with_context(|| format!("in {}", path.display()))
    }

    fn parse(text: &str, base: Option<&Path>) -> Result<Self> {
        let table: toml::Table = toml::from_str(text)?;
        if let Some(key) = table.keys().find(|k| !KEYS.contains(&k.as_str())) {
            bail!("unknown key '{key}' (expected one of {})", KEYS.join(", "));
        }

        let name = match table.get("name") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => bail!("'name' must be a string"),
        };

        let plugins = strings(table.get("plugins"), "plugins")?
            .into_iter()
            .map(|p| match base {
                Some(base) if Path::new(&p).is_relative() => base.join(p),
                _ => PathBuf::from(p),
            })
            .collect();

        let mut blocks = Vec::new();
        match table.get("blocks") {
            None => {}
            Some(Value::Table(decls)) => {
                for (name, decl) in decls {
                    blocks.push(block_decl(name, decl)?);
                }
            }
            Some(_) => bail!("'blocks' must be a table of blocks ([blocks.<name>])"),
        }

        let text = strings(table.get("connections"), "connections")?.join("\n");
        let connections = connect::parse(&text).map_err(|e| anyhow!("connections:{e}"))?;

        let desc = Self {
            name,
            plugins,
            inputs: ports(table.get("inputs"), "inputs", &blocks)?,
            outputs: ports(table.get("outputs"), "outputs", &blocks)?,
            links: connections.links,
            blocks,
        };
        desc.validate(&connections.blocks)?;
        Ok(desc)
    }

    fn validate(&self, mentioned: &[String]) -> Result<()> {
        let declared: HashSet<&str> = self.blocks.iter().map(|b| b.name.as_str()).collect();
        if let Some(missing) = mentioned.iter().find(|b| !declared.contains(b.as_str())) {
            let line = self
                .links
                .iter()
                .find(|l| &l.src == missing || &l.dst == missing)
                .map(|l| format!(" (connections line {})", l.line))
                .unwrap_or_default();
            bail!("block '{missing}' is connected but not declared{line}");
        }

        let mut names = HashSet::new();
        for port in self.inputs.iter().chain(&self.outputs) {
            if !names.insert(port.name.as_str()) {
                bail!("port '{}' is declared twice", port.name);
            }
        }

        // An input port is fed from outside; the block port must not also be
        // fed inside the flowgraph.
        let mut fed: HashMap<(&str, &str), String> = HashMap::new();
        for link in self.links.iter().filter(|l| l.kind == Kind::Stream) {
            let key = (link.dst.as_str(), link.dst_port.as_str());
            if let Some(first) = fed.insert(key, link.to_string()) {
                bail!(
                    "{}.{} is connected twice: {first} and {link}",
                    link.dst,
                    link.dst_port
                );
            }
        }
        for port in &self.inputs {
            if let Some(link) = fed.get(&(port.block.as_str(), port.port.as_str())) {
                bail!(
                    "input '{}' is bound to {}.{}, which is already connected: {link}",
                    port.name,
                    port.block,
                    port.port
                );
            }
        }
        Ok(())
    }

    /// The input or output port `name`.
    pub fn port(&self, name: &str) -> Option<&PortDecl> {
        self.inputs
            .iter()
            .chain(&self.outputs)
            .find(|p| p.name == name)
    }

    /// The block `name`.
    pub fn block(&self, name: &str) -> Option<&BlockDecl> {
        self.blocks.iter().find(|b| b.name == name)
    }
}

fn strings(value: Option<&Value>, key: &str) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("'{key}' must only contain strings"))
            })
            .collect(),
        Some(_) => bail!("'{key}' must be a string or a list of strings"),
    }
}

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

fn block_decl(name: &str, decl: &Value) -> Result<BlockDecl> {
    if !is_ident(name) {
        bail!("block name '{name}' must be an identifier (letters, digits, `_`)");
    }
    let Value::Table(fields) = decl else {
        bail!("block '{name}' must be a table with a 'type'");
    };
    let type_name = match fields.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(_) => bail!("block '{name}': 'type' must be a string"),
        None => bail!("block '{name}' has no 'type'"),
    };
    let values = fields
        .iter()
        .filter(|(k, _)| k.as_str() != "type")
        .map(|(k, v)| (k.clone(), to_pmt(v)))
        .collect();
    Ok(BlockDecl {
        name: name.to_string(),
        type_name,
        settings: Settings::new(name, values),
    })
}

fn ports(value: Option<&Value>, key: &str, blocks: &[BlockDecl]) -> Result<Vec<PortDecl>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Value::Table(entries) = value else {
        bail!("'{key}' must be a table of ports");
    };
    entries
        .iter()
        .map(|(name, entry)| {
            let (target, item) = match entry {
                Value::String(target) => (target.as_str(), None),
                Value::Table(fields) => {
                    let target = fields
                        .get("port")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("{key}.{name}: missing 'port = \"block.port\"'"))?;
                    let item = fields.get("type").map(|t| {
                        t.as_str()
                            .ok_or_else(|| anyhow!("{key}.{name}: 'type' must be a string"))
                            .and_then(|t| {
                                t.parse::<ItemType>()
                                    .map_err(|e| anyhow!("{key}.{name}: {e}"))
                            })
                    });
                    (target, item.transpose()?)
                }
                _ => bail!("{key}.{name} must be \"block.port\" or a table"),
            };
            let (block, port) = target
                .split_once('.')
                .ok_or_else(|| anyhow!("{key}.{name}: '{target}' is not \"block.port\""))?;
            let decl = blocks
                .iter()
                .find(|b| b.name == block)
                .ok_or_else(|| anyhow!("{key}.{name}: no block '{block}'"))?;
            let item = item
                .or_else(|| ItemType::of_block_type(&decl.type_name))
                .ok_or_else(|| {
                    anyhow!(
                        "{key}.{name}: cannot tell the item type from '{}'; add type = \"...\"",
                        decl.type_name
                    )
                })?;
            Ok(PortDecl {
                name: name.clone(),
                block: block.to_string(),
                port: port.to_string(),
                item,
            })
        })
        .collect()
}

/// A TOML value as a [`Pmt`].
pub fn to_pmt(value: &Value) -> Pmt {
    match value {
        Value::String(s) => Pmt::String(s.clone()),
        Value::Integer(i) => Pmt::Isize(*i as isize),
        Value::Float(f) => Pmt::F64(*f),
        Value::Boolean(b) => Pmt::Bool(*b),
        Value::Datetime(d) => Pmt::String(d.to_string()),
        Value::Array(items) => Pmt::VecPmt(items.iter().map(to_pmt).collect()),
        Value::Table(fields) => {
            Pmt::MapStrPmt(fields.iter().map(|(k, v)| (k.clone(), to_pmt(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAIN: &str = r#"
        name = "chain"
        plugins = ["libfsdr_blocks_basic.so"]
        connections = """
        src > head
        head > snk
        """

        [blocks.src]
        type = "VectorSource<f32>"
        items = [1.0, 2, 3.5]

        [blocks.head]
        type = "Head<f32>"
        n_items = 2

        [blocks.snk]
        type = "VectorSink<f32>"

        [outputs]
        copy = { port = "head.output", type = "c32" }
        tap = "src.output"
    "#;

    #[test]
    fn parses_blocks_links_and_ports() {
        let d = Description::parse(CHAIN, Some(Path::new("/opt/fg"))).unwrap();
        assert_eq!(d.name.as_deref(), Some("chain"));
        assert_eq!(
            d.plugins,
            [PathBuf::from("/opt/fg/libfsdr_blocks_basic.so")]
        );
        assert_eq!(
            d.blocks.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
            ["src", "head", "snk"],
            "file order is kept"
        );
        assert_eq!(
            d.block("head")
                .unwrap()
                .settings
                .get::<u64>("n_items")
                .unwrap(),
            2
        );
        assert_eq!(
            d.block("src")
                .unwrap()
                .settings
                .get::<Vec<f32>>("items")
                .unwrap(),
            [1.0, 2.0, 3.5]
        );
        assert_eq!(d.links.len(), 2);
        let copy = d.port("copy").unwrap();
        assert_eq!(
            (copy.block.as_str(), copy.port.as_str(), copy.item),
            ("head", "output", ItemType::Complex32)
        );
        assert_eq!(d.port("tap").unwrap().item, ItemType::F32);
    }

    #[test]
    fn rejects_mistakes() {
        let cases = [
            ("unknown = 1", "unknown key"),
            ("connections = 'a > b'", "not declared"),
            ("[blocks.a]\nn = 1", "no 'type'"),
            ("[blocks.a-b]\ntype = 'X'", "identifier"),
            (
                "connections = 'a >'\n[blocks.a]\ntype = 'X'",
                "connections:",
            ),
            (
                "connections = 'a > b; c > b'\n[blocks.a]\ntype='X'\n[blocks.b]\ntype='X'\n[blocks.c]\ntype='X'",
                "connected twice",
            ),
            (
                "connections = 'a > b'\n[blocks.a]\ntype='X'\n[blocks.b]\ntype='Head<f32>'\n[inputs]\nx = 'b.input'",
                "already connected",
            ),
            (
                "[blocks.a]\ntype='Mix'\n[inputs]\nx = 'a.input'",
                "item type",
            ),
            (
                "[blocks.a]\ntype='Head<u8>'\n[inputs]\nx = 'b.input'",
                "no block 'b'",
            ),
            (
                "[blocks.a]\ntype='Head<u8>'\n[inputs]\nx = 'a.input'\n[outputs]\nx = 'a.output'",
                "twice",
            ),
        ];
        for (text, expected) in cases {
            let err = format!("{:#}", Description::from_toml(text).unwrap_err());
            assert!(err.contains(expected), "{text:?}: {err}");
        }
    }
}
