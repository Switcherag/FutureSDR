use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use futuresdr::runtime::Flowgraph;
use libloading::os::unix::Library;
use libloading::os::unix::RTLD_LOCAL;
use libloading::os::unix::RTLD_NOW;
use plugin_api::ABI_VERSION;
use plugin_api::Abi;
use plugin_api::Added;
use plugin_api::BlockType;
use plugin_api::ENTRY_SYMBOL;
use plugin_api::Plugin;
use plugin_api::Settings;

/// Where a block type was registered from.
#[derive(Debug, Clone)]
pub struct Origin {
    /// Name of the plugin.
    pub plugin: &'static str,
    /// Its library, or `None` for a plugin linked into this program.
    pub library: Option<PathBuf>,
}

/// A registered block type.
#[derive(Clone)]
pub struct Entry {
    /// What the plugin exported.
    pub block: BlockType,
    /// Where it came from.
    pub origin: Origin,
}

/// Block types by name, from loaded plugin libraries and linked-in plugins.
///
/// Libraries are never unloaded: the blocks they create run their code, and
/// may outlive any registry.
#[derive(Default, Clone)]
pub struct Registry {
    types: BTreeMap<String, Entry>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin that is linked into this program, e.g. the result of
    /// calling a plugin crate's `futuresdr_plugin_entry()` directly.
    pub fn register(&mut self, plugin: Plugin) -> Result<()> {
        self.insert(plugin, None)
    }

    /// Load the plugin library at `path` and register its block types.
    ///
    /// All its symbols are resolved at once, so a library built against
    /// another build of `futuresdr-plugin-rt` is rejected here.
    pub fn load(&mut self, path: &Path) -> Result<Vec<String>> {
        let library = unsafe { Library::open(Some(path), RTLD_NOW | RTLD_LOCAL) }.with_context(|| {
            format!(
                "loading plugin {} (was it built against this program's SDK?)",
                path.display()
            )
        })?;
        let entry: fn() -> Plugin = *unsafe { library.get::<fn() -> Plugin>(ENTRY_SYMBOL.as_bytes()) }
            .with_context(|| format!("{} is not a FutureSDR plugin", path.display()))?;
        // Keep the code mapped for the rest of the process.
        std::mem::forget(library);

        let plugin = entry();
        let names = plugin.blocks.iter().map(|b| b.name.clone()).collect();
        self.insert(plugin, Some(path.to_path_buf()))?;
        Ok(names)
    }

    /// Load every plugin library (`lib*.so`) in `dir`, skipping the shared
    /// runtime library itself. Returns the paths loaded.
    pub fn load_dir(&mut self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                name.starts_with("lib")
                    && name.ends_with(".so")
                    && !name.starts_with("libfuturesdr_plugin_rt")
                    && !name.starts_with("libstd-")
            })
            .collect();
        paths.sort();
        for path in &paths {
            self.load(path)?;
        }
        Ok(paths)
    }

    fn insert(&mut self, plugin: Plugin, library: Option<PathBuf>) -> Result<()> {
        if plugin.abi.version != ABI_VERSION {
            bail!(
                "plugin '{}' uses plugin ABI {}, this program {}",
                plugin.name,
                plugin.abi.version,
                ABI_VERSION
            );
        }
        if plugin.abi != Abi::current() {
            bail!(
                "plugin '{}' is linked against another copy of futuresdr-plugin-rt",
                plugin.name
            );
        }
        if let Some(dup) = plugin.blocks.iter().find(|b| self.types.contains_key(&b.name)) {
            let other = &self.types[&dup.name].origin;
            bail!(
                "block type '{}' of plugin '{}' is already registered by plugin '{}'",
                dup.name,
                plugin.name,
                other.plugin
            );
        }
        for block in plugin.blocks {
            let origin = Origin {
                plugin: plugin.name,
                library: library.clone(),
            };
            self.types.insert(block.name.clone(), Entry { block, origin });
        }
        Ok(())
    }

    /// The block type `name`.
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.types.get(name)
    }

    /// All block type names, sorted.
    pub fn type_names(&self) -> impl Iterator<Item = &str> {
        self.types.keys().map(String::as_str)
    }

    /// Add a block of type `name` to `fg`.
    pub fn add(&self, fg: &mut Flowgraph, name: &str, settings: &Settings) -> Result<Added> {
        let entry = self.get(name).ok_or_else(|| {
            let similar: Vec<&str> = self
                .type_names()
                .filter(|t| t.split('<').next() == name.split('<').next())
                .collect();
            if similar.is_empty() {
                anyhow!("block '{}': unknown block type '{name}'", settings.block())
            } else {
                anyhow!(
                    "block '{}': unknown block type '{name}' (available: {})",
                    settings.block(),
                    similar.join(", ")
                )
            }
        })?;
        (entry.block.add)(fg, settings)
            .with_context(|| format!("block '{}' ({name})", settings.block()))
    }
}
