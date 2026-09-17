//! Streams between flowgraphs.
//!
//! A [`Channel`] carries one stream from a flowgraph output to a flowgraph
//! input. The bridge blocks at both ends hold a *generation*: only the
//! channel's current reader may take items and only its current writer may
//! add them, so moving an end to another flowgraph is a single switch under
//! the channel's lock.
//!
//! The bridges of a flowgraph that runs on standby wait: its sources are not
//! the reader, and its sinks are standby writers until they are committed.

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
    /// Committed writers; they take over, in order, as the writers before
    /// them finish.
    pending_writers: VecDeque<u64>,
    /// Writers of flowgraphs on standby: they wait, and since they may never
    /// be committed, their end is not the stream's end.
    standby_writers: Vec<u64>,
    /// Standby writers that finished; committing one of them ends the
    /// stream, unless another writer follows.
    finished_early: Vec<u64>,
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
                standby_writers: Vec::new(),
                finished_early: Vec::new(),
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
            return if st.waiting_writer(generation) {
                Write::Wait
            } else {
                // Replaced: nobody reads what this bridge writes any more.
                Write::Taken
            };
        }
        if items.is_empty() {
            return Write::Taken;
        }
        // Only the newest `capacity` items can stay: skip the others before
        // copying them.
        let skip = items.len().saturating_sub(self.capacity);
        let items = &items[skip..];
        let excess = (st.queue.len() + items.len()).saturating_sub(self.capacity);
        if excess > 0 {
            st.queue.drain(..excess);
        }
        st.dropped += (skip + excess) as u64;
        st.queue.extend(items);
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
        let (first, second) = st.queue.as_slices();
        let k = first.len().min(n);
        out[..k].copy_from_slice(&first[..k]);
        out[k..n].copy_from_slice(&second[..n - k]);
        st.queue.drain(..n);
        Read::Items(n)
    }

    /// Writer `generation` finished: hand over to the next writer, or end
    /// the stream if there is none. A standby writer only leaves the
    /// standby; its end counts once it is committed.
    pub(crate) fn writer_finished(&self, generation: u64) {
        let mut st = self.lock();
        if remove(&mut st.standby_writers, generation) {
            st.finished_early.push(generation);
            return;
        }
        Self::remove_writer(st, generation, true);
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
        if !st.waiting_writer(generation) {
            return Poll::Ready(());
        }
        st.writers_waiting.push(cx.waker().clone());
        Poll::Pending
    }
}

impl<T> State<T> {
    /// Writer `generation` has not had its turn yet.
    fn waiting_writer(&self, generation: u64) -> bool {
        self.pending_writers.contains(&generation) || self.standby_writers.contains(&generation)
    }
}

/// Remove `generation` from `list`; whether it was there.
fn remove(list: &mut Vec<u64>, generation: u64) -> bool {
    let len = list.len();
    list.retain(|g| *g != generation);
    list.len() != len
}

/// What the controller does with a channel, whatever its item type.
pub(crate) trait Pipe: Send + Sync {
    fn item(&self) -> ItemType;
    /// Make `generation` the only reader. Wakes the old and the new one.
    fn set_reader(&self, generation: u64);
    /// Make `generation` the only reader, on an empty queue:
    /// [`set_reader`](Pipe::set_reader) for
    /// [`Hold::Discard`](crate::Hold::Discard), in one step.
    fn restart_reader(&self, generation: u64);
    /// Put writer `generation` on standby: it waits until committed.
    fn standby_writer(&self, generation: u64);
    /// Let standby writer `generation` write once the writers before it have
    /// finished (at once if there are none). From now on its end is the
    /// stream's end.
    fn commit_writer(&self, generation: u64);
    /// Drop standby writer `generation`, without ending the stream.
    fn withdraw_writer(&self, generation: u64);
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

    fn restart_reader(&self, generation: u64) {
        let mut st = self.lock();
        st.reader = generation;
        st.queue.clear();
        let waiting = std::mem::take(&mut st.readers_waiting);
        drop(st);
        wake(waiting);
    }

    fn standby_writer(&self, generation: u64) {
        self.lock().standby_writers.push(generation);
    }

    fn commit_writer(&self, generation: u64) {
        let mut st = self.lock();
        let waiting = if remove(&mut st.finished_early, generation) {
            // It ended without writing: the stream ends with the writers
            // before it.
            if st.writer != NOBODY {
                return;
            }
            st.closed = true;
            std::mem::take(&mut st.readers_waiting)
        } else {
            if !remove(&mut st.standby_writers, generation) {
                return;
            }
            if st.writer == NOBODY {
                st.writer = generation;
                st.closed = false;
            } else {
                st.pending_writers.push_back(generation);
            }
            std::mem::take(&mut st.writers_waiting)
        };
        drop(st);
        wake(waiting);
    }

    fn withdraw_writer(&self, generation: u64) {
        let mut st = self.lock();
        remove(&mut st.standby_writers, generation);
        remove(&mut st.finished_early, generation);
        // It no longer waits: its bridge drops what it writes.
        let waiting = std::mem::take(&mut st.writers_waiting);
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

    /// Commit `generation` as a writer of `ch`.
    fn writer(ch: &Channel<u8>, generation: u64) {
        ch.standby_writer(generation);
        ch.commit_writer(generation);
    }

    fn read(ch: &Channel<u8>, generation: u64, active: &mut bool) -> (Read, Vec<u8>) {
        let mut out = [0; 16];
        let r = ch.read(generation, &mut out, active);
        let n = if let Read::Items(n) = r { n } else { 0 };
        (r, out[..n].to_vec())
    }

    #[test]
    fn full_queue_drops_the_oldest() {
        let ch = Channel::<u8>::new(ItemType::U8, 4);
        writer(&ch, 1);
        ch.set_reader(2);
        let _ = ch.write(1, &[1, 2, 3]);
        let _ = ch.write(1, &[4, 5, 6]);
        let mut active = false;
        assert_eq!(read(&ch, 2, &mut active).1, [3, 4, 5, 6]);
        assert_eq!(ch.stats().dropped, 2);
        // more than the capacity at once
        let _ = ch.write(1, &[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(read(&ch, 2, &mut active).1, [4, 5, 6, 7]);
    }

    #[test]
    fn reader_switch_is_exact() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        writer(&ch, 1);
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
    fn a_restarted_reader_starts_on_an_empty_queue() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        writer(&ch, 1);
        ch.set_reader(2);
        let _ = ch.write(1, &[1, 2, 3]);
        ch.restart_reader(3);
        let _ = ch.write(1, &[4]);
        let (mut old, mut new) = (true, false);
        assert!(matches!(read(&ch, 2, &mut old).0, Read::End));
        assert_eq!(read(&ch, 3, &mut new).1, [4]);
    }

    #[test]
    fn writers_take_turns_and_the_last_one_ends_the_stream() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        ch.set_reader(9);
        writer(&ch, 1);
        writer(&ch, 2);
        assert!(matches!(ch.write(2, &[20]), Write::Wait));
        let _ = ch.write(1, &[10]);
        ch.writer_finished(1);
        let _ = ch.write(2, &[21]);
        assert!(
            matches!(ch.write(3, &[30]), Write::Taken),
            "a stranger is ignored"
        );
        ch.writer_finished(2);
        let mut active = false;
        assert_eq!(read(&ch, 9, &mut active).1, [10, 21]);
        assert!(matches!(read(&ch, 9, &mut active).0, Read::End));
    }

    #[test]
    fn standby_writers_wait_and_take_their_turn_when_committed() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        ch.set_reader(9);
        writer(&ch, 1);
        for g in 2..=4 {
            ch.standby_writer(g);
        }
        assert!(matches!(ch.write(3, &[30]), Write::Wait));
        // Committed out of order: 4 before 2; 3 is withdrawn.
        ch.commit_writer(4);
        ch.commit_writer(2);
        ch.withdraw_writer(3);
        assert!(
            matches!(ch.write(3, &[30]), Write::Taken),
            "withdrawn: dropped"
        );
        assert!(matches!(ch.write(2, &[20]), Write::Wait));
        let _ = ch.write(1, &[10]);
        ch.writer_finished(1);
        assert!(matches!(ch.write(2, &[20]), Write::Wait), "4 comes first");
        let _ = ch.write(4, &[40]);
        ch.writer_finished(4);
        let _ = ch.write(2, &[20]);
        let mut active = false;
        assert_eq!(read(&ch, 9, &mut active).1, [10, 40, 20]);
        assert!(!ch.stats().closed);
    }

    #[test]
    fn a_writer_ends_the_stream_only_once_committed() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        ch.set_reader(9);
        let mut active = false;

        // Its flowgraph never goes live: withdrawn, the stream goes on.
        ch.standby_writer(1);
        ch.writer_finished(1);
        ch.withdraw_writer(1);
        assert!(!ch.stats().closed);

        // Its flowgraph finished on standby: the stream ends on commit.
        ch.standby_writer(2);
        ch.writer_finished(2);
        assert!(!ch.stats().closed, "not committed yet");
        ch.commit_writer(2);
        assert!(ch.stats().closed);
        assert!(matches!(read(&ch, 9, &mut active).0, Read::End));

        // Finished on standby behind a running writer: the running writer's
        // end ends the stream.
        writer(&ch, 3);
        assert!(!ch.stats().closed, "a new writer reopens the stream");
        ch.standby_writer(4);
        ch.writer_finished(4);
        ch.commit_writer(4);
        assert!(!ch.stats().closed);
        ch.writer_finished(3);
        assert!(ch.stats().closed);
    }
}

/// The channel against a reference model, under random operations: same
/// results, same statistics, same readiness, and no lost wake-up.
#[cfg(test)]
mod model {
    use std::collections::BTreeMap;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::test_rng::Rng;
    use crate::test_rng::for_each_seed;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum W {
        Standby,
        StandbyEnded,
        Committed,
        Done,
        Withdrawn,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Items(Vec<i32>),
        Empty,
        End,
    }

    #[derive(Default)]
    struct Model {
        capacity: usize,
        writers: BTreeMap<u64, W>,
        /// Committed writers that have not finished, in commit order; the
        /// first one writes.
        turns: VecDeque<u64>,
        queue: VecDeque<i32>,
        reader: u64,
        closed: bool,
        dropped: u64,
    }

    impl Model {
        fn state(&self, g: u64) -> Option<W> {
            self.writers.get(&g).copied()
        }

        fn commit(&mut self, g: u64) {
            match self.state(g) {
                Some(W::Standby) => {
                    self.writers.insert(g, W::Committed);
                    if self.turns.is_empty() {
                        self.closed = false;
                    }
                    self.turns.push_back(g);
                }
                Some(W::StandbyEnded) => {
                    self.writers.insert(g, W::Done);
                    if self.turns.is_empty() {
                        self.closed = true;
                    }
                }
                _ => {}
            }
        }

        fn withdraw(&mut self, g: u64) {
            if matches!(self.state(g), Some(W::Standby | W::StandbyEnded)) {
                self.writers.insert(g, W::Withdrawn);
            }
        }

        fn finished(&mut self, g: u64) {
            match self.state(g) {
                Some(W::Standby) => {
                    self.writers.insert(g, W::StandbyEnded);
                }
                Some(W::Committed) => {
                    self.writers.insert(g, W::Done);
                    let was_writing = self.turns.front() == Some(&g);
                    self.turns.retain(|t| *t != g);
                    if was_writing && self.turns.is_empty() {
                        self.closed = true;
                    }
                }
                _ => {}
            }
        }

        fn waiting(&self, g: u64) -> bool {
            match self.state(g) {
                Some(W::Standby) => true,
                Some(W::Committed) => self.turns.front() != Some(&g),
                _ => false,
            }
        }

        /// Whether the items were taken (queued or dropped).
        fn write(&mut self, g: u64, items: &[i32]) -> bool {
            if self.turns.front() != Some(&g) {
                return !self.waiting(g);
            }
            self.queue.extend(items);
            while self.queue.len() > self.capacity {
                self.queue.pop_front();
                self.dropped += 1;
            }
            true
        }

        fn read(&mut self, r: u64, max: usize, active: &mut bool) -> Outcome {
            if self.reader != r {
                return if *active {
                    Outcome::End
                } else {
                    Outcome::Empty
                };
            }
            *active = true;
            if self.queue.is_empty() {
                return if self.closed {
                    Outcome::End
                } else {
                    Outcome::Empty
                };
            }
            let n = max.min(self.queue.len());
            Outcome::Items(self.queue.drain(..n).collect())
        }

        fn reader_ready(&self, r: u64, was_reader: bool) -> bool {
            let is_reader = self.reader == r;
            is_reader != was_reader || (is_reader && (!self.queue.is_empty() || self.closed))
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Op {
        Standby(u64),
        Commit(u64),
        Withdraw(u64),
        Finished(u64),
        Write(u64, usize),
        SetReader(u64),
        RestartReader(u64),
        Read(u64, usize),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum Waiter {
        Writer(u64),
        Reader(u64, bool),
    }

    struct Flag(AtomicBool);

    impl std::task::Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn poll(ch: &Channel<i32>, waiter: Waiter, flag: &Arc<Flag>) -> bool {
        let waker = Waker::from(flag.clone());
        let mut cx = Context::from_waker(&waker);
        let ready = match waiter {
            Waiter::Writer(g) => ch.writer_ready(g, &mut cx),
            Waiter::Reader(r, was) => ch.reader_ready(r, was, &mut cx),
        };
        ready.is_ready()
    }

    fn op(rng: &mut Rng, next_gen: &mut u64, writers: &[u64], finished: &[u64]) -> Op {
        let live: Vec<u64> = writers
            .iter()
            .copied()
            .filter(|g| !finished.contains(g))
            .collect();
        let any = |rng: &mut Rng| *rng.pick(writers);
        let reader = |rng: &mut Rng| 100 + rng.below(3) as u64;
        match rng.below(100) {
            _ if writers.is_empty() => {
                *next_gen += 1;
                Op::Standby(*next_gen)
            }
            0..10 => {
                *next_gen += 1;
                Op::Standby(*next_gen)
            }
            10..25 => Op::Commit(any(rng)),
            25..30 => Op::Withdraw(any(rng)),
            30..40 => Op::Finished(any(rng)),
            40..65 if !live.is_empty() => Op::Write(*rng.pick(&live), rng.range(0, 6)),
            65..70 => Op::SetReader(reader(rng)),
            70..73 => Op::RestartReader(reader(rng)),
            _ => Op::Read(reader(rng), rng.range(1, 8)),
        }
    }

    #[test]
    fn channel_matches_its_model() {
        for_each_seed(300, |rng| {
            let capacity = rng.range(1, 12);
            let ch = Channel::<i32>::new(ItemType::I32, capacity);
            let mut model = Model {
                capacity,
                reader: NOBODY,
                ..Model::default()
            };
            let (mut next_gen, mut next_item) = (0u64, 0i32);
            let (mut writers, mut finished) = (Vec::new(), Vec::new());
            // Items a writer still has to write after being told to wait.
            let mut unsent: HashMap<u64, Vec<i32>> = HashMap::new();
            let mut active: HashMap<u64, bool> = HashMap::new();
            let mut waiting: Vec<(Waiter, Arc<Flag>)> = Vec::new();

            for step in 0..200 {
                let op = op(rng, &mut next_gen, &writers, &finished);
                let at = format!("step {step}: {op:?}");
                if let Op::Finished(g) = op {
                    // A finished bridge waits no more.
                    waiting.retain(|(w, _)| *w != Waiter::Writer(g));
                }
                match op {
                    Op::Standby(g) => {
                        writers.push(g);
                        model.writers.insert(g, W::Standby);
                        ch.standby_writer(g);
                    }
                    Op::Commit(g) => {
                        model.commit(g);
                        ch.commit_writer(g);
                    }
                    Op::Withdraw(g) => {
                        model.withdraw(g);
                        ch.withdraw_writer(g);
                    }
                    Op::Finished(g) => {
                        finished.push(g);
                        model.finished(g);
                        ch.writer_finished(g);
                    }
                    Op::Write(g, n) => {
                        let items = unsent.entry(g).or_default();
                        items.extend(next_item..next_item + n as i32);
                        next_item += n as i32;
                        let taken = model.write(g, items);
                        let real = ch.write(g, items);
                        assert_eq!(matches!(real, Write::Taken), taken, "{at}");
                        if taken {
                            items.clear();
                        }
                    }
                    Op::SetReader(r) => {
                        model.reader = r;
                        ch.set_reader(r);
                    }
                    Op::RestartReader(r) => {
                        model.reader = r;
                        model.queue.clear();
                        ch.restart_reader(r);
                    }
                    Op::Read(r, max) => {
                        let mut model_active = *active.get(&r).unwrap_or(&false);
                        let expected = model.read(r, max, &mut model_active);
                        let real_active = active.entry(r).or_default();
                        let mut out = vec![0; max];
                        let got = match ch.read(r, &mut out, real_active) {
                            Read::Items(n) => Outcome::Items(out[..n].to_vec()),
                            Read::Empty => Outcome::Empty,
                            Read::End => Outcome::End,
                        };
                        assert_eq!(got, expected, "{at}");
                        assert_eq!(*real_active, model_active, "{at}");
                    }
                }
                let stats = ch.stats();
                assert_eq!(
                    (stats.queued, stats.dropped, stats.closed),
                    (model.queue.len(), model.dropped, model.closed),
                    "{at}"
                );

                // Whoever waited and may go on now must have been woken.
                for (waiter, flag) in waiting.drain(..) {
                    let ready = match waiter {
                        Waiter::Writer(g) => !model.waiting(g),
                        Waiter::Reader(r, was) => model.reader_ready(r, was),
                    };
                    assert!(
                        !ready || flag.0.load(Ordering::SeqCst),
                        "{at}: {waiter:?} was not woken"
                    );
                }
                // Everyone waits again, where the model says so.
                let mut waiters: Vec<Waiter> = writers
                    .iter()
                    .filter(|g| !finished.contains(g))
                    .map(|g| Waiter::Writer(*g))
                    .collect();
                waiters
                    .extend((100..103).map(|r| Waiter::Reader(r, active.get(&r) == Some(&true))));
                for waiter in waiters {
                    let flag = Arc::new(Flag(AtomicBool::new(false)));
                    let expected = match waiter {
                        Waiter::Writer(g) => !model.waiting(g),
                        Waiter::Reader(r, was) => model.reader_ready(r, was),
                    };
                    assert_eq!(poll(&ch, waiter, &flag), expected, "{at}: {waiter:?}");
                    if !expected {
                        waiting.push((waiter, flag));
                    }
                }
            }
        });
    }
}

/// Writer and reader threads that go through a series of commits: every
/// committed writer's items arrive, in commit order, a withdrawn writer's
/// never do, and nobody hangs.
#[cfg(test)]
mod threads {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::Duration;

    use futuresdr::runtime::block_on;

    use super::*;
    use crate::test_rng::for_each_seed;

    fn write_all(ch: &Arc<Channel<i32>>, g: u64, items: &[i32], chunk: usize) {
        for chunk in items.chunks(chunk) {
            while let Write::Wait = ch.write(g, chunk) {
                block_on(WriterReady {
                    channel: ch.clone(),
                    generation: g,
                });
            }
        }
    }

    #[test]
    fn writers_hand_over_without_loss_or_hang() {
        for_each_seed(40, |rng| {
            let ch = Channel::<i32>::new(ItemType::I32, usize::MAX);
            let n_writers = rng.range(1, 5);
            let (reader, withdrawn, ended) = (100, 50, 51);
            let mut order: Vec<u64> = (1..=n_writers as u64).collect();
            rng.shuffle(&mut order);
            // A writer that finished on standby, committed while another one
            // writes (committed first, it would end the stream at once).
            let at = rng.range(1, order.len());
            order.insert(at, ended);

            let mut threads = Vec::new();
            let mut may_finish = HashMap::new();
            let mut expected: Vec<i32> = Vec::new();
            for &g in &order {
                ch.standby_writer(g);
                if g == ended {
                    ch.writer_finished(g);
                    continue;
                }
                let items: Vec<i32> = (0..rng.range(0, 3000) as i32)
                    .map(|i| g as i32 * 1_000_000 + i)
                    .collect();
                expected.extend(&items);
                let (tx, rx) = mpsc::channel::<()>();
                may_finish.insert(g, tx);
                let (ch, chunk) = (ch.clone(), rng.range(1, 64));
                threads.push(std::thread::spawn(move || {
                    write_all(&ch, g, &items, chunk);
                    rx.recv().unwrap();
                    ch.writer_finished(g);
                }));
            }
            ch.standby_writer(withdrawn);
            let lost = ch.clone();
            threads.push(std::thread::spawn(move || {
                write_all(&lost, withdrawn, &[-1; 100], 7);
                lost.writer_finished(withdrawn);
            }));

            let (done_tx, done_rx) = mpsc::channel();
            let reads = ch.clone();
            let reader_thread = std::thread::spawn(move || {
                let (mut got, mut active, mut out) = (Vec::<i32>::new(), false, [0; 97]);
                loop {
                    match reads.read(reader, &mut out, &mut active) {
                        Read::Items(n) => got.extend(&out[..n]),
                        Read::Empty => block_on(ReaderReady {
                            channel: reads.clone(),
                            generation: reader,
                            was_reader: active,
                        }),
                        Read::End => break,
                    }
                }
                done_tx.send(got).unwrap();
            });

            // Commit in order, with pauses; a writer may finish once the next
            // one that writes is committed (make before break).
            let reader_at = rng.below(order.len());
            let withdraw_at = rng.below(order.len());
            let mut previous: Option<u64> = None;
            for (i, &g) in order.iter().enumerate() {
                if i == reader_at {
                    ch.set_reader(reader);
                }
                if i == withdraw_at {
                    ch.withdraw_writer(withdrawn);
                }
                std::thread::sleep(Duration::from_micros(rng.range(0, 300) as u64));
                ch.commit_writer(g);
                if g != ended
                    && let Some(p) = previous.replace(g)
                {
                    may_finish[&p].send(()).unwrap();
                }
            }
            if let Some(p) = previous {
                may_finish[&p].send(()).unwrap();
            }

            let got = done_rx
                .recv_timeout(Duration::from_secs(20))
                .expect("the reader hangs");
            reader_thread.join().unwrap();
            for t in threads {
                t.join().unwrap();
            }
            assert!(
                got == expected,
                "{} items, expected {}",
                got.len(),
                expected.len()
            );
        });
    }
}

/// Items per second through a bridge, against the same flowgraph without
/// one. `cargo test --release -p futuresdr-plugin-host --lib -- --ignored
/// --nocapture throughput`
#[cfg(test)]
mod throughput {
    use std::time::Duration;
    use std::time::Instant;

    use futuresdr::blocks::NullSink;
    use futuresdr::blocks::NullSource;
    use futuresdr::num_complex::Complex32;
    use futuresdr::runtime::BlockRef;
    use futuresdr::runtime::Flowgraph;
    use futuresdr::runtime::Runtime;
    use futuresdr::runtime::block_on;

    use super::*;

    const RUN: Duration = Duration::from_secs(3);

    /// Run the flowgraphs for `RUN`, then count what `snk` of the first one got.
    fn received<T: CpuSample>(
        rt: &Runtime,
        fgs: Vec<Flowgraph>,
        snk: &BlockRef<NullSink<T>>,
    ) -> f64 {
        let running: Vec<_> = fgs
            .into_iter()
            .map(|fg| rt.start(fg).unwrap().split())
            .collect();
        let t = Instant::now();
        std::thread::sleep(RUN);
        let elapsed = t.elapsed();
        let mut done = Vec::new();
        for (task, handle) in running {
            block_on(handle.stop()).unwrap();
            done.push(block_on(task).unwrap());
        }
        done[0].block(snk).unwrap().n_received() as f64 / elapsed.as_secs_f64()
    }

    fn bench<T: CpuSample>(rt: &Runtime, item: ItemType, capacity: usize) {
        let mut fg = Flowgraph::new();
        let src = fg.add(NullSource::<T>::new()).unwrap();
        let snk = fg.add(NullSink::<T>::new()).unwrap();
        fg.stream_dyn(src.id(), "output", snk.id(), "input")
            .unwrap();
        let direct = received(rt, vec![fg], &snk);

        let channel = Channel::<T>::new(item, capacity);
        let (writer, reader) = (next_generation(), next_generation());
        let mut up = Flowgraph::new();
        let src = up.add(NullSource::<T>::new()).unwrap();
        let bsnk = up.add(BridgeSink::new(channel.clone(), writer)).unwrap();
        up.stream_dyn(src.id(), "output", bsnk.id(), "input")
            .unwrap();
        let mut down = Flowgraph::new();
        let bsrc = down
            .add(BridgeSource::new(channel.clone(), reader))
            .unwrap();
        let snk = down.add(NullSink::<T>::new()).unwrap();
        down.stream_dyn(bsrc.id(), "output", snk.id(), "input")
            .unwrap();
        channel.standby_writer(writer);
        channel.commit_writer(writer);
        channel.set_reader(reader);
        let bridged = received(rt, vec![down, up], &snk);
        println!(
            "dynv4 {:<5} direct {:9.1} M/s   bridged {:9.1} M/s",
            item.name(),
            direct / 1e6,
            bridged / 1e6
        );
    }

    #[test]
    #[ignore]
    fn throughput() {
        let rt = Runtime::new();
        // The capacities of the dyn branch's bridges, or $THROUGHPUT_CAPACITY.
        let capacity = |dyn_capacity| {
            std::env::var("THROUGHPUT_CAPACITY").map_or(dyn_capacity, |c| c.parse().unwrap())
        };
        bench::<u8>(&rt, ItemType::U8, capacity(32_768));
        bench::<f32>(&rt, ItemType::F32, capacity(8_192));
        bench::<Complex32>(&rt, ItemType::Complex32, capacity(32_768));
    }
}
