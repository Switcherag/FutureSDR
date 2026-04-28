use std::thread;

use futures::channel::mpsc;
use futures::StreamExt;

use crate::prelude::*;

/// Read samples from [iceoryx2](https://github.com/eclipse-iceoryx/iceoryx2) shared memory.
///
/// A background thread polls the iceoryx2 subscriber and forwards received data
/// through an async channel. The `work` method awaits on this channel, properly
/// yielding the async runtime when no data is available.
#[derive(Block)]
pub struct SubSource<T, O = DefaultCpuWriter<T>>
where
    T: Send + 'static,
    O: CpuBufferWriter<Item = T>,
{
    #[output]
    output: O,
    service_name: String,
    receiver: Option<mpsc::Receiver<Vec<u8>>>,
    _thread: Option<thread::JoinHandle<()>>,
}

impl<T, O> SubSource<T, O>
where
    T: Send + 'static,
    O: CpuBufferWriter<Item = T>,
{
    /// Create SubSource block
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            output: O::default(),
            service_name: service_name.into(),
            receiver: None,
            _thread: None,
        }
    }
}

#[doc(hidden)]
impl<T, O> Kernel for SubSource<T, O>
where
    T: Send + 'static,
    O: CpuBufferWriter<Item = T>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let rx = self.receiver.as_mut().unwrap();

        match rx.next().await {
            Some(data) => {
                let n = data.len() / std::mem::size_of::<T>();
                if n > 0 {
                    let o = self.output.slice();
                    let to_copy = n.min(o.len());
                    if to_copy > 0 {
                        let byte_count = to_copy * std::mem::size_of::<T>();
                        let dst = o.as_ptr() as *mut u8;
                        unsafe {
                            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, byte_count);
                        }
                        debug!("iceoryx2 SubSource received {} items", to_copy);
                        self.output.produce(to_copy);
                    }
                }
            }
            None => {
                debug!("iceoryx2 SubSource channel closed");
                io.finished = true;
            }
        }

        Ok(())
    }

    async fn init(&mut self, _mio: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        debug!("iceoryx2 SubSource Init");

        let (mut tx, rx) = mpsc::channel::<Vec<u8>>(64);
        let service_name = self.service_name.clone();

        let handle = thread::Builder::new()
            .name("iox2-sub".into())
            .spawn(move || {
                use iceoryx2::prelude::*;

                let node = NodeBuilder::new()
                    .create::<ipc::Service>()
                    .expect("iceoryx2: failed to create node");

                let service_name = ServiceName::new(&service_name)
                    .expect("iceoryx2: invalid service name");

                let service = node
                    .service_builder(&service_name)
                    .publish_subscribe::<[u8]>()
                    .open_or_create()
                    .expect("iceoryx2: failed to create publish-subscribe service");

                let subscriber = service
                    .subscriber_builder()
                    .create()
                    .expect("iceoryx2: failed to create subscriber");

                loop {
                    match subscriber.receive() {
                        Ok(Some(sample)) => {
                            let data = sample.payload().to_vec();
                            loop {
                                match tx.try_send(data.clone()) {
                                    Ok(()) => break,
                                    Err(e) if e.is_full() => {
                                        // Channel full — back-pressure, wait briefly
                                        std::thread::sleep(
                                            std::time::Duration::from_micros(100),
                                        );
                                    }
                                    Err(_) => return, // receiver dropped
                                }
                            }
                        }
                        Ok(None) => {
                            // No data — brief sleep to avoid busy spinning
                            std::thread::sleep(std::time::Duration::from_micros(50));
                        }
                        Err(e) => {
                            eprintln!("iceoryx2: receive error: {:?}", e);
                            break;
                        }
                    }
                }
            })
            .map_err(|e| anyhow::anyhow!("failed to spawn iceoryx2 thread: {}", e))?;

        info!(
            "iceoryx2 SubSource subscribing on {:?}",
            self.service_name
        );
        self.receiver = Some(rx);
        self._thread = Some(handle);

        Ok(())
    }
}

/// Build an iceoryx2 [SubSource].
pub struct SubSourceBuilder<T, O = DefaultCpuWriter<T>>
where
    T: Send + 'static,
    O: CpuBufferWriter<Item = T>,
{
    service_name: String,
    _type: std::marker::PhantomData<O>,
}

impl<T, O> SubSourceBuilder<T, O>
where
    T: Send + 'static,
    O: CpuBufferWriter<Item = T>,
{
    /// Create SubSource builder
    pub fn new() -> Self {
        SubSourceBuilder {
            service_name: "futuresdr/default".into(),
            _type: std::marker::PhantomData,
        }
    }

    /// Set the iceoryx2 service name
    #[must_use]
    pub fn service_name(mut self, name: &str) -> Self {
        self.service_name = name.to_string();
        self
    }

    /// Build iceoryx2 source
    pub fn build(self) -> SubSource<T, O> {
        SubSource::<T, O>::new(self.service_name)
    }
}

impl<T, O> Default for SubSourceBuilder<T, O>
where
    T: Send + 'static,
    O: CpuBufferWriter<Item = T>,
{
    fn default() -> Self {
        Self::new()
    }
}
