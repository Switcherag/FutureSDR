//! [`ResponsiveLocalScheduler`]: see the [module](super).

use std::future::Future;
use std::pin::pin;

use async_executor::LocalExecutor;
use futuresdr::futures::future::Either;
use futuresdr::futures::future::select;
use futuresdr::runtime::scheduler::LocalScheduler;
use futuresdr::runtime::scheduler::Task;

/// A local scheduler that polls the domain's event loop after each task poll.
pub struct ResponsiveLocalScheduler {
    executor: LocalExecutor<'static>,
}

impl ResponsiveLocalScheduler {
    /// A scheduler for the current local-domain thread.
    pub fn new() -> Self {
        Self {
            executor: LocalExecutor::new(),
        }
    }
}

impl Default for ResponsiveLocalScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalScheduler for ResponsiveLocalScheduler {
    fn spawn<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> Task<T> {
        self.executor.spawn(future)
    }

    async fn run<'a, T: 'a>(&'a self, future: impl Future<Output = T> + 'a) -> T {
        let mut future = pin!(future);
        loop {
            // `future` first, then one task; asleep when neither is ready.
            match select(future.as_mut(), pin!(self.executor.tick())).await {
                Either::Left((out, _)) => return out,
                Either::Right(((), _)) => {}
            }
        }
    }
}
