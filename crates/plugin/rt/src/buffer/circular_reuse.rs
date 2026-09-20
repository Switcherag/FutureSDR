//! FutureSDR's circular buffer, with the buffer kept for the next flowgraph.
//!
//! This is `futuresdr::runtime::buffer::circular` (Apache-2.0, FutureSDR)
//! with one change: the ring of a connection whose flowgraph is gone goes to
//! a pool instead of being unmapped, and a connection that
//! asks for the same size takes it from there. A host that replaces
//! flowgraphs pays neither the mapping nor the unmapping (see [`set_pool_limit`](super::set_pool_limit)).
//!
//! The ring is kept only once neither end can reach it: the writer hands it
//! over when its port is dropped, and the last reader to go puts it away.

use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use futuresdr::runtime::BlockId;
use futuresdr::runtime::Error;
use futuresdr::runtime::PortIndex;
use futuresdr::runtime::buffer::BufferReader;
use futuresdr::runtime::buffer::BufferWriter;
use futuresdr::runtime::buffer::CpuBufferReader;
use futuresdr::runtime::buffer::CpuBufferWriter;
use futuresdr::runtime::buffer::CpuSample;
use futuresdr::runtime::buffer::Tags;
use futuresdr::runtime::buffer::ThreadSafeConnect;
use futuresdr::runtime::buffer::dev::BlockInbox;
use futuresdr::runtime::buffer::dev::BufferInbox;
use futuresdr::runtime::buffer::dev::BufferNotifier;
use futuresdr::runtime::buffer::dev::BufferRequirements;
use futuresdr::runtime::buffer::dev::ConnectionState;
use futuresdr::runtime::buffer::dev::PortCore;
use futuresdr::runtime::buffer::dev::PortEndpoint;
use futuresdr::runtime::config::config;
use futuresdr::runtime::dev::ItemTag;
use futuresdr::tracing::warn;
use vmcircbuffer::generic;

use super::pool;

struct MyNotifier<N: BufferNotifier> {
    notifier: N,
}

impl<N: BufferNotifier> generic::Notifier for MyNotifier<N> {
    // we never arm the notifier
    fn arm(&mut self) {}

    // we notify blocks for every change to the buffer
    fn notify(&mut self) {
        self.notifier.notify();
    }
}

struct MyMetadata {
    tags: Vec<ItemTag>,
}

impl generic::Metadata for MyMetadata {
    type Item = ItemTag;

    fn new() -> Self {
        MyMetadata { tags: Vec::new() }
    }
    fn add_from_slice(&mut self, offset: usize, tags: &[Self::Item]) {
        for t in tags {
            let mut t = t.clone();
            t.index += offset;
            self.tags.push(t);
        }
    }
    fn get_into(&self, out: &mut Vec<Self::Item>) {
        out.clear();
        out.extend(self.tags.iter().cloned());
    }
    fn consume(&mut self, items: usize) {
        self.tags.retain(|x| x.index >= items);
        for t in self.tags.iter_mut() {
            t.index -= items;
        }
    }
}

/// The ring under a connection: the writer of the underlying buffer, which
/// outlives the flowgraph that used it.
type Ring<D, I> = generic::Writer<D, MyNotifier<<I as BufferInbox>::Notifier>, MyMetadata>;
/// One end reading that ring.
type RingReader<D, I> = generic::Reader<D, MyNotifier<<I as BufferInbox>::Notifier>, MyMetadata>;

/// What the two ends of a connection share, so that the ring is kept once
/// neither of them can reach it.
struct Shared<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    /// The ring, once the writer's port is gone.
    ring: Mutex<Option<Ring<D, I>>>,
    /// Readers that can still take items from it.
    readers: AtomicUsize,
    capacity: usize,
    bytes: usize,
}

impl<D, I> Shared<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            ring: Mutex::new(None),
            readers: AtomicUsize::new(0),
            capacity,
            bytes: capacity * D::SIZE.get(),
        })
    }

    fn reader_added(&self) {
        self.readers.fetch_add(1, Ordering::AcqRel);
    }

    /// The writer's port is gone, with the ring it held.
    fn writer_gone(&self, ring: Ring<D, I>) {
        let mut kept = self.lock();
        *kept = Some(ring);
        self.keep(kept);
    }

    /// A reader's port is gone.
    fn reader_gone(&self) {
        if self.readers.fetch_sub(1, Ordering::AcqRel) == 1 {
            let kept = self.lock();
            self.keep(kept);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Ring<D, I>>> {
        self.ring.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Put the ring away, once the writer handed it over and every reader is
    /// gone. Both ends take this lock, so only one of them finds both.
    fn keep(&self, mut kept: MutexGuard<'_, Option<Ring<D, I>>>) {
        if self.readers.load(Ordering::Acquire) > 0 {
            return;
        }
        if let Some(ring) = kept.take() {
            pool::keep(self.capacity, self.bytes, ring);
        }
    }
}

/// Circular writer whose buffer is kept for the next flowgraph.
pub struct Writer<D, I = BlockInbox>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    core: PortCore<I>,
    state: ConnectionState<ConnectedWriter<D, I>>,
    tags: Vec<ItemTag>,
}

struct ConnectedWriter<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    ring: Option<Ring<D, I>>,
    readers: Vec<PortEndpoint<I>>,
    shared: Arc<Shared<D, I>>,
}

impl<D, I> ConnectedWriter<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    /// A connection of `capacity` items, on a kept ring if there is one.
    fn with_capacity(capacity: usize) -> Self {
        let ring = pool::take::<Ring<D, I>>(capacity)
            .unwrap_or_else(|| generic::Circular::with_capacity(capacity).unwrap());
        Self {
            ring: Some(ring),
            readers: Vec::new(),
            shared: Shared::new(capacity),
        }
    }

    fn ring(&mut self) -> &mut Ring<D, I> {
        self.ring
            .as_mut()
            .expect("a connected writer holds its ring")
    }
}

impl<D, I> Drop for ConnectedWriter<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    fn drop(&mut self) {
        if let Some(ring) = self.ring.take() {
            self.shared.writer_gone(ring);
        }
    }
}

/// Reader offer for a cross-domain connection.
#[doc(hidden)]
pub struct ThreadSafeConnectToken<D>
where
    D: CpuSample,
{
    reader: PortEndpoint<BlockInbox>,
    reader_min_items: Option<usize>,
    reader_min_buffer_size: Option<usize>,
    _item: std::marker::PhantomData<D>,
}

/// Reader installation returned by the writer.
#[doc(hidden)]
pub struct ThreadSafeReturnToken<D>
where
    D: CpuSample,
{
    connected: ConnectedReader<D, BlockInbox>,
    min_buffer_size: usize,
}

impl<D, I> Writer<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    fn new() -> Self {
        Self {
            core: PortCore::new_unbound(),
            state: ConnectionState::disconnected(),
            tags: vec![],
        }
    }
}

impl<D, I> Default for Writer<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<D, I> BufferWriter for Writer<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    type Inbox = I;
    type Reader = Reader<D, I>;

    fn init(&mut self, block_id: BlockId, port_id: PortIndex, inbox: I) {
        self.core.init(block_id, port_id, inbox);
    }

    fn max_readers(&self) -> usize {
        usize::MAX
    }

    fn buffer_requirements(&self) -> BufferRequirements {
        self.core.requirements()
    }
    fn raise_buffer_requirements(&mut self, requirements: BufferRequirements) {
        self.core.raise_requirements(requirements);
    }
    fn validate(&self) -> Result<(), Error> {
        if self.state.is_connected() {
            Ok(())
        } else {
            Err(self.core.not_connected_error())
        }
    }
    fn connect(&mut self, dest: &mut Self::Reader) {
        let mut connected = if let Some(connected) = self.state.take_connected() {
            if self.core.min_buffer_size_in_items().unwrap_or(0)
                < dest.core.min_buffer_size_in_items().unwrap_or(0)
            {
                warn!("buffer is already created, size constraints of reader are not considered.");
                warn!(
                    "buffer size is {:?}, reader requirement {:?}",
                    self.core.min_buffer_size_in_items(),
                    dest.core.min_buffer_size_in_items()
                );
            }
            if self.core.min_buffer_size_in_items().unwrap_or(0)
                - self.core.min_items().unwrap_or(0)
                + 1
                < dest.core.min_items().unwrap_or(1)
            {
                warn!("buffer is already created, size constraints of reader are not considered.");
                warn!(
                    "buffer size is {:?}, writer min items {:?}",
                    self.core.min_buffer_size_in_items(),
                    self.core.min_items()
                );
            }
            connected
        } else {
            let page_size = vmcircbuffer::double_mapped_buffer::pagesize();
            let mut buffer_size = page_size;

            // Items required for work() to proceed
            let min_self = self.core.min_items().unwrap_or(1);
            let min_reader = dest.core.min_items().unwrap_or(1);
            let mut min_bytes = (min_self + min_reader - 1) * D::SIZE.get();

            let buffer_size_configured = self.core.min_buffer_size_in_items().is_some()
                || dest.core.min_buffer_size_in_items().is_some();

            min_bytes = if buffer_size_configured {
                let min_self = self.core.min_buffer_size_in_items().unwrap_or(0);
                let min_reader = dest.core.min_buffer_size_in_items().unwrap_or(0);
                std::cmp::max(
                    min_bytes,
                    std::cmp::max(min_self, min_reader) * D::SIZE.get(),
                )
            } else {
                std::cmp::max(min_bytes, config().buffer_size)
            };

            while (buffer_size < min_bytes) || !buffer_size.is_multiple_of(D::SIZE.get()) {
                buffer_size += page_size;
            }

            self.core
                .set_min_buffer_size_in_items(buffer_size / D::SIZE);
            dest.core
                .set_min_buffer_size_in_items(buffer_size / D::SIZE);

            ConnectedWriter::with_capacity(buffer_size / D::SIZE)
        };

        let writer_notifier = MyNotifier {
            notifier: self.core.notifier(),
        };

        let reader_notifier = MyNotifier {
            notifier: dest.core.notifier(),
        };

        let reader = connected
            .ring()
            .add_reader(reader_notifier, writer_notifier);

        connected.readers.push(PortEndpoint::new(
            dest.core.inbox().clone(),
            dest.core.port_id(),
        ));
        let shared = connected.shared.clone();
        self.state.set_connected(connected);

        dest.state.set_connected(ConnectedReader::new(
            reader,
            PortEndpoint::new(self.core.inbox().clone(), self.core.port_id()),
            shared,
        ));
    }
    async fn notify_finished(&mut self) {
        for i in &self.state.connected().readers {
            let _ = i.inbox().stream_input_done(i.port_id()).await;
        }
    }
    fn block_id(&self) -> BlockId {
        self.core.block_id()
    }
    fn port_id(&self) -> PortIndex {
        self.core.port_id()
    }
}

impl<D> ThreadSafeConnect for Writer<D, BlockInbox>
where
    D: CpuSample,
{
    type ReaderToken = ThreadSafeConnectToken<D>;
    type WriterToken = ThreadSafeReturnToken<D>;

    fn take_reader_token(reader: &mut Reader<D, BlockInbox>) -> Self::ReaderToken {
        ThreadSafeConnectToken {
            reader: PortEndpoint::new(reader.core.inbox().clone(), reader.core.port_id()),
            reader_min_items: reader.core.min_items(),
            reader_min_buffer_size: reader.core.min_buffer_size_in_items(),
            _item: std::marker::PhantomData,
        }
    }

    fn connect_reader(&mut self, token: Self::ReaderToken) -> Self::WriterToken {
        let min_buffer_size = if self.state.is_connected() {
            if self.core.min_buffer_size_in_items().unwrap_or(0)
                < token.reader_min_buffer_size.unwrap_or(0)
            {
                warn!("buffer is already created, size constraints of reader are not considered.");
            }
            if self.core.min_buffer_size_in_items().unwrap_or(0)
                - self.core.min_items().unwrap_or(0)
                + 1
                < token.reader_min_items.unwrap_or(1)
            {
                warn!("buffer is already created, size constraints of reader are not considered.");
            }
            self.core.min_buffer_size_in_items().unwrap_or(0)
        } else {
            let page_size = vmcircbuffer::double_mapped_buffer::pagesize();
            let mut buffer_size = page_size;
            let min_self = self.core.min_items().unwrap_or(1);
            let min_reader = token.reader_min_items.unwrap_or(1);
            let mut min_bytes = (min_self + min_reader - 1) * D::SIZE.get();
            let buffer_size_configured = self.core.min_buffer_size_in_items().is_some()
                || token.reader_min_buffer_size.is_some();

            min_bytes = if buffer_size_configured {
                let min_self = self.core.min_buffer_size_in_items().unwrap_or(0);
                let min_reader = token.reader_min_buffer_size.unwrap_or(0);
                std::cmp::max(
                    min_bytes,
                    std::cmp::max(min_self, min_reader) * D::SIZE.get(),
                )
            } else {
                std::cmp::max(min_bytes, config().buffer_size)
            };

            while (buffer_size < min_bytes) || !buffer_size.is_multiple_of(D::SIZE.get()) {
                buffer_size += page_size;
            }

            self.core
                .set_min_buffer_size_in_items(buffer_size / D::SIZE);
            self.state
                .set_connected(ConnectedWriter::with_capacity(buffer_size / D::SIZE));
            buffer_size / D::SIZE
        };

        let writer_notifier = MyNotifier {
            notifier: self.core.notifier(),
        };
        let reader_notifier = MyNotifier {
            notifier: BlockInbox::notifier(token.reader.inbox()),
        };
        let connected = self.state.connected_mut();
        let reader = connected
            .ring()
            .add_reader(reader_notifier, writer_notifier);
        connected.readers.push(token.reader);
        let shared = connected.shared.clone();

        ThreadSafeReturnToken {
            connected: ConnectedReader::new(
                reader,
                PortEndpoint::new(self.core.inbox().clone(), self.core.port_id()),
                shared,
            ),
            min_buffer_size,
        }
    }

    fn finish_reader(reader: &mut Reader<D, BlockInbox>, token: Self::WriterToken) {
        reader
            .core
            .set_min_buffer_size_in_items(token.min_buffer_size);
        reader.state.set_connected(token.connected);
    }
}

impl<D, I> CpuBufferWriter for Writer<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    type Item = D;

    fn slice(&mut self) -> &mut [Self::Item] {
        self.state.connected_mut().ring().slice(false)
    }

    fn produce(&mut self, items: usize) {
        self.state.connected_mut().ring().produce(items, &self.tags);
        self.tags.clear();
    }
    fn slice_with_tags(&mut self) -> (&mut [Self::Item], Tags<'_>) {
        let s = self.state.connected_mut().ring().slice(false);
        (s, Tags::new(&mut self.tags, 0))
    }
}

impl<D, I> fmt::Debug for Writer<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("circular_reuse::Writer")
            .field("output_id", &self.core.port_id_if_bound())
            .finish()
    }
}

/// Circular reader of a buffer that is kept for the next flowgraph.
pub struct Reader<D, I = BlockInbox>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    state: ConnectionState<ConnectedReader<D, I>>,
    finished: bool,
    core: PortCore<I>,
    tags: Vec<ItemTag>,
}

struct ConnectedReader<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    ring: Option<RingReader<D, I>>,
    writer: PortEndpoint<I>,
    shared: Arc<Shared<D, I>>,
}

impl<D, I> ConnectedReader<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    fn new(ring: RingReader<D, I>, writer: PortEndpoint<I>, shared: Arc<Shared<D, I>>) -> Self {
        shared.reader_added();
        Self {
            ring: Some(ring),
            writer,
            shared,
        }
    }

    fn ring(&mut self) -> &mut RingReader<D, I> {
        self.ring
            .as_mut()
            .expect("a connected reader holds its end")
    }
}

impl<D, I> Drop for ConnectedReader<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    fn drop(&mut self) {
        // Dropping the end takes it out of the ring's readers, so that what
        // is kept has nobody left to read it.
        drop(self.ring.take());
        self.shared.reader_gone();
    }
}

impl<D, I> Default for Reader<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    fn default() -> Self {
        Self {
            state: ConnectionState::disconnected(),
            finished: false,
            core: PortCore::new_unbound(),
            tags: vec![],
        }
    }
}

impl<D, I> BufferReader for Reader<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    type Inbox = I;
    fn init(&mut self, block_id: BlockId, port_id: PortIndex, inbox: I) {
        self.core.init(block_id, port_id, inbox);
    }
    fn buffer_requirements(&self) -> BufferRequirements {
        self.core.requirements()
    }
    fn raise_buffer_requirements(&mut self, requirements: BufferRequirements) {
        self.core.raise_requirements(requirements);
    }
    fn validate(&self) -> Result<(), Error> {
        if self.state.is_connected() {
            Ok(())
        } else {
            Err(self.core.not_connected_error())
        }
    }
    async fn notify_finished(&mut self) {
        let writer = &self.state.connected().writer;
        let _ = writer.inbox().stream_output_done(writer.port_id()).await;
    }
    fn finish(&mut self) {
        self.finished = true;
    }
    fn finished(&self) -> bool {
        self.finished
    }
    fn block_id(&self) -> BlockId {
        self.core.block_id()
    }
    fn port_id(&self) -> PortIndex {
        self.core.port_id()
    }
}

impl<D, I> CpuBufferReader for Reader<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    type Item = D;

    fn slice(&mut self) -> &[Self::Item] {
        self.state
            .connected_mut()
            .ring()
            .slice(false)
            .unwrap_or(&[])
    }

    fn slice_with_tags(&mut self) -> (&[Self::Item], &[ItemTag]) {
        match self
            .state
            .connected_mut()
            .ring()
            .slice_with_metadata_into(false, &mut self.tags)
        {
            Some(s) => (s, &self.tags),
            _ => {
                debug_assert!(self.tags.is_empty());
                (&[], &self.tags)
            }
        }
    }
    fn consume(&mut self, amount: usize) {
        self.state.connected_mut().ring().consume(amount);
    }

    fn max_contiguous_items(&self) -> usize {
        self.core
            .min_buffer_size_in_items()
            .expect("circular buffer capacity missing after validation")
    }
}

impl<D, I> fmt::Debug for Reader<D, I>
where
    D: CpuSample,
    I: BufferInbox,
    I::Notifier: Send,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("circular_reuse::Reader")
            .field(
                "writer_output_id",
                &self.state.as_ref().map(|state| state.writer.port_id()),
            )
            .field("finished", &self.finished)
            .finish()
    }
}
