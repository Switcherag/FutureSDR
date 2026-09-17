//! Host side of FutureSDR plugins.
//!
//! - [`Registry`]: loads plugin libraries and adds their blocks by type name.
//! - [`Description`]: a flowgraph in TOML, connections in `connect!` syntax.
//! - [`build`]: turns a description into a [`Flowgraph`](futuresdr::runtime::Flowgraph).
//! - [`Controller`]: runs described flowgraphs, links their ports (one
//!   output to several inputs, streams and messages), replaces them while the
//!   rest keeps running, parks and selects links, sets the controls they
//!   ask for, and taps their messages.

// Make sure FutureSDR comes from the shared library plugins link against.
use futuresdr_plugin_rt as _;

mod bridge;
mod builder;
pub mod connect;
mod controller;
mod description;
mod items;
mod registry;
#[cfg(test)]
mod test_rng;
mod topic;

pub use bridge::ChannelStats;
pub use builder::Blocks;
pub use builder::Built;
pub use builder::build;
pub use controller::Controller;
pub use controller::Finished;
pub use controller::Hold;
pub use controller::ReplaceTimings;
pub use controller::Replacement;
pub use controller::Retired;
pub use controller::Standby;
pub use description::BlockDecl;
pub use description::Description;
pub use description::MessagePortDecl;
pub use description::PortDecl;
pub use description::to_pmt;
pub use items::ItemType;
pub use plugin_api::Added;
pub use plugin_api::Settings;
pub use registry::Entry;
pub use registry::Origin;
pub use registry::Registry;
pub use topic::Tap;
