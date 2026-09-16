//! What a FutureSDR plugin exports, and what a host reads from it.
//!
//! A plugin is a Rust `dylib` exporting one function, [`ENTRY_SYMBOL`], that
//! returns a [`Plugin`]: a list of [`BlockType`]s. Each block type knows how
//! to add one block to a [`Flowgraph`], configured from [`Settings`]. The
//! typed code — which kernel, which item type — stays inside the plugin; the
//! host only deals with type names, settings and [`BlockId`]s.
//!
//! Generic blocks are exported once per item type of a fixed list, under
//! names like `Head<f32>`:
//!
//! ```ignore
//! extern crate futuresdr_plugin_rt as futuresdr;
//! use futuresdr::prelude::*;
//!
//! export_plugin! {
//!     name: "basic",
//!     blocks: [
//!         {
//!             name: "Head",
//!             types: [u8, f32, Complex32],
//!             description: "Forward the first `n_items` items, then finish.",
//!             add: |s| blocks::Head::<T>::new(s.get("n_items")?),
//!         },
//!         {
//!             name: "MessageCopy",
//!             add: |_s| blocks::MessageCopy::new(),
//!         },
//!     ]
//! }
//! ```
//!
//! Plugins and host must share one compiled copy of FutureSDR and of this
//! crate: they link against the `futuresdr-plugin-rt` shared library, and
//! plugins are compiled against that very build (see `futuresdr-plugin-sdk`).
//! Rust symbol names then match by construction; a plugin built against
//! another build fails to load instead of misbehaving.

use std::any::Any;

pub use anyhow;
pub use futuresdr;

use futuresdr::runtime::BlockId;
use futuresdr::runtime::BlockRef;
use futuresdr::runtime::Flowgraph;
use futuresdr::runtime::dev::SendKernel;

mod settings;
pub use settings::FromSetting;
pub use settings::Settings;

/// Name of the function every plugin exports, `fn() -> Plugin`.
pub const ENTRY_SYMBOL: &str = "futuresdr_plugin_entry";

/// Bumped whenever [`Plugin`] or [`BlockType`] change shape.
pub const ABI_VERSION: u32 = 1;

#[doc(hidden)]
pub static ABI_ANCHOR: u8 = 0;

/// Identifies the shared library a plugin was linked against.
///
/// `anchor` is the address of one static inside that library. Host and plugin
/// agree on it only when both use the same loaded copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abi {
    /// [`ABI_VERSION`] as compiled into the plugin.
    pub version: u32,
    anchor: usize,
}

impl Abi {
    /// The ABI of the code calling this.
    pub fn current() -> Self {
        Self {
            version: ABI_VERSION,
            anchor: &ABI_ANCHOR as *const u8 as usize,
        }
    }
}

/// A block a plugin added to a flowgraph.
pub struct Added {
    /// Id of the new block.
    pub id: BlockId,
    /// Its typed [`BlockRef`], for typed access after the flowgraph ran.
    /// Downcast it to `BlockRef<K>` for the concrete kernel `K`.
    pub block_ref: Box<dyn Any + Send>,
}

impl std::fmt::Debug for Added {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Added").field("id", &self.id).finish_non_exhaustive()
    }
}

/// Adds one configured block to a flowgraph.
pub type AddFn = fn(&mut Flowgraph, &Settings) -> anyhow::Result<Added>;

/// One block type a plugin provides.
#[derive(Clone)]
pub struct BlockType {
    /// Name used in flowgraph descriptions, e.g. `Head<f32>`.
    pub name: String,
    /// One-line description.
    pub description: &'static str,
    /// Adds a block of this type.
    pub add: AddFn,
}

/// Everything a plugin provides.
pub struct Plugin {
    /// Plugin name.
    pub name: &'static str,
    /// ABI the plugin was built for.
    pub abi: Abi,
    /// Block types, in declaration order.
    pub blocks: Vec<BlockType>,
}

impl Plugin {
    /// A plugin with the ABI of the calling code.
    pub fn new(name: &'static str, blocks: Vec<BlockType>) -> Self {
        Self {
            name,
            abi: Abi::current(),
            blocks,
        }
    }
}

/// Add `kernel` to `fg`. Used by [`export_plugin!`].
pub fn add_kernel<K: SendKernel + 'static>(fg: &mut Flowgraph, kernel: K) -> anyhow::Result<Added> {
    let block: BlockRef<K> = fg.add(kernel)?;
    Ok(Added {
        id: block.id(),
        block_ref: Box::new(block),
    })
}

#[doc(hidden)]
pub fn type_label(ty: &str) -> String {
    let last = ty.rsplit("::").next().unwrap_or(ty);
    last.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Export the entry function of a plugin.
///
/// Each block is `{ name, [types], [description], add }`:
///
/// - `types: [..]` exports the block once per listed type, as `name<type>`;
///   inside `add`, `T` is that type. `types: default` is
///   `[u8, i16, i32, f32, f64, Complex32]`.
/// - `add: |s| expr` builds the kernel from the block's [`Settings`] `s`; `?`
///   may be used on settings lookups and anything returning
///   [`anyhow::Result`].
#[macro_export]
macro_rules! export_plugin {
    (name: $plugin:literal, blocks: [ $( $block:tt ),* $(,)? ] $(,)?) => {
        #[unsafe(no_mangle)]
        pub fn futuresdr_plugin_entry() -> $crate::Plugin {
            let mut blocks = ::std::vec::Vec::new();
            $( $crate::__export_block!(blocks, $block); )*
            $crate::Plugin::new($plugin, blocks)
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __export_block {
    ($blocks:ident, {
        name: $name:literal,
        types: default,
        $( description: $desc:literal, )?
        add: |$s:ident| $body:expr $(,)?
    }) => {
        $crate::__export_block!($blocks, {
            name: $name,
            types: [u8, i16, i32, f32, f64, $crate::futuresdr::num_complex::Complex32],
            $( description: $desc, )?
            add: |$s| $body
        });
    };
    ($blocks:ident, {
        name: $name:literal,
        types: [ $( $ty:ty ),+ $(,)? ],
        $( description: $desc:literal, )?
        add: |$s:ident| $body:expr $(,)?
    }) => {
        let description: &'static str = $crate::__description!($($desc)?);
        $(
            $blocks.push($crate::BlockType {
                name: ::std::format!("{}<{}>", $name, $crate::type_label(::std::stringify!($ty))),
                description,
                add: |fg, settings| {
                    #[allow(dead_code)]
                    type T = $ty;
                    let $s: &$crate::Settings = settings;
                    let kernel = $body;
                    $crate::add_kernel(fg, kernel)
                },
            });
        )+
    };
    ($blocks:ident, {
        name: $name:literal,
        $( description: $desc:literal, )?
        add: |$s:ident| $body:expr $(,)?
    }) => {
        $blocks.push($crate::BlockType {
            name: ::std::string::String::from($name),
            description: $crate::__description!($($desc)?),
            add: |fg, settings| {
                let $s: &$crate::Settings = settings;
                let kernel = $body;
                $crate::add_kernel(fg, kernel)
            },
        });
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __description {
    () => {
        ""
    };
    ($desc:literal) => {
        $desc
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_labels_drop_paths_and_spaces() {
        assert_eq!(type_label("f32"), "f32");
        assert_eq!(type_label("num_complex :: Complex32"), "Complex32");
        assert_eq!(type_label("Vec < u8 >"), "Vec<u8>");
    }
}
