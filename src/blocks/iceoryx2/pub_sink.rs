use std::sync::mpsc;
use std::thread;

use crate::prelude::*;

/// Push samples into [iceoryx2](https://github.com/eclipse-iceoryx/iceoryx2) shared memory.
///
/// A background thread handles all iceoryx2 operations. Data is sent to the
/// thread via a channel.
#[derive(Block)]
pub struct PubSink<T, I = DefaultCpuReader<T>>
where
    T: Send + 'static,
    I: CpuBufferReader<Item = T>,
{
    #[input]
    input: I,
    service_name: String,
    max_slice_len: usize,
    sender: Option<mpsc::SyncSender<Vec<u8>>>,
    _thread: Option<thread::JoinHandle<()>>,
}

impl<T, I> PubSink<T, I>
where
    T: Send + 'static,
    I: CpuBufferReader<Item = T>,
{
    /// Create PubSink
    pub fn new(service_name: impl Into<String>, max_slice_len: usize) -> Self {
        Self {
            input: I::default(),
            service_name: service_name.into(),
            max_slice_len,
            sender: None,
            _thread: None,
        }
    }
}

#[doc(hidden)]
impl<T, I> Kernel for PubSink<T, I>
where
    T: Send + 'static,
    I: CpuBufferReader<Item = T>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let n = i.len();

        if n > 0 {
            let ptr = i.as_ptr() as *const u8;
            let byte_len = std::mem::size_of_val(i);
            let send_len = byte_len.min(self.max_slice_len);
            let data = unsafe { std::slice::from_raw_parts(ptr, send_len) };

            self.sender
                .as_ref()
                .unwrap()
                .send(data.to_vec())
                .map_err(|e| anyhow::anyhow!("iceoryx2 channel send failed: {}", e))?;

            let items_sent = send_len / std::mem::size_of::<T>();
            self.input.consume(items_sent);
        }

        if self.input.finished() {
            io.finished = true;
        }

        Ok(())
    }

    async fn init(&mut self, _mio: &mut MessageOutputs, _meta: &mut BlockMeta) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(64);
        let service_name = self.service_name.clone();
        let max_slice_len = self.max_slice_len;

        let handle = thread::Builder::new()
            .name("iox2-pub".into())
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

                let publisher = service
                    .publisher_builder()
                    .initial_max_slice_len(max_slice_len)
                    .create()
                    .expect("iceoryx2: failed to create publisher");

                while let Ok(data) = rx.recv() {
                    let sample = publisher
                        .loan_slice_uninit(data.len())
                        .expect("iceoryx2: failed to loan slice");
                    let sample = sample.write_from_slice(&data);
                    sample.send().expect("iceoryx2: failed to send sample");
                }
            })
            .map_err(|e| anyhow::anyhow!("failed to spawn iceoryx2 thread: {}", e))?;

        info!("iceoryx2 PubSink publishing on {:?}", self.service_name);
        self.sender = Some(tx);
        self._thread = Some(handle);

        Ok(())
    }
}

/// Build an iceoryx2 [PubSink].
pub struct PubSinkBuilder<T, I = DefaultCpuReader<T>>
where
    T: Send + 'static,
    I: CpuBufferReader<Item = T>,
{
    service_name: String,
    max_slice_len: usize,
    _type: std::marker::PhantomData<I>,
}

impl<T, I> PubSinkBuilder<T, I>
where
    T: Send + 'static,
    I: CpuBufferReader<Item = T>,
{
    /// Create PubSink builder
    pub fn new() -> Self {
        PubSinkBuilder {
            service_name: "futuresdr/default".into(),
            max_slice_len: 1_048_576,
            _type: std::marker::PhantomData,
        }
    }

    /// Set the iceoryx2 service name
    #[must_use]
    pub fn service_name(mut self, name: &str) -> Self {
        self.service_name = name.to_string();
        self
    }

    /// Set the maximum slice length in bytes for shared memory loans
    #[must_use]
    pub fn max_slice_len(mut self, len: usize) -> Self {
        self.max_slice_len = len;
        self
    }

    /// Build PubSink
    pub fn build(self) -> PubSink<T, I> {
        PubSink::<T, I>::new(self.service_name, self.max_slice_len)
    }
}

impl<T, I> Default for PubSinkBuilder<T, I>
where
    T: Send + 'static,
    I: CpuBufferReader<Item = T>,
{
    fn default() -> Self {
        Self::new()
    }
}
