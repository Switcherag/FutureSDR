//! Streams between flowgraphs.
//!
//! A [`Channel`] carries the stream of one flowgraph output to the inputs
//! linked to it, each through a *subscription* with a queue of its own. The
//! bridge blocks at both ends hold a *generation*: only a subscription's
//! current reader may take its items and only the channel's current writer
//! may add them, so moving an end to another flowgraph is a single switch
//! under the channel's lock.
//!
//! The bridges of a flowgraph that runs on standby wait: its sources are not
//! the reader, and its sinks are standby writers until they are committed.
//!
//! A *parked* subscription gets no items: its flowgraph idles, and writing
//! costs nothing for it.

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
use futuresdr_plugin_rt::buffer::ReuseCpuReader;
use futuresdr_plugin_rt::buffer::ReuseCpuWriter;

use crate::Hold;
use crate::items::ItemType;

/// Generation of no bridge block.
pub(crate) const NOBODY: u64 = 0;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// A new generation, or subscription id.
pub(crate) fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

struct Subscription<T> {
    id: u64,
    queue: VecDeque<T>,
    /// Position in the stream of the first queued item.
    head: u64,
    /// Tags of queued items, by position in the stream, in order.
    tags: VecDeque<(u64, Tag)>,
    reader: u64,
    parked: bool,
    dropped: u64,
    readers_waiting: Vec<Waker>,
}

impl<T> Subscription<T> {
    fn new(id: u64, parked: bool) -> Self {
        Self {
            id,
            queue: VecDeque::new(),
            head: 0,
            tags: VecDeque::new(),
            reader: NOBODY,
            parked,
            dropped: 0,
            readers_waiting: Vec::new(),
        }
    }

    /// Drop the first `n` queued items, and their tags.
    fn drop_front(&mut self, n: usize) {
        self.queue.drain(..n);
        self.head += n as u64;
        while self.tags.front().is_some_and(|(at, _)| *at < self.head) {
            self.tags.pop_front();
        }
    }

    /// Drop every queued item, and their tags.
    fn clear(&mut self) {
        self.drop_front(self.queue.len());
    }
}

struct State<T> {
    subscriptions: Vec<Subscription<T>>,
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
    /// Writers whose first write when their turn comes is dropped: what
    /// they held on standby.
    drop_held: Vec<u64>,
    /// The writer finished with nobody to follow: end of stream.
    closed: bool,
    writers_waiting: Vec<Waker>,
}

/// The queues between a flowgraph output and the inputs linked to it.
pub(crate) struct Channel<T> {
    item: ItemType,
    capacity: usize,
    state: Mutex<State<T>>,
}

fn wake(wakers: Vec<Waker>) {
    wakers.into_iter().for_each(Waker::wake);
}

/// Move the wakers of `from` to `to`.
fn gather(to: &mut Vec<Waker>, from: &mut Vec<Waker>) {
    if to.is_empty() {
        std::mem::swap(to, from);
    } else {
        to.append(from);
    }
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
                subscriptions: Vec::new(),
                writer: NOBODY,
                pending_writers: VecDeque::new(),
                standby_writers: Vec::new(),
                finished_early: Vec::new(),
                drop_held: Vec::new(),
                closed: false,
                writers_waiting: Vec::new(),
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn write(&self, generation: u64, items: &[T]) -> Write {
        self.write_tagged(generation, items, &[])
    }

    /// [`write`](Self::write) `items` with their `tags`, indexed in `items`.
    pub(crate) fn write_tagged(&self, generation: u64, items: &[T], tags: &[ItemTag]) -> Write {
        let mut st = self.lock();
        if st.writer != generation {
            return if st.waiting_writer(generation) {
                Write::Wait
            } else {
                // Replaced: nobody reads what this bridge writes any more.
                Write::Taken
            };
        }
        if items.is_empty() || remove(&mut st.drop_held, generation) {
            return Write::Taken;
        }
        // Only the newest `capacity` items can stay: skip the others before
        // copying them.
        let skip = items.len().saturating_sub(self.capacity);
        let items = &items[skip..];
        let mut waiting = Vec::new();
        for sub in st.subscriptions.iter_mut().filter(|s| !s.parked) {
            let excess = (sub.queue.len() + items.len()).saturating_sub(self.capacity);
            if excess > 0 {
                sub.drop_front(excess);
            }
            sub.dropped += (skip + excess) as u64;
            let base = sub.head + sub.queue.len() as u64;
            for t in tags.iter().filter(|t| t.index >= skip) {
                sub.tags
                    .push_back((base + (t.index - skip) as u64, t.tag.clone()));
            }
            sub.queue.extend(items);
            gather(&mut waiting, &mut sub.readers_waiting);
        }
        drop(st);
        wake(waiting);
        Write::Taken
    }

    /// Take items of subscription `sub` into `out` for the reader
    /// `generation`. `active` records whether this reader ever had its turn.
    #[cfg(test)]
    pub(crate) fn read(&self, sub: u64, generation: u64, out: &mut [T], active: &mut bool) -> Read {
        self.read_tagged(sub, generation, out, &mut Vec::new(), active)
    }

    /// [`read`](Self::read), and the tags of the items read into `tags`,
    /// indexed in `out`.
    pub(crate) fn read_tagged(
        &self,
        sub: u64,
        generation: u64,
        out: &mut [T],
        tags: &mut Vec<ItemTag>,
        active: &mut bool,
    ) -> Read {
        tags.clear();
        let mut st = self.lock();
        let closed = st.closed;
        let Some(s) = st.subscription(sub) else {
            // The link is gone.
            return Read::End;
        };
        if s.reader != generation {
            // A reader that had its turn is done; one that did not waits.
            return if *active { Read::End } else { Read::Empty };
        }
        *active = true;
        let n = out.len().min(s.queue.len());
        if n == 0 {
            return if closed { Read::End } else { Read::Empty };
        }
        let (first, second) = s.queue.as_slices();
        let k = first.len().min(n);
        out[..k].copy_from_slice(&first[..k]);
        out[k..n].copy_from_slice(&second[..n - k]);
        let end = s.head + n as u64;
        while s.tags.front().is_some_and(|(at, _)| *at < end) {
            let (at, tag) = s.tags.pop_front().unwrap();
            tags.push(ItemTag {
                index: (at - s.head) as usize,
                tag,
            });
        }
        s.drop_front(n);
        Read::Items(n)
    }

    /// Writer `generation` finished: hand over to the next writer, or end
    /// the stream if there is none. A standby writer only leaves the
    /// standby; its end counts once it is committed.
    pub(crate) fn writer_finished(&self, generation: u64) {
        let mut st = self.lock();
        remove(&mut st.drop_held, generation);
        if remove(&mut st.standby_writers, generation) {
            st.finished_early.push(generation);
            return;
        }
        if st.writer == generation {
            match st.pending_writers.pop_front() {
                Some(next) => st.writer = next,
                None => {
                    st.writer = NOBODY;
                    st.closed = true;
                }
            }
        } else {
            st.pending_writers.retain(|g| *g != generation);
        }
        let waiting = st.all_waiting();
        drop(st);
        wake(waiting);
    }

    fn reader_ready(
        &self,
        sub: u64,
        generation: u64,
        was_reader: bool,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        let mut st = self.lock();
        let closed = st.closed;
        let Some(s) = st.subscription(sub) else {
            return Poll::Ready(());
        };
        let is_reader = s.reader == generation;
        if is_reader != was_reader || (is_reader && (!s.queue.is_empty() || closed)) {
            return Poll::Ready(());
        }
        s.readers_waiting.push(cx.waker().clone());
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

    /// Apply `f` to subscription `sub` and wake its readers.
    fn with_subscription(&self, sub: u64, f: impl FnOnce(&mut Subscription<T>)) {
        let mut st = self.lock();
        let Some(s) = st.subscription(sub) else {
            return;
        };
        f(s);
        let waiting = std::mem::take(&mut s.readers_waiting);
        drop(st);
        wake(waiting);
    }
}

impl<T> State<T> {
    /// Writer `generation` has not had its turn yet.
    fn waiting_writer(&self, generation: u64) -> bool {
        self.pending_writers.contains(&generation) || self.standby_writers.contains(&generation)
    }

    fn subscription(&mut self, id: u64) -> Option<&mut Subscription<T>> {
        self.subscriptions.iter_mut().find(|s| s.id == id)
    }

    /// Everybody waiting, readers and writers.
    fn all_waiting(&mut self) -> Vec<Waker> {
        let mut waiting = std::mem::take(&mut self.writers_waiting);
        for sub in &mut self.subscriptions {
            gather(&mut waiting, &mut sub.readers_waiting);
        }
        waiting
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
    /// A new subscription, which queues what is written from now on unless
    /// `parked`.
    fn subscribe(&self, parked: bool) -> u64;
    /// Remove subscription `sub`; its reader ends.
    fn unsubscribe(&self, sub: u64);
    /// Make `generation` the only reader of `sub`. Wakes the old and the new
    /// one.
    fn set_reader(&self, sub: u64, generation: u64);
    /// Make `generation` the only reader of `sub`, on an empty queue:
    /// [`set_reader`](Pipe::set_reader) for [`Hold::Discard`], in one step.
    fn restart_reader(&self, sub: u64, generation: u64);
    /// Stop queuing items for `sub`; with [`Hold::Discard`], also drop the
    /// queued ones.
    fn park(&self, sub: u64, hold: Hold);
    /// Queue items for `sub` again.
    fn unpark(&self, sub: u64);
    /// Unpark `sub` and park every other subscription, at once: the next
    /// item goes to `sub` only. `hold` is for the others; `restart` drops
    /// what `sub` holds.
    fn select(&self, sub: u64, hold: Hold, restart: bool);
    /// Put writer `generation` on standby: it waits until committed.
    fn standby_writer(&self, generation: u64);
    /// Let standby writer `generation` write once the writers before it have
    /// finished (at once if there are none). From now on its end is the
    /// stream's end.
    fn commit_writer(&self, generation: u64);
    /// Drop standby writer `generation`, without ending the stream.
    fn withdraw_writer(&self, generation: u64);
    /// Drop what writer `generation` holds when its turn comes (its first
    /// write).
    fn drop_held(&self, generation: u64);
    /// State of subscription `sub`, or of all of them.
    fn stats(&self, sub: Option<u64>) -> Option<ChannelStats>;
    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

/// State of a link between flowgraphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelStats {
    /// Items waiting (for an output: summed over its links).
    pub queued: usize,
    /// Items dropped because the queue was full (summed likewise).
    pub dropped: u64,
    /// The stream ended.
    pub closed: bool,
    /// The link gets no items (for an output: all its links).
    pub parked: bool,
}

impl<T: CpuSample> Pipe for Channel<T> {
    fn item(&self) -> ItemType {
        self.item
    }

    fn subscribe(&self, parked: bool) -> u64 {
        let id = next_generation();
        self.lock()
            .subscriptions
            .push(Subscription::new(id, parked));
        id
    }

    fn unsubscribe(&self, sub: u64) {
        let mut st = self.lock();
        let Some(at) = st.subscriptions.iter().position(|s| s.id == sub) else {
            return;
        };
        let removed = st.subscriptions.swap_remove(at);
        drop(st);
        wake(removed.readers_waiting);
    }

    fn set_reader(&self, sub: u64, generation: u64) {
        self.with_subscription(sub, |s| s.reader = generation);
    }

    fn restart_reader(&self, sub: u64, generation: u64) {
        self.with_subscription(sub, |s| {
            s.reader = generation;
            s.clear();
        });
    }

    fn park(&self, sub: u64, hold: Hold) {
        self.with_subscription(sub, |s| {
            s.parked = true;
            if hold == Hold::Discard {
                s.clear();
            }
        });
    }

    fn unpark(&self, sub: u64) {
        self.with_subscription(sub, |s| s.parked = false);
    }

    fn select(&self, sub: u64, hold: Hold, restart: bool) {
        let mut st = self.lock();
        let mut waiting = Vec::new();
        for s in &mut st.subscriptions {
            s.parked = s.id != sub;
            if (s.parked && hold == Hold::Discard) || (!s.parked && restart) {
                s.clear();
            }
            gather(&mut waiting, &mut s.readers_waiting);
        }
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
            st.all_waiting()
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

    fn drop_held(&self, generation: u64) {
        self.lock().drop_held.push(generation);
    }

    fn stats(&self, sub: Option<u64>) -> Option<ChannelStats> {
        let st = self.lock();
        let mut stats = ChannelStats {
            queued: 0,
            dropped: 0,
            closed: st.closed,
            parked: true,
        };
        let mut found = false;
        for s in st
            .subscriptions
            .iter()
            .filter(|s| sub.is_none_or(|id| id == s.id))
        {
            found = true;
            stats.queued += s.queue.len();
            stats.dropped += s.dropped;
            stats.parked &= s.parked;
        }
        stats.parked &= found;
        (found || sub.is_none()).then_some(stats)
    }

    fn as_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

/// Resolves when the reader state of a subscription may have changed.
pub(crate) struct ReaderReady<T> {
    channel: Arc<Channel<T>>,
    sub: u64,
    generation: u64,
    was_reader: bool,
}

impl<T: CpuSample> Future for ReaderReady<T> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.channel
            .reader_ready(self.sub, self.generation, self.was_reader, cx)
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

/// Feeds a flowgraph input from a subscription.
#[derive(Block)]
pub(crate) struct BridgeSource<T: CpuSample> {
    #[output]
    output: ReuseCpuWriter<T>,
    channel: Arc<Channel<T>>,
    sub: u64,
    generation: u64,
    active: bool,
    wait: Option<ReaderReady<T>>,
    /// Tags of the items read, to add to the output.
    tags: Vec<ItemTag>,
}

impl<T: CpuSample> BridgeSource<T> {
    pub(crate) fn new(channel: Arc<Channel<T>>, sub: u64, generation: u64) -> Self {
        Self {
            output: ReuseCpuWriter::default(),
            channel,
            sub,
            generation,
            active: false,
            wait: None,
            tags: Vec::new(),
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
        let (out, mut out_tags) = self.output.slice_with_tags();
        if out.is_empty() {
            self.wait = None;
            return Ok(());
        }
        match self.channel.read_tagged(
            self.sub,
            self.generation,
            out,
            &mut self.tags,
            &mut self.active,
        ) {
            Read::Items(n) => {
                for t in self.tags.drain(..) {
                    out_tags.add_tag(t.index, t.tag);
                }
                self.output.produce(n);
                self.wait = None;
                io.call_again = true;
            }
            Read::Empty => {
                self.wait = Some(ReaderReady {
                    channel: self.channel.clone(),
                    sub: self.sub,
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
    input: ReuseCpuReader<T>,
    channel: Arc<Channel<T>>,
    generation: u64,
    wait: Option<WriterReady<T>>,
}

impl<T: CpuSample> BridgeSink<T> {
    pub(crate) fn new(channel: Arc<Channel<T>>, generation: u64) -> Self {
        Self {
            input: ReuseCpuReader::default(),
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
        let (items, tags) = self.input.slice_with_tags();
        let n = items.len();
        match self.channel.write_tagged(self.generation, items, tags) {
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

    fn read(ch: &Channel<u8>, sub: u64, generation: u64, active: &mut bool) -> (Read, Vec<u8>) {
        let mut out = [0; 16];
        let r = ch.read(sub, generation, &mut out, active);
        let n = if let Read::Items(n) = r { n } else { 0 };
        (r, out[..n].to_vec())
    }

    fn stats(ch: &Channel<u8>) -> ChannelStats {
        ch.stats(None).unwrap()
    }

    /// Items, each tagged with its own value, in `n`-item reads: the
    /// tags must come out on the items they were put on.
    fn read_tagged(ch: &Channel<u8>, sub: u64, generation: u64, n: usize) -> Vec<(u8, u64)> {
        let mut active = false;
        let mut got = Vec::new();
        let mut out = vec![0; n];
        let mut tags = Vec::new();
        while let Read::Items(k) = ch.read_tagged(sub, generation, &mut out, &mut tags, &mut active)
        {
            for t in &tags {
                let Tag::Id(id) = t.tag else { panic!() };
                assert!(t.index < k);
                got.push((out[t.index], id));
            }
        }
        got
    }

    fn tagged(items: &[u8]) -> Vec<ItemTag> {
        items
            .iter()
            .enumerate()
            .filter(|(_, v)| *v % 3 == 0)
            .map(|(k, v)| ItemTag {
                index: k,
                tag: Tag::Id(*v as u64),
            })
            .collect()
    }

    #[test]
    fn tags_stay_on_their_items() {
        let ch = Channel::<u8>::new(ItemType::U8, 8);
        let s = ch.subscribe(false);
        writer(&ch, 1);
        ch.set_reader(s, 2);
        // Across writes and reads of other sizes.
        let items: Vec<u8> = (1..=7).collect();
        let _ = ch.write_tagged(1, &items, &tagged(&items));
        assert_eq!(read_tagged(&ch, s, 2, 2), [(3, 3), (6, 6)]);
        // The queue full: the oldest items go, with their tags.
        let items: Vec<u8> = (10..=21).collect();
        let _ = ch.write_tagged(1, &items[..6], &tagged(&items[..6]));
        let _ = ch.write_tagged(1, &items[6..], &tagged(&items[6..]));
        assert_eq!(read_tagged(&ch, s, 2, 3), [(15, 15), (18, 18), (21, 21)]);
        // Dropped items take their tags along.
        let items: Vec<u8> = (30..=33).collect();
        let _ = ch.write_tagged(1, &items, &tagged(&items));
        ch.restart_reader(s, 3);
        let items: Vec<u8> = (40..=42).collect();
        let _ = ch.write_tagged(1, &items, &tagged(&items));
        assert_eq!(read_tagged(&ch, s, 3, 16), [(42, 42)]);
    }

    #[test]
    fn full_queue_drops_the_oldest() {
        let ch = Channel::<u8>::new(ItemType::U8, 4);
        let s = ch.subscribe(false);
        writer(&ch, 1);
        ch.set_reader(s, 2);
        let _ = ch.write(1, &[1, 2, 3]);
        let _ = ch.write(1, &[4, 5, 6]);
        let mut active = false;
        assert_eq!(read(&ch, s, 2, &mut active).1, [3, 4, 5, 6]);
        assert_eq!(stats(&ch).dropped, 2);
        // more than the capacity at once
        let _ = ch.write(1, &[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(read(&ch, s, 2, &mut active).1, [4, 5, 6, 7]);
    }

    #[test]
    fn reader_switch_is_exact() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        let s = ch.subscribe(false);
        writer(&ch, 1);
        ch.set_reader(s, 2);
        let _ = ch.write(1, &[1, 2, 3]);
        let (mut old, mut new) = (false, false);
        let mut out = [0; 2];
        assert!(matches!(ch.read(s, 2, &mut out, &mut old), Read::Items(2)));
        assert!(
            matches!(ch.read(s, 3, &mut out, &mut new), Read::Empty),
            "not its turn yet"
        );
        ch.set_reader(s, 3);
        assert!(
            matches!(ch.read(s, 2, &mut out, &mut old), Read::End),
            "the old reader is done"
        );
        assert!(matches!(ch.read(s, 3, &mut out, &mut new), Read::Items(1)));
        assert_eq!(out[0], 3);
    }

    #[test]
    fn a_restarted_reader_starts_on_an_empty_queue() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        let s = ch.subscribe(false);
        writer(&ch, 1);
        ch.set_reader(s, 2);
        let _ = ch.write(1, &[1, 2, 3]);
        ch.restart_reader(s, 3);
        let _ = ch.write(1, &[4]);
        let (mut old, mut new) = (true, false);
        assert!(matches!(read(&ch, s, 2, &mut old).0, Read::End));
        assert_eq!(read(&ch, s, 3, &mut new).1, [4]);
    }

    #[test]
    fn writers_take_turns_and_the_last_one_ends_the_stream() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        let s = ch.subscribe(false);
        ch.set_reader(s, 9);
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
        assert_eq!(read(&ch, s, 9, &mut active).1, [10, 21]);
        assert!(matches!(read(&ch, s, 9, &mut active).0, Read::End));
    }

    #[test]
    fn standby_writers_wait_and_take_their_turn_when_committed() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        let s = ch.subscribe(false);
        ch.set_reader(s, 9);
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
        assert_eq!(read(&ch, s, 9, &mut active).1, [10, 40, 20]);
        assert!(!stats(&ch).closed);
    }

    #[test]
    fn a_writer_ends_the_stream_only_once_committed() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        let s = ch.subscribe(false);
        ch.set_reader(s, 9);
        let mut active = false;

        // Its flowgraph never goes live: withdrawn, the stream goes on.
        ch.standby_writer(1);
        ch.writer_finished(1);
        ch.withdraw_writer(1);
        assert!(!stats(&ch).closed);

        // Its flowgraph finished on standby: the stream ends on commit.
        ch.standby_writer(2);
        ch.writer_finished(2);
        assert!(!stats(&ch).closed, "not committed yet");
        ch.commit_writer(2);
        assert!(stats(&ch).closed);
        assert!(matches!(read(&ch, s, 9, &mut active).0, Read::End));

        // Finished on standby behind a running writer: the running writer's
        // end ends the stream.
        writer(&ch, 3);
        assert!(!stats(&ch).closed, "a new writer reopens the stream");
        ch.standby_writer(4);
        ch.writer_finished(4);
        ch.commit_writer(4);
        assert!(!stats(&ch).closed);
        ch.writer_finished(3);
        assert!(stats(&ch).closed);
    }

    #[test]
    fn a_writer_can_drop_what_it_held() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        let s = ch.subscribe(false);
        ch.set_reader(s, 9);
        writer(&ch, 1);
        ch.standby_writer(2);
        ch.drop_held(2);
        let _ = ch.write(1, &[1]);
        ch.commit_writer(2);
        assert!(matches!(ch.write(2, &[2]), Write::Wait), "still 1's turn");
        ch.writer_finished(1);
        assert!(matches!(ch.write(2, &[2]), Write::Taken), "dropped");
        let _ = ch.write(2, &[3]);
        assert_eq!(read(&ch, s, 9, &mut false).1, [1, 3]);
        // A writer that finishes forgets it.
        ch.standby_writer(3);
        ch.drop_held(3);
        ch.writer_finished(3);
        ch.withdraw_writer(3);
    }

    #[test]
    fn every_subscription_gets_the_stream() {
        let ch = Channel::<u8>::new(ItemType::U8, 4);
        let (a, b) = (ch.subscribe(false), ch.subscribe(false));
        writer(&ch, 1);
        ch.set_reader(a, 10);
        ch.set_reader(b, 20);
        let _ = ch.write(1, &[1, 2, 3]);
        let (mut ra, mut rb) = (false, false);
        assert_eq!(read(&ch, a, 10, &mut ra).1, [1, 2, 3]);
        let _ = ch.write(1, &[4, 5]);
        assert_eq!(read(&ch, a, 10, &mut ra).1, [4, 5]);
        assert_eq!(read(&ch, b, 20, &mut rb).1, [2, 3, 4, 5], "b fell behind");
        assert!(
            matches!(read(&ch, a, 20, &mut rb).0, Read::End),
            "not a's reader"
        );
        let total = stats(&ch);
        assert_eq!((total.queued, total.dropped, total.parked), (0, 1, false));
        assert_eq!(ch.stats(Some(b)).unwrap().dropped, 1);
        assert!(ch.stats(Some(99)).is_none());

        // A reader whose subscription goes away ends.
        let _ = ch.write(1, &[6]);
        ch.unsubscribe(b);
        ch.unsubscribe(b);
        assert!(matches!(read(&ch, b, 20, &mut false).0, Read::End));
        assert_eq!(stats(&ch).queued, 1);
    }

    #[test]
    fn parked_subscriptions_get_nothing() {
        let ch = Channel::<u8>::new(ItemType::U8, 16);
        let (a, b) = (ch.subscribe(false), ch.subscribe(true));
        writer(&ch, 1);
        ch.set_reader(a, 10);
        ch.set_reader(b, 20);
        let (mut ra, mut rb) = (false, false);
        let _ = ch.write(1, &[1, 2]);
        assert_eq!(read(&ch, b, 20, &mut rb).1, [] as [u8; 0]);
        assert!(ch.stats(Some(b)).unwrap().parked);

        // Parked with Keep, a keeps what it has; b goes on from now.
        ch.park(a, Hold::Keep);
        ch.unpark(b);
        let _ = ch.write(1, &[3]);
        assert_eq!(read(&ch, a, 10, &mut ra).1, [1, 2]);
        assert_eq!(read(&ch, b, 20, &mut rb).1, [3]);

        // Selecting a parks b, which drops what it has with Discard.
        let _ = ch.write(1, &[4]);
        ch.select(a, Hold::Discard, false);
        let _ = ch.write(1, &[5]);
        assert_eq!(read(&ch, a, 10, &mut ra).1, [5]);
        assert!(matches!(read(&ch, b, 20, &mut rb).0, Read::Empty));
        let _ = ch.write(1, &[6]);
        ch.select(b, Hold::Keep, false);
        let _ = ch.write(1, &[7]);
        assert_eq!(read(&ch, a, 10, &mut ra).1, [6]);
        assert_eq!(read(&ch, b, 20, &mut rb).1, [7]);
        // Restarted, the selected one drops what it held.
        let _ = ch.write(1, &[8]);
        ch.park(b, Hold::Keep);
        ch.select(b, Hold::Keep, true);
        let _ = ch.write(1, &[9]);
        assert_eq!(read(&ch, b, 20, &mut rb).1, [9]);
        ch.park(b, Hold::Keep);
        assert!(stats(&ch).parked, "all links parked");
        // Parking does not end the stream.
        ch.writer_finished(1);
        assert!(matches!(read(&ch, b, 20, &mut rb).0, Read::End));
        // Unknown subscriptions are ignored.
        ch.park(99, Hold::Discard);
        ch.set_reader(99, 1);
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
    struct Sub {
        queue: VecDeque<i32>,
        reader: u64,
        parked: bool,
        dropped: u64,
    }

    #[derive(Default)]
    struct Model {
        capacity: usize,
        writers: BTreeMap<u64, W>,
        /// Committed writers that have not finished, in commit order; the
        /// first one writes.
        turns: VecDeque<u64>,
        /// By subscription id; removed ones are gone.
        subs: BTreeMap<u64, Sub>,
        drop_held: Vec<u64>,
        closed: bool,
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
            self.drop_held.retain(|d| *d != g);
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
            if items.is_empty() {
                return true;
            }
            if self.drop_held.contains(&g) {
                self.drop_held.retain(|d| *d != g);
                return true;
            }
            for sub in self.subs.values_mut().filter(|s| !s.parked) {
                sub.queue.extend(items);
                while sub.queue.len() > self.capacity {
                    sub.queue.pop_front();
                    sub.dropped += 1;
                }
            }
            true
        }

        fn read(&mut self, s: u64, r: u64, max: usize, active: &mut bool) -> Outcome {
            let closed = self.closed;
            let Some(sub) = self.subs.get_mut(&s) else {
                return Outcome::End;
            };
            if sub.reader != r {
                return if *active {
                    Outcome::End
                } else {
                    Outcome::Empty
                };
            }
            *active = true;
            if sub.queue.is_empty() {
                return if closed { Outcome::End } else { Outcome::Empty };
            }
            let n = max.min(sub.queue.len());
            Outcome::Items(sub.queue.drain(..n).collect())
        }

        fn reader_ready(&self, s: u64, r: u64, was_reader: bool) -> bool {
            let Some(sub) = self.subs.get(&s) else {
                return true;
            };
            let is_reader = sub.reader == r;
            is_reader != was_reader || (is_reader && (!sub.queue.is_empty() || self.closed))
        }

        fn stats(&self, s: Option<u64>) -> Option<(usize, u64, bool, bool)> {
            let subs: Vec<&Sub> = match s {
                None => self.subs.values().collect(),
                Some(s) => vec![self.subs.get(&s)?],
            };
            Some((
                subs.iter().map(|s| s.queue.len()).sum(),
                subs.iter().map(|s| s.dropped).sum(),
                self.closed,
                !subs.is_empty() && subs.iter().all(|s| s.parked),
            ))
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Op {
        Standby(u64),
        Commit(u64),
        Withdraw(u64),
        Finished(u64),
        Write(u64, usize),
        Subscribe(bool),
        Unsubscribe(u64),
        SetReader(u64, u64),
        RestartReader(u64, u64),
        Park(u64, Hold),
        Unpark(u64),
        Select(u64, Hold, bool),
        DropHeld(u64),
        Read(u64, u64, usize),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum Waiter {
        Writer(u64),
        Reader(u64, u64, bool),
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
            Waiter::Reader(s, r, was) => ch.reader_ready(s, r, was, &mut cx),
        };
        ready.is_ready()
    }

    const READERS: [u64; 3] = [100, 101, 102];

    fn op(
        rng: &mut Rng,
        next_gen: &mut u64,
        writers: &[u64],
        finished: &[u64],
        subs: &[u64],
    ) -> Op {
        let live: Vec<u64> = writers
            .iter()
            .copied()
            .filter(|g| !finished.contains(g))
            .collect();
        let any = |rng: &mut Rng| *rng.pick(writers);
        let reader = |rng: &mut Rng| *rng.pick(&READERS);
        let hold = |rng: &mut Rng| *rng.pick(&[Hold::Keep, Hold::Discard]);
        match rng.below(100) {
            _ if writers.is_empty() => {
                *next_gen += 1;
                Op::Standby(*next_gen)
            }
            _ if subs.is_empty() => Op::Subscribe(rng.chance(30)),
            0..8 => {
                *next_gen += 1;
                Op::Standby(*next_gen)
            }
            8..20 => Op::Commit(any(rng)),
            20..24 => Op::Withdraw(any(rng)),
            24..32 => Op::Finished(any(rng)),
            32..55 if !live.is_empty() => Op::Write(*rng.pick(&live), rng.range(0, 6)),
            55..58 => Op::Subscribe(rng.chance(30)),
            58..60 => Op::Unsubscribe(*rng.pick(subs)),
            60..65 => Op::SetReader(*rng.pick(subs), reader(rng)),
            65..67 => Op::RestartReader(*rng.pick(subs), reader(rng)),
            67..70 => Op::Park(*rng.pick(subs), hold(rng)),
            70..73 => Op::Unpark(*rng.pick(subs)),
            73..75 => Op::Select(*rng.pick(subs), hold(rng), rng.chance(30)),
            75..77 => Op::DropHeld(any(rng)),
            _ => Op::Read(*rng.pick(subs), reader(rng), rng.range(1, 8)),
        }
    }

    #[test]
    fn channel_matches_its_model() {
        for_each_seed(300, |rng| {
            let capacity = rng.range(1, 12);
            let ch = Channel::<i32>::new(ItemType::I32, capacity);
            let mut model = Model {
                capacity,
                ..Model::default()
            };
            let (mut next_gen, mut next_item) = (0u64, 0i32);
            let (mut writers, mut finished) = (Vec::new(), Vec::new());
            // Every subscription ever made; removed ones stay here.
            let mut subs: Vec<u64> = Vec::new();
            // Items a writer still has to write after being told to wait.
            let mut unsent: HashMap<u64, Vec<i32>> = HashMap::new();
            let mut active: HashMap<(u64, u64), bool> = HashMap::new();
            let mut waiting: Vec<(Waiter, Arc<Flag>)> = Vec::new();

            for step in 0..200 {
                let op = op(rng, &mut next_gen, &writers, &finished, &subs);
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
                    Op::Subscribe(parked) => {
                        let s = ch.subscribe(parked);
                        subs.push(s);
                        model.subs.insert(
                            s,
                            Sub {
                                reader: NOBODY,
                                parked,
                                ..Sub::default()
                            },
                        );
                    }
                    Op::Unsubscribe(s) => {
                        model.subs.remove(&s);
                        ch.unsubscribe(s);
                    }
                    Op::SetReader(s, r) => {
                        if let Some(sub) = model.subs.get_mut(&s) {
                            sub.reader = r;
                        }
                        ch.set_reader(s, r);
                    }
                    Op::RestartReader(s, r) => {
                        if let Some(sub) = model.subs.get_mut(&s) {
                            sub.reader = r;
                            sub.queue.clear();
                        }
                        ch.restart_reader(s, r);
                    }
                    Op::Park(s, hold) => {
                        if let Some(sub) = model.subs.get_mut(&s) {
                            sub.parked = true;
                            if hold == Hold::Discard {
                                sub.queue.clear();
                            }
                        }
                        ch.park(s, hold);
                    }
                    Op::Unpark(s) => {
                        if let Some(sub) = model.subs.get_mut(&s) {
                            sub.parked = false;
                        }
                        ch.unpark(s);
                    }
                    Op::Select(s, hold, restart) => {
                        for (id, sub) in model.subs.iter_mut() {
                            sub.parked = *id != s;
                            if (sub.parked && hold == Hold::Discard) || (!sub.parked && restart) {
                                sub.queue.clear();
                            }
                        }
                        ch.select(s, hold, restart);
                    }
                    Op::DropHeld(g) => {
                        model.drop_held.push(g);
                        ch.drop_held(g);
                    }
                    Op::Read(s, r, max) => {
                        let key = (s, r);
                        let mut model_active = *active.get(&key).unwrap_or(&false);
                        let expected = model.read(s, r, max, &mut model_active);
                        let real_active = active.entry(key).or_default();
                        let mut out = vec![0; max];
                        let got = match ch.read(s, r, &mut out, real_active) {
                            Read::Items(n) => Outcome::Items(out[..n].to_vec()),
                            Read::Empty => Outcome::Empty,
                            Read::End => Outcome::End,
                        };
                        assert_eq!(got, expected, "{at}");
                        if expected != Outcome::End || model.subs.contains_key(&s) {
                            assert_eq!(*real_active, model_active, "{at}");
                        }
                    }
                }
                for which in subs.iter().map(|s| Some(*s)).chain([None]) {
                    let stats = ch
                        .stats(which)
                        .map(|s| (s.queued, s.dropped, s.closed, s.parked));
                    assert_eq!(stats, model.stats(which), "{at}: stats of {which:?}");
                }

                // Whoever waited and may go on now must have been woken.
                for (waiter, flag) in waiting.drain(..) {
                    let ready = match waiter {
                        Waiter::Writer(g) => !model.waiting(g),
                        Waiter::Reader(s, r, was) => model.reader_ready(s, r, was),
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
                for &s in &subs {
                    for r in READERS {
                        let was = active.get(&(s, r)) == Some(&true);
                        waiters.push(Waiter::Reader(s, r, was));
                    }
                }
                for waiter in waiters {
                    let flag = Arc::new(Flag(AtomicBool::new(false)));
                    let expected = match waiter {
                        Waiter::Writer(g) => !model.waiting(g),
                        Waiter::Reader(s, r, was) => model.reader_ready(s, r, was),
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
/// committed writer's items arrive, in commit order, at every subscription,
/// a withdrawn writer's never do, and nobody hangs.
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
            let (withdrawn, ended) = (50, 51);
            let subs: Vec<(u64, u64)> = (0..rng.range(1, 3))
                .map(|i| (ch.subscribe(false), 100 + i as u64))
                .collect();
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

            let mut readers = Vec::new();
            for &(sub, reader) in &subs {
                let (done_tx, done_rx) = mpsc::channel();
                let reads = ch.clone();
                let thread = std::thread::spawn(move || {
                    let (mut got, mut active, mut out) = (Vec::<i32>::new(), false, [0; 97]);
                    loop {
                        match reads.read(sub, reader, &mut out, &mut active) {
                            Read::Items(n) => got.extend(&out[..n]),
                            Read::Empty => block_on(ReaderReady {
                                channel: reads.clone(),
                                sub,
                                generation: reader,
                                was_reader: active,
                            }),
                            Read::End => break,
                        }
                    }
                    done_tx.send(got).unwrap();
                });
                readers.push((thread, done_rx));
            }

            // Commit in order, with pauses; a writer may finish once the next
            // one that writes is committed (make before break).
            let reader_at = rng.below(order.len());
            let withdraw_at = rng.below(order.len());
            let mut previous: Option<u64> = None;
            for (i, &g) in order.iter().enumerate() {
                if i == reader_at {
                    for &(sub, reader) in &subs {
                        ch.set_reader(sub, reader);
                    }
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

            for (thread, done_rx) in readers {
                let got = done_rx
                    .recv_timeout(Duration::from_secs(20))
                    .expect("a reader hangs");
                thread.join().unwrap();
                assert!(
                    got == expected,
                    "{} items, expected {}",
                    got.len(),
                    expected.len()
                );
            }
            for t in threads {
                t.join().unwrap();
            }
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

    use futuresdr::num_complex::Complex32;
    use futuresdr::runtime::BlockRef;
    use futuresdr::runtime::Flowgraph;
    use futuresdr::runtime::Runtime;
    use futuresdr::runtime::block_on;

    use super::*;

    /// FutureSDR's blocks, on the buffer the bridges use.
    type NullSink<T> = futuresdr::blocks::NullSink<T, ReuseCpuReader<T>>;
    type NullSource<T> = futuresdr::blocks::NullSource<T, ReuseCpuWriter<T>>;

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
        let sub = channel.subscribe(false);
        let (writer, reader) = (next_generation(), next_generation());
        let mut up = Flowgraph::new();
        let src = up.add(NullSource::<T>::new()).unwrap();
        let bsnk = up.add(BridgeSink::new(channel.clone(), writer)).unwrap();
        up.stream_dyn(src.id(), "output", bsnk.id(), "input")
            .unwrap();
        let mut down = Flowgraph::new();
        let bsrc = down
            .add(BridgeSource::new(channel.clone(), sub, reader))
            .unwrap();
        let snk = down.add(NullSink::<T>::new()).unwrap();
        down.stream_dyn(bsrc.id(), "output", snk.id(), "input")
            .unwrap();
        channel.standby_writer(writer);
        channel.commit_writer(writer);
        channel.set_reader(sub, reader);
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
