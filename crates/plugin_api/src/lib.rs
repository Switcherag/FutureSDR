//! Minimal plugin-side ABI for FutureSDR dynamic plugins.
//!
//! Every plugin depends ONLY on this crate. It is deliberately tiny: a Rust
//! `dylib` re-exports (and therefore embeds) the full public API of every rlib
//! linked into it, so anything that lives here ends up baked into every plugin
//! `.so`. The host-side machinery — the dlopen loader, flowgraph controller,
//! radio controller, TOML config parsing — lives in the separate `plugin_host`
//! crate so plugins never carry it.

use std::any::Any;
use futuresdr::runtime::{Block, BlockId};

pub trait BlockFactory: Send + Sync {
    fn create_block(&self, id: BlockId, config: Box<dyn Any + Send>) -> Box<dyn Block>;
    fn block_name(&self) -> &'static str;
    fn block_description(&self) -> &'static str;
}

pub type CreateBlockFactoryFn = fn() -> Box<dyn BlockFactory>;
pub type AbiFingerPrintFn = fn() -> (&'static str, &'static str);

/// Generates the factory struct and `create_block_factory` export for a plugin.
///
/// Usage:
/// ```ignore
/// export_plugin! {
///     name: "Head",
///     description: "Copies N samples then stops",
///     config: u64,
///     create: |n_items, id| {
///         Head::<u8>::new(n_items)
///     }
/// }
/// ```
///
/// The closure receives the downcasted config and the BlockId.
/// It must return the kernel (the macro wraps it in WrappedKernel + Box).
///
/// The macro also exports a `plugin_abi_fingerprint` symbol used by the host
/// loader to validate binary compatibility at load time.
#[macro_export]
macro_rules! export_plugin {
    (
        name: $name:expr,
        description: $desc:expr,
        config: $config_ty:ty,
        create: |$config:ident, $id:ident| $body:expr
    ) => {
        struct __PluginFactory;

        impl $crate::BlockFactory for __PluginFactory {
            fn create_block(
                &self,
                $id: ::futuresdr::runtime::BlockId,
                config: Box<dyn ::std::any::Any + Send>,
            ) -> Box<dyn ::futuresdr::runtime::Block> {
                let $config = *config
                    .downcast::<$config_ty>()
                    .unwrap_or_else(|_| panic!(
                        "{} expects Box<{}>",
                        $name,
                        stringify!($config_ty)
                    ));
                let kernel = $body;
                Box::new(::futuresdr::runtime::WrappedKernel::new(kernel, $id))
            }

            fn block_name(&self) -> &'static str { $name }
            fn block_description(&self) -> &'static str { $desc }
        }

        #[unsafe(no_mangle)]
        pub fn create_block_factory() -> Box<dyn $crate::BlockFactory> {
            Box::new(__PluginFactory)
        }

        /// ABI fingerprint captured at plugin compile time.
        /// Returns (fingerprint_hash, detail_string).
        #[unsafe(no_mangle)]
        pub fn plugin_abi_fingerprint() -> (&'static str, &'static str) {
            (
                ::futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT,
                ::futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT_DETAIL,
            )
        }
    };
}
