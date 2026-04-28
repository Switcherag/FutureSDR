//! ## [iceoryx2](https://github.com/eclipse-iceoryx/iceoryx2) Shared Memory Blocks
//!
//! iceoryx2 types are not `Send` and the API is synchronous. These blocks use
//! a dedicated background thread for iceoryx2 operations, communicating with
//! the async FutureSDR runtime via channels.

mod pub_sink;
pub use pub_sink::PubSink;
pub use pub_sink::PubSinkBuilder;

mod sub_source;
pub use sub_source::SubSource;
pub use sub_source::SubSourceBuilder;
