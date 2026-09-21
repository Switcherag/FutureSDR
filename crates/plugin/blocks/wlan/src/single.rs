//! The receiver as one block: `WlanSync > WlanSyncLong > WlanEqualizer >
//! WlanDecoder`, the same four kernels, run one after the other inside one
//! block on buffers of their own instead of the runtime's. Replacing it is
//! replacing one block; it decodes what the four blocks decode.

use std::fmt::Debug;

use futuresdr::prelude::*;
use futuresdr::runtime::buffer::BufferReader;
use futuresdr::runtime::buffer::BufferWriter;
use futuresdr::runtime::buffer::Tags;
use futuresdr::runtime::buffer::dev::BlockInbox;
use futuresdr::runtime::buffer::dev::BufferRequirements;

use crate::Decoder;
use crate::FrameEqualizer;
use crate::Standard;
use crate::SyncLong;
use crate::SyncShort;

/// Items a pipe's writer can take at once.
const CAPACITY: usize = 1 << 16;

/// The reading end of a pipe between two stages: what the stage before
/// wrote and this one has not consumed yet.
#[derive(Debug)]
pub struct PipeReader<T: Debug + Send + 'static> {
    data: Vec<T>,
    start: usize,
    /// Tags, indexed from `start`.
    tags: Vec<ItemTag>,
    finished: bool,
    block_id: BlockId,
    port_id: PortIndex,
    requirements: BufferRequirements,
}

impl<T: Debug + Send + Clone + 'static> PipeReader<T> {
    /// Append `items` and their `tags` (indexed in `items`).
    fn push(&mut self, items: &[T], tags: &[ItemTag]) {
        if self.start > 0 && self.start >= self.data.len() / 2 {
            self.data.drain(..self.start);
            self.start = 0;
        }
        let base = self.data.len() - self.start;
        self.data.extend_from_slice(items);
        self.tags.extend(tags.iter().map(|t| ItemTag {
            index: base + t.index,
            tag: t.tag.clone(),
        }));
    }

    fn len(&self) -> usize {
        self.data.len() - self.start
    }
}

impl<T: Debug + Send + 'static> Default for PipeReader<T> {
    fn default() -> Self {
        Self {
            data: Vec::new(),
            start: 0,
            tags: Vec::new(),
            finished: false,
            block_id: BlockId(0),
            port_id: PortIndex::new(0),
            requirements: BufferRequirements::new(),
        }
    }
}

impl<T: Debug + Send + 'static> BufferReader for PipeReader<T> {
    type Inbox = BlockInbox;

    fn buffer_requirements(&self) -> BufferRequirements {
        self.requirements
    }
    fn raise_buffer_requirements(&mut self, requirements: BufferRequirements) {
        self.requirements.merge(requirements);
    }
    fn init(&mut self, block_id: BlockId, port_id: PortIndex, _inbox: BlockInbox) {
        self.block_id = block_id;
        self.port_id = port_id;
    }
    fn validate(&self) -> Result<(), Error> {
        Ok(())
    }
    async fn notify_finished(&mut self) {}
    fn finish(&mut self) {
        self.finished = true;
    }
    fn finished(&self) -> bool {
        self.finished
    }
    fn block_id(&self) -> BlockId {
        self.block_id
    }
    fn port_id(&self) -> PortIndex {
        self.port_id
    }
}

impl<T: CpuSample> CpuBufferReader for PipeReader<T> {
    type Item = T;

    fn slice_with_tags(&mut self) -> (&[T], &[ItemTag]) {
        (&self.data[self.start..], &self.tags)
    }
    fn consume(&mut self, n: usize) {
        self.start += n;
        self.tags.retain(|t| t.index >= n);
        for t in &mut self.tags {
            t.index -= n;
        }
    }
    fn max_contiguous_items(&self) -> usize {
        self.data.len() - self.start
    }
}

/// The writing end of a pipe: what a stage produced, until moved to the
/// next stage's [`PipeReader`].
#[derive(Debug)]
pub struct PipeWriter<T: Clone + Debug + Send + 'static> {
    data: Vec<T>,
    produced: usize,
    tags: Vec<ItemTag>,
    block_id: BlockId,
    port_id: PortIndex,
    requirements: BufferRequirements,
}

impl<T: Clone + Debug + Send + Default + 'static> PipeWriter<T> {
    /// Move what was produced into `reader`; returns how many items.
    fn drain_into(&mut self, reader: &mut PipeReader<T>) -> usize {
        let n = self.produced;
        if n > 0 {
            reader.push(&self.data[..n], &self.tags);
            self.produced = 0;
            self.tags.clear();
        }
        n
    }
}

impl<T: Clone + Debug + Send + Default + 'static> Default for PipeWriter<T> {
    fn default() -> Self {
        Self {
            data: vec![T::default(); CAPACITY],
            produced: 0,
            tags: Vec::new(),
            block_id: BlockId(0),
            port_id: PortIndex::new(0),
            requirements: BufferRequirements::new(),
        }
    }
}

impl<T: Clone + Debug + Send + Default + 'static> BufferWriter for PipeWriter<T> {
    type Inbox = BlockInbox;
    type Reader = PipeReader<T>;

    fn buffer_requirements(&self) -> BufferRequirements {
        self.requirements
    }
    fn raise_buffer_requirements(&mut self, requirements: BufferRequirements) {
        self.requirements.merge(requirements);
    }
    fn init(&mut self, block_id: BlockId, port_id: PortIndex, _inbox: BlockInbox) {
        self.block_id = block_id;
        self.port_id = port_id;
    }
    fn validate(&self) -> Result<(), Error> {
        Ok(())
    }
    fn connect(&mut self, _dest: &mut Self::Reader) {}
    async fn notify_finished(&mut self) {}
    fn block_id(&self) -> BlockId {
        self.block_id
    }
    fn port_id(&self) -> PortIndex {
        self.port_id
    }
}

impl<T: CpuSample + Default> CpuBufferWriter for PipeWriter<T> {
    type Item = T;

    fn slice_with_tags(&mut self) -> (&mut [T], Tags<'_>) {
        (
            &mut self.data[self.produced..],
            Tags::new(&mut self.tags, self.produced),
        )
    }
    fn produce(&mut self, n: usize) {
        self.produced += n;
    }
}

type C32 = Complex32;

/// The whole receiver in one block: samples in, frames posted on
/// `rx_frames` and `rftap` (the equalizer's `symbols` and `channel_est`
/// too).
#[derive(Block)]
#[message_outputs(rx_frames, rftap, symbols, channel_est)]
pub struct Receiver<S: Standard> {
    #[input]
    input: DefaultCpuReader<C32>,
    sync: SyncShort<S, PipeReader<C32>, PipeWriter<C32>>,
    long: SyncLong<S, PipeReader<C32>, PipeWriter<C32>>,
    eq: FrameEqualizer<S, PipeReader<C32>, PipeWriter<u8>>,
    dec: Decoder<S, PipeReader<u8>>,
}

impl<S: Standard> Receiver<S> {
    pub fn new(threshold: f32, invalid_frames: bool) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            sync: SyncShort::new(threshold),
            long: SyncLong::new(),
            eq: FrameEqualizer::new(),
            dec: Decoder::with_hard(invalid_frames, false),
        }
    }
}

impl<S: Standard> Kernel for Receiver<S> {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mo: &mut MessageOutputs,
        meta: &BlockMeta,
    ) -> Result<()> {
        // Everything the input has, to the first stage.
        let (input, tags) = self.input.slice_with_tags();
        let n = input.len();
        self.sync.input.push(input, tags);
        self.input.consume(n);
        let ended = self.input.finished() && self.input.slice().is_empty();

        // Then each stage in turn, until none makes progress.
        let mut inner = WorkIo {
            call_again: false,
            finished: false,
        };
        loop {
            let before = (
                self.sync.input.len(),
                self.long.input.len(),
                self.eq.input.len(),
                self.dec.input.len(),
            );
            self.sync.work(&mut inner, mo, meta).await?;
            self.sync.output.drain_into(&mut self.long.input);
            self.long.work(&mut inner, mo, meta).await?;
            self.long.output.drain_into(&mut self.eq.input);
            self.eq.work(&mut inner, mo, meta).await?;
            self.eq.output.drain_into(&mut self.dec.input);
            self.dec.work(&mut inner, mo, meta).await?;
            let after = (
                self.sync.input.len(),
                self.long.input.len(),
                self.eq.input.len(),
                self.dec.input.len(),
            );
            if after == before {
                break;
            }
        }
        if ended {
            io.finished = true;
        }
        Ok(())
    }
}
