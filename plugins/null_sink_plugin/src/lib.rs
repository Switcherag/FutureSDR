use futuresdr::blocks::NullSink;
use futuresdr::prelude::Complex32;
use futuresdr::runtime::{Block, BlockId, WrappedKernel};
use std::any::Any;

struct NullSinkFactory;

impl plugin_api::BlockFactory for NullSinkFactory {
    fn create_block(&self, id: BlockId, config: Box<dyn Any + Send>) -> Box<dyn Block> {
        // Try String config for type selection; default to u8 if () is passed
        if let Ok(type_name) = config.downcast::<String>() {
            match type_name.as_str() {
                "c32" | "complex32" | "Complex32" => {
                    Box::new(WrappedKernel::new(NullSink::<Complex32>::new(), id))
                }
                "f32" => {
                    Box::new(WrappedKernel::new(NullSink::<f32>::new(), id))
                }
                _ => {
                    Box::new(WrappedKernel::new(NullSink::<u8>::new(), id))
                }
            }
        } else {
            // () or any other config → default u8
            Box::new(WrappedKernel::new(NullSink::<u8>::new(), id))
        }
    }

    fn block_name(&self) -> &'static str { "NullSink" }
    fn block_description(&self) -> &'static str { "Generic NullSink — config: String type (\"u8\", \"c32\", \"f32\") or () for u8" }
}

#[unsafe(no_mangle)]
pub fn create_block_factory() -> Box<dyn plugin_api::BlockFactory> {
    Box::new(NullSinkFactory)
}

#[unsafe(no_mangle)]
pub fn plugin_abi_fingerprint() -> (&'static str, &'static str) {
    (
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT,
        futuresdr::runtime::abi_fingerprint::ABI_FINGERPRINT_DETAIL,
    )
}
