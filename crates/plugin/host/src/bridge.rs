//! Streams between flowgraphs.
//!
//! A [`Channel`] carries one stream from a flowgraph output to a flowgraph
//! input. The bridge blocks at both ends hold a *generation*: only the
//! channel's current reader may take items and only its current writer may
//! add them, so moving an end to another flowgraph is a single switch under
//! the channel's lock.

use std::any::Any;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use futuresdr::runtime::dev::prelude::*;

use crate::items::ItemType;

/// Generation of no bridge block.
pub(crate) const NOBODY: u64 = 0;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

struct State<T> {
    queue: VecDeque<T>,
    reader: u64,
    writer: u64,
    /// Take over, in order, as the writers before them finish.
    pending_writers: VecDeque<u64>,
    /// Queued writers whose flowgraph has not started yet: it may never
    /// start, so their end is not the stream's end.
    uncommitted: Vec<u64>,
    /// Uncommitted writers that finished; the stream ends when they are
    /// committed, unless another writer follows.
    finished_early: Vec<u64>,
    /// Writes are dropped while false.
    accept: bool,
    /// The writer finished with nobody to follow: end of stream.
    closed: bool,
    dropped: u64,
    readers_waiting: Vec<Waker>,
    writers_waiting: Vec<Waker>,
}

/// The queue between two flowgraphs.
pub(crate) struct Channel<T> {
    item: ItemType,
    capacity: usize,
    state: Mutex<State<T>>,
}

fn wake(wakers: Vec<Waker>) {
    wakers.into_iter().for_each(Waker::wake);
}

pub(crate) enum Write {
    /// Items taken (or dropped); consume them.
    Taken,
    /// This bridge's turn has not come yet; keep the items.
    Wait,
}

pub(crate) enum Read {
    Items(usize),
    Empty,
    End,
}

impl<T: CpuSample> Channel<T> {
    pub(crate) fn new(item: ItemType, capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            item,
            capacity,
            state: Mutex::new(State {
                queue: VecDeque::new(),
                reader: NOBODY,
                writer: NOBODY,
                pending_writers: VecDeque::new(),
                uncommitted: Vec::new(),
                finished_early: Vec::new(),
                accept: true,
                closed: false,
                dropped: 0,
                readers_waiting: Vec::new(),
                writers_waiting: Vec::new(),
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write(&self, generation: u64, items: &[T]) -> Write {
        let mut st = self.lock();
        if st.writer != generation {
            return if st.pending_writers.contains(&generation) {
                Write::Wait
            } else {
                // Replaced: nobody reads what this bridge writes any more.
                Write::Taken
            };
        }
        if !st.accept || items.is_empty() {
            return Write::Taken;
        }
        st.queue.extend(items.iter().cloned());
        let excess = st.queue.len().saturating_sub(self.capacity);
        if excess > 0 {
            st.queue.drain(..excess);
            st.dropped += excess as u64;
        }
        let waiting = std::mem::take(&mut st.readers_waiting);
        drop(st);
        wake(waiting);
        Write::Taken
    }

    /// Take items into `out` for the reader `generation`. `active` records
    /// whether this reader ever had its turn.
    pub(crate) fn read(&self, generation: u64, out: &mut [T], active: &mut bool) -> Read {
        let mut st = self.lock();
        if st.reader != generation {
            // A reader that had its turn is done; one that did not waits.
            return if *active { Read::End } else { Read::Empty };
        }
        *active = true;
        let n = out.len().min(st.queue.len());
        if n == 0 {
            return if st.closed { Read::End } else { Read::Empty };
        }
        for (slot, item) in out.iter_mut().zip(st.queue.drain(..n)) {
            *slot = item;
        }
        Read::Items(n)
    }

    /// Writer `generation` finished: hand over to the next writer, or end
    /// the stream if there is none. A writer that is not committed yet only
    /// withdraws; its end counts once it is committed.
    pub(crate) fn writer_finished(&self, generation: u64) {
        let mut st = self.lock();
        let committed = !st.uncommitted.contains(&generation);
        if !committed {
            st.finished_early.push(generation);
        }
        Self::remove_writer(st, generation, committed);
    }

    fn remove_writer(mut st: MutexGuard<'_, State<T>>, generation: u64, end_of_stream: bool) {
        if st.writer == generation {
            match st.pending_writers.pop_front() {
                Some(next) => st.writer = next,
                None => {
                    st.writer = NOBODY;
                    st.closed = end_of_stream;
                }
            }
        } else {
            st.pending_writers.retain(|g| *g != generation);
        }
        let readers = std::mem::take(&mut st.readers_waiting);
        let writers = std::mem::take(&mut st.writers_waiting);
        drop(st);
        wake(readers);
        wake(writers);
    }

    fn reader_ready(&self, generation: u64, was_reader: bool, cx: &mut Context<'_>) -> Poll<()> {
        let mut st = self.lock();
        let is_reader = st.reader == generation;
        if is_reader != was_reader || (is_reader && (!st.queue.is_empty() || st.closed)) {
            return Poll::Ready(());
        }
        st.readers_waiting.push(cx.waker().clone());
        Poll::Pending
    }

    fn writer_ready(&self, generation: u64, cx: &mut Context<'_>) -> Poll<()> {
        let mut st = self.lock();
        if !st.pending_writers.contains(&generation) {
            return Poll::Ready(());
        }
        st.writers_waiting.push(cx.waker().clone());
        Poll::Pending
    }
}

/// What the controller does with a channel, whatever its item type.
pub(crate) trait Pipe: Send + Sync {
    fn item(&self) -> ItemType;
    /// Make `generation` the only reader. Wakes the old and the new one.
    fn set_reader(&self, generation: u64);
    /// Let `generation` write once the writers before it have finished (at
    /// once if there are none).
    fn queue_writer(&self, generation: u64);
    /// The flowgraph of `generation` started: from now on its end is the
    /// stream's end.
    fn commit_writer(&self, generation: u64);
    /// Take back [`queue_writer`](Pipe::queue_writer), without ending the
    /// stream.
    fn unqueue_writer(&self, generation: u64);
    fn set_accept(&self, accept: bool);
    /// Make `generation` the only reader, starting on an empty queue, and
    /// take writes again: [`set_reader`](Pipe::set_reader) for
    /// [`Hold::Discard`](crate::Hold::Discard), in one step.
    fn restart_reader(&self, generation: u64);
    fn stats(&self) -> ChannelStats;
    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

/// State of a channel between two flowgraphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelStats {
    /// Items waiting.
    pub queued: usize,
    /// Items dropped because the queue was full.
    pub dropped: u64,
    /// The stream ended.
    pub closed: bool,
}

impl<T: CpuSample> Pipe for Channel<T> {
    fn item(&self) -> ItemType {
        self.item
    }

    fn set_reader(&self, generation: u64) {
        let mut st = self.lock();
        st.reader = generation;
        let waiting = std::mem::take(&mut st.readers_waiting);
        drop(st);
        wake(waiting);
    }

    fn queue_writer(&self, generation: u64) {
        let mut st = self.lock();
        if st.writer == NOBODY {
            st.writer = generation;
            st.closed = false;
        } else {
            st.pending_writers.push_back(generation);
        }
        st.uncommitted.push(generation);
        let waiting = std::mem::take(&mut st.writers_waiting);
        drop(st);
        wake(waiting);
    }

    fn commit_writer(&self, generation: u64) {
        let mut st = self.lock();
        st.uncommitted.retain(|g| *g != generation);
        let len = st.finished_early.len();
        st.finished_early.retain(|g| *g != generation);
        let finished = st.finished_early.len() != len;
        if finished && st.writer == NOBODY && st.pending_writers.is_empty() {
            st.closed = true;
            let waiting = std::mem::take(&mut st.readers_waiting);
            drop(st);
            wake(waiting);
        }
    }

    fn unqueue_writer(&self, generation: u64) {
        let mut st = self.lock();
        st.uncommitted.retain(|g| *g != generation);
        st.finished_early.retain(|g| *g != generation);
        Self::remove_writer(st, generation, false);
    }

    fn set_accept(&self, accept: bool) {
        self.lock().accept = accept;
    }

    fn restart_reader(&self, generation: u64) {
        let mut st = self.lock();
        st.reader = generation;
        st.queue.clear();
        st.accept = true;
        let waiting = std::mem::take(&mut st.readers_waiting);
        drop(st);
        wake(waiting);
    }

    fn stats(&self) -> ChannelStats {
        let st = self.lock();
        ChannelStats {
            queued: st.queue.len(),
            dropped: st.dropped,
            closed: st.closed,
        }
    }

    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

/// Resolves when the reader state of a channel may have changed.
pub(crate) struct ReaderReady<T> {
    channel: Arc<Channel<T>>,
    generation: u64,
    was_reader: bool,
}

impl<T: CpuSample> Future for ReaderReady<T> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.channel
            .reader_ready(self.generation, self.was_reader, cx)
    }
}

/// Resolves when a waiting writer may write.
pub(crate) struct WriterReady<T> {
    channel: Arc<Channel<T>>,
    generation: u64,
}

impl<T: CpuSample> Future for WriterReady<T> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.channel.writer_ready(self.generation, cx)
    }
}

/// Feeds a flowgraph input from a channel.
#[derive(Block)]
pub(crate) struct BridgeSource<T: CpuSample> {
    #[output]
    output: DefaultCpuWriter<T>,
    channel: Arc<Channel<T>>,
    generation: u64,
    active: bool,
    wait: Option<ReaderReady<T>>,
}

impl<T: CpuSample> BridgeSource<T> {
    pub(crate) fn new(channel: Arc<Channel<T>>, generation: u64) -> Self {
        Self {
            output: DefaultCpuWriter::default(),
            channel,
            generation,
            active: false,
            wait: None,
        }
    }
}

impl<T: CpuSample> Kernel for BridgeSource<T> {
    type BlockOn = ReaderReady<T>;

    fn block_on(&mut self) -> Option<Pin<&mut ReaderReady<T>>> {
        self.wait.as_mut().map(Pin::new)
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let out = self.output.slice();
        if out.is_empty() {
            self.wait = None;
            return Ok(());
        }
        match self.channel.read(self.generation, out, &mut self.active) {
            Read::Items(n) => {
                self.output.produce(n);
                self.wait = None;
                io.call_again = true;
            }
            Read::Empty => {
                self.wait = Some(ReaderReady {
                    channel: self.channel.clone(),
                    generation: self.generation,
                    was_reader: self.active,
                });
            }
            Read::End => io.finished = true,
        }
        Ok(())
    }
}

/// Writes a flowgraph output into a channel.
#[derive(Block)]
pub(crate) struct BridgeSink<T: CpuSample> {
    #[input]
    input: DefaultCpuReader<T>,
    channel: Arc<Channel<T>>,
    generation: u64,
    wait: Option<WriterReady<T>>,
}

impl<T: CpuSample> BridgeSink<T> {
    pub(crate) fn new(channel: Arc<Channel<T>>, generation: u64) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            channel,
            generation,
            wait: None,
        }
    }
}

impl<T: CpuSample> Kernel for BridgeSink<T> {
    type BlockOn = WriterReady<T>;

    fn block_on(&mut self) -> Option<Pin<&mut WriterReady<T>>> {
        self.wait.as_mut().map(Pin::new)
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let items = self.input.slice();
        let n = items.len();
        match self.channel.write(self.generation, items) {
            Write::Taken => {
                self.wait = None;
                self.input.consume(n);
                if self.input.finished() {
                    io.finished = true;
                }
            }
            Write::Wait => {
                self.wait = Some(WriterReady {
                    channel: self.channel.clone(),
                    generation: self.generation,
                });
            }
        }
        Ok(())
    }

    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &BlockMeta) -> Result<()> {
        self.channel.writer_finished(self.generation);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_queue_drops_the_oldest() {
        let ch = Channel::<u8>::new(ItemType::U8, 4);
        ch.queue_writer(1);
        ch.set_reader(2);
        let _ = ch.write(1, &[1, 2, 3]);
        let _ = ch.write(1, &[4, 5, 6]);
        let mut out = [0; 8];
        let mut active = false;
        assert!(matches!(ch.read(2, &mut out, &mut active), Read::Items(4)));
        assert_eq!(&out[..4], &[3, 4, 5, 6]);
        assert_eq!(ch.stats().dropped, 2);
        // more than the capacity at once
        let _ = ch.write(1, &[1, 2, 3, 4, 5, 6, 7]);
        assert!(matches!(ch.read(2, &mut out, &mut active), Read::Items(4)));
        assert_eq!(&out[..4], &[4, 5, 6, 7]);
    }

    #[test]
    fn reader_switch_is_exact() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        ch.queue_writer(1);
        ch.set_reader(2);
        let _ = ch.write(1, &[1, 2, 3]);
        let (mut old, mut new) = (false, false);
        let mut out = [0; 2];
        assert!(matches!(ch.read(2, &mut out, &mut old), Read::Items(2)));
        assert!(
            matches!(ch.read(3, &mut out, &mut new), Read::Empty),
            "not its turn yet"
        );
        ch.set_reader(3);
        assert!(
            matches!(ch.read(2, &mut out, &mut old), Read::End),
            "the old reader is done"
        );
        assert!(matches!(ch.read(3, &mut out, &mut new), Read::Items(1)));
        assert_eq!(out[0], 3);
    }

    #[test]
    fn writers_take_turns_and_the_last_one_ends_the_stream() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        ch.set_reader(9);
        for g in 1..=2 {
            ch.queue_writer(g);
            ch.commit_writer(g);
        }
        assert!(matches!(ch.write(2, &[20]), Write::Wait));
        let _ = ch.write(1, &[10]);
        ch.writer_finished(1);
        let _ = ch.write(2, &[21]);
        assert!(
            matches!(ch.write(3, &[30]), Write::Taken),
            "a stranger is ignored"
        );
        ch.writer_finished(2);
        let mut out = [0; 4];
        let mut active = false;
        assert!(matches!(ch.read(9, &mut out, &mut active), Read::Items(2)));
        assert_eq!(&out[..2], &[10, 21]);
        assert!(matches!(ch.read(9, &mut out, &mut active), Read::End));
    }

    #[test]
    fn writers_queue_in_order_and_can_withdraw() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        ch.set_reader(9);
        for g in 1..=4 {
            ch.queue_writer(g);
            ch.commit_writer(g);
        }
        ch.unqueue_writer(3);
        assert!(
            matches!(ch.write(3, &[30]), Write::Taken),
            "withdrawn: dropped"
        );
        assert!(matches!(ch.write(4, &[40]), Write::Wait));
        ch.writer_finished(1);
        ch.writer_finished(2);
        let _ = ch.write(4, &[41]);
        ch.unqueue_writer(4);
        assert!(
            !ch.stats().closed,
            "withdrawing the writer does not end the stream"
        );
        let mut out = [0; 4];
        let mut active = false;
        assert!(matches!(ch.read(9, &mut out, &mut active), Read::Items(1)));
        assert_eq!(out[0], 41);
    }

    #[test]
    fn a_writer_ends_the_stream_only_once_committed() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        ch.set_reader(9);
        let mut out = [0; 4];
        let mut active = false;

        // Its flowgraph failed to start: withdrawn, the stream goes on.
        ch.queue_writer(1);
        ch.writer_finished(1);
        ch.unqueue_writer(1);
        assert!(!ch.stats().closed);

        // Its flowgraph started and already finished: the stream ends on commit.
        ch.queue_writer(2);
        let _ = ch.write(2, &[20]);
        ch.writer_finished(2);
        assert!(!ch.stats().closed, "not committed yet");
        ch.commit_writer(2);
        assert!(ch.stats().closed);
        assert!(matches!(ch.read(9, &mut out, &mut active), Read::Items(1)));
        assert!(matches!(ch.read(9, &mut out, &mut active), Read::End));

        // Ended early behind a running writer: the running writer's end
        // ends the stream.
        ch.queue_writer(3);
        ch.commit_writer(3);
        ch.queue_writer(4);
        ch.writer_finished(4);
        ch.commit_writer(4);
        assert!(!ch.stats().closed);
        ch.writer_finished(3);
        assert!(ch.stats().closed);
    }
}
