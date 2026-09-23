use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

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
use plugin_api::AddFn;
use plugin_api::Added;
use plugin_api::ENTRY_SYMBOL;
use plugin_api::Plugin;
use plugin_api::Settings;

/// Where a block type was registered from.
#[derive(Debug, Clone)]
pub struct Origin {
    /// Name of the plugin, copied out of its library: it is read after the
    /// library may have been unloaded.
    pub plugin: String,
    /// Its library, or `None` for a plugin linked into this program.
    pub library: Option<PathBuf>,
}

/// A registered block type.
///
/// It owns what it says about itself: a plugin's strings live in its library,
/// which [`Registry::unload`] may close.
#[derive(Clone)]
pub struct Entry {
    /// Name of the block type, e.g. `WlanDecoder<Ah>`.
    pub name: String,
    /// What the plugin says the block does.
    pub description: String,
    /// Where it came from.
    pub origin: Origin,
    /// Code in the library below.
    add: AddFn,
    /// The library `add` is in, kept mapped for as long as this entry lives;
    /// `None` for a plugin linked into this program.
    library: Option<Arc<Library>>,
}

impl Entry {
    /// Add a block of this type to `fg`.
    pub fn add(&self, fg: &mut Flowgraph, settings: &Settings) -> Result<Added> {
        (self.add)(fg, settings)
    }

    /// What keeps this block type's code mapped. Whatever holds a block built
    /// from it must hold this too, or unloading the library would leave the
    /// block's `work` and its destructor pointing into nothing.
    pub fn keepalive(&self) -> Keepalive {
        Keepalive(self.library.iter().cloned().collect())
    }
}

/// Holds plugin libraries open for as long as blocks built from them live.
///
/// A [`Blocks`](crate::Blocks) carries one; dropping the last holder of a
/// library closes it (see [`Registry::unload`]).
#[derive(Clone, Default)]
pub struct Keepalive(Vec<Arc<Library>>);

impl Keepalive {
    /// Hold `other`'s libraries as well.
    pub fn extend(&mut self, other: Keepalive) {
        for library in other.0 {
            if !self.0.iter().any(|held| Arc::ptr_eq(held, &library)) {
                self.0.push(library);
            }
        }
    }

    /// How many libraries it holds.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Keepalive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keepalive({} libraries)", self.0.len())
    }
}

/// Block types by name, from loaded plugin libraries and linked-in plugins.
///
/// A library stays mapped while anything built from it lives: its entries
/// here, the blocks of a flowgraph ([`Keepalive`]), and clones of this
/// registry all hold it. [`unload`](Self::unload) forgets its block types and
/// closes it once the last of those is gone.
#[derive(Default, Clone)]
pub struct Registry {
    types: BTreeMap<String, Entry>,
    libraries: BTreeMap<PathBuf, Arc<Library>>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin that is linked into this program, e.g. the result of
    /// calling a plugin crate's `futuresdr_plugin_entry()` directly.
    pub fn register(&mut self, plugin: Plugin) -> Result<()> {
        self.insert(plugin, None, None)
    }

    /// Load the plugin library at `path` and register its block types.
    /// Returns their names; nothing if the library is already loaded.
    ///
    /// All its symbols are resolved at once, so a library built against
    /// another build of `futuresdr-plugin-rt` is rejected here.
    pub fn load(&mut self, path: &Path) -> Result<Vec<String>> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("plugin {}", path.display()))?;
        if self.libraries.contains_key(&canonical) {
            return Ok(Vec::new());
        }
        let library =
            unsafe { Library::open(Some(path), RTLD_NOW | RTLD_LOCAL) }.with_context(|| {
                format!(
                    "loading plugin {} (was it built against this program's SDK?)",
                    path.display()
                )
            })?;
        let entry: fn() -> Plugin =
            *unsafe { library.get::<fn() -> Plugin>(ENTRY_SYMBOL.as_bytes()) }
                .with_context(|| format!("{} is not a FutureSDR plugin", path.display()))?;
        let library = Arc::new(library);

        let plugin = entry();
        let names = plugin.blocks.iter().map(|b| b.name.clone()).collect();
        self.insert(plugin, Some(canonical.clone()), Some(library.clone()))?;
        self.libraries.insert(canonical, library);
        Ok(names)
    }

    /// Load every library in `paths` that is not loaded yet.
    pub fn load_all<P: AsRef<Path>>(&mut self, paths: impl IntoIterator<Item = P>) -> Result<()> {
        for path in paths {
            self.load(path.as_ref())?;
        }
        Ok(())
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

    /// Forget the block types of the plugin library at `path` and close it,
    /// unless a flowgraph's blocks or a clone of this registry still hold it.
    /// Returns whether it was closed here.
    ///
    /// Closing a library that a running block came from would leave its
    /// `work` and its destructor pointing into nothing, which is why the
    /// library is only closed once every [`Keepalive`] of it is gone.
    pub fn unload(&mut self, path: &Path) -> Result<bool> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("plugin {}", path.display()))?;
        let Some(library) = self.libraries.remove(&canonical) else {
            return Ok(false);
        };
        self.types
            .retain(|_, e| e.origin.library.as_deref() != Some(canonical.as_path()));
        drop_pooled_buffers();
        // `into_inner` takes the library out if this was its last holder,
        // and dropping it closes it.
        Ok(Arc::into_inner(library).is_some())
    }

    /// Whether the plugin library at `path` is loaded.
    pub fn is_loaded(&self, path: &Path) -> bool {
        path.canonicalize()
            .is_ok_and(|p| self.libraries.contains_key(&p))
    }

    /// The library block type `name` came from, if it came from one.
    pub fn library_of(&self, name: &str) -> Option<&Path> {
        self.types.get(name)?.origin.library.as_deref()
    }

    /// How many references to `library` this registry itself holds: the one
    /// in `libraries`, and one per block type that came from it.
    fn held_here(&self, library: &Arc<Library>) -> usize {
        1 + self
            .types
            .values()
            .filter(|e| {
                e.library
                    .as_ref()
                    .is_some_and(|held| Arc::ptr_eq(held, library))
            })
            .count()
    }

    /// The libraries loaded, in path order.
    pub fn loaded(&self) -> impl Iterator<Item = &Path> {
        self.libraries.keys().map(PathBuf::as_path)
    }

    fn insert(
        &mut self,
        plugin: Plugin,
        library: Option<PathBuf>,
        handle: Option<Arc<Library>>,
    ) -> Result<()> {
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
        if let Some(dup) = plugin
            .blocks
            .iter()
            .find(|b| self.types.contains_key(&b.name))
        {
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
                plugin: plugin.name.to_string(),
                library: library.clone(),
            };
            self.types.insert(
                block.name.clone(),
                Entry {
                    name: block.name,
                    description: block.description.to_string(),
                    origin,
                    add: block.add,
                    library: handle.clone(),
                },
            );
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
        entry
            .add(fg, settings)
            .with_context(|| format!("block '{}' ({name})", settings.block()))
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        if self
            .libraries
            .values()
            .any(|library| Arc::strong_count(library) == self.held_here(library))
        {
            drop_pooled_buffers();
        }
    }
}

/// Buffers are kept between flowgraphs, and a buffer of a plugin's blocks is
/// a type that plugin instantiated: its vtable and its destructor are in the
/// library. Whatever closes a library drops what is kept first, while the
/// library is still mapped, or the next flowgraph to take one of those
/// buffers calls into an unmapped page.
fn drop_pooled_buffers() {
    let limit = futuresdr_plugin_rt::buffer::set_pool_limit(0);
    futuresdr_plugin_rt::buffer::set_pool_limit(limit);
}
