//! Host side of FutureSDR plugins.
//!
//! - [`Registry`]: loads plugin libraries and adds their blocks by type name.

// Make sure FutureSDR comes from the shared library plugins link against.
use futuresdr_plugin_rt as _;

mod registry;

pub use plugin_api::Added;
pub use plugin_api::Settings;
pub use registry::Entry;
pub use registry::Origin;
pub use registry::Registry;
