//! Schedulers for local domains whose blocks are always ready.
//!
//! A local domain runs its blocks on one thread, with FutureSDR's
//! [`BasicLocalScheduler`](futuresdr::runtime::scheduler::BasicLocalScheduler):
//! an executor for the block tasks, driving the domain's event loop, which
//! takes what other threads send the domain's blocks (a `call` or `post` to
//! a message input). That executor polls the event loop only when no task is
//! ready, or after 200 task polls. A block that is always ready, as a radio
//! source reading its device in `work()` is, has its messages wait 200 of its
//! polls.
//!
//! [`ResponsiveLocalScheduler`] polls the event loop between every two task
//! polls instead, so a message waits one poll at most:
//!
//! ```ignore
//! use futuresdr_plugin_rt::scheduler::ResponsiveLocalScheduler;
//!
//! let domain = fg.local_domain_with_scheduler::<ResponsiveLocalScheduler>()?;
//! let src = fg.with_local_domain(domain, |ctx| Ok(ctx.add(source)))?;
//! ```
//!
//! A block's task is polled until it waits, and a block asked to be called
//! again is called within the same poll: one that never waits on anything,
//! reading its device synchronously, has to yield in `work()` for any
//! scheduler to take its messages at all.
//!
//! Better, where it can, the block waits: a thread of its own reads the
//! device and wakes it, and the domain's executor, with nothing ready, takes
//! the messages at once (the bladeRF source of the `real_device_swap`
//! example).

mod responsive;

pub use responsive::ResponsiveLocalScheduler;
