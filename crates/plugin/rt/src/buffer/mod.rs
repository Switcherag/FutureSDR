//! Buffers for hosts that replace flowgraphs.
//!
//! FutureSDR's default buffer maps its ring twice into the address space, so
//! that a block always sees its items as one slice. That mapping is made when
//! a flowgraph is connected and undone when it is dropped, which a host that
//! replaces flowgraphs pays over and over. [`circular_reuse`] is that buffer
//! with the ring kept in a pool between flowgraphs:
//!
//! ```ignore
//! use futuresdr_plugin_rt::prelude::*;
//!
//! let src = fg.add(blocks::NullSource::<u8, ReuseCpuWriter<u8>>::new())?;
//! ```
//!
//! Every port of one connection must agree on the type, so a flowgraph uses
//! either these or the default ones throughout. Plugin blocks take their
//! buffer types as parameters for that reason, and the plugins this workspace
//! builds register them with these.
//!
//! The pool holds at most [`DEFAULT_POOL_LIMIT`] bytes; `set_pool_limit(0)`
//! empties it and stops keeping, which is what a program that builds its
//! flowgraph once wants.

pub mod circular_reuse;
mod pool;

pub use pool::DEFAULT_POOL_LIMIT;
pub use pool::pool_stats;
pub use pool::set_pool_limit;

/// A reader port on a buffer kept for the next flowgraph.
pub type ReuseCpuReader<D> = circular_reuse::Reader<D>;
/// A writer port on a buffer kept for the next flowgraph.
pub type ReuseCpuWriter<D> = circular_reuse::Writer<D>;
