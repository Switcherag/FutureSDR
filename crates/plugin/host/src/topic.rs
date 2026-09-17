//! Messages between flowgraphs.
//!
//! A [`Topic`] carries what the blocks bound to one flowgraph message output
//! post, to the message inputs linked to it and to the application's
//! [`Tap`]s, each through a subscription with a queue of its own. As for
//! streams, a subscription has one reader at a time, and moving it to another
//! flowgraph is one switch.
//!
//! Every flowgraph that ran under the output's name may publish: a replaced
//! flowgraph's last messages still arrive. A flowgraph on standby publishes
//! once it is committed.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use futuresdr::runtime::dev::prelude::*;

use crate::Hold;
use crate::bridge::NOBODY;
use crate::bridge::next_generation;

/// Messages a subscription or a standby publisher holds at most; the
/// oldest go first.
pub(crate) const MESSAGE_CAPACITY: usize = 4096;

struct Subscription {
    id: u64,
    /// A tap: selecting a link does not park it.
    tap: bool,
    queue: VecDeque<Pmt>,
    reader: u64,
    parked: bool,
    dropped: u64,
    waiting: Vec<Waker>,
}

#[derive(Default)]
struct State {
    subscriptions: Vec<Subscription>,
    /// Publishers on standby, with what they posted.
    standby: Vec<(u64, VecDeque<Pmt>)>,
    /// Standby publishers that were dropped; they publish nothing.
    withdrawn: Vec<u64>,
    closed: bool,
}

impl State {
    fn subscription(&mut self, id: u64) -> Option<&mut Subscription> {
        self.subscriptions.iter_mut().find(|s| s.id == id)
    }

    fn deliver(&mut self, messages: impl IntoIterator<Item = Pmt>) -> Vec<Waker> {
        let mut waiting = Vec::new();
        for pmt in messages {
            let mut rest = self.subscriptions.iter_mut().filter(|s| !s.parked);
            let Some(mut sub) = rest.next() else {
                return waiting;
            };
            // The last subscription takes the message itself.
            for next in rest {
                push(sub, pmt.clone());
                waiting.append(&mut sub.waiting);
                sub = next;
            }
            push(sub, pmt);
            waiting.append(&mut sub.waiting);
        }
        waiting
    }
}

fn push(sub: &mut Subscription, pmt: Pmt) {
    if sub.queue.len() == MESSAGE_CAPACITY {
        sub.queue.pop_front();
        sub.dropped += 1;
    }
    sub.queue.push_back(pmt);
}

fn wake(wakers: Vec<Waker>) {
    wakers.into_iter().for_each(Waker::wake);
}

pub(crate) enum Take {
    Message(Pmt),
    Empty,
    End,
}

/// The queues between a flowgraph message output and its subscribers.
#[derive(Default)]
pub(crate) struct Topic {
    state: Mutex<State>,
}

impl Topic {
    pub(crate) fn new() -> Arc<Self> {
        Arc::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Publisher `generation` posted `pmt`.
    pub(crate) fn publish(&self, generation: u64, pmt: Pmt) {
        let mut st = self.lock();
        if st.withdrawn.contains(&generation) {
            return;
        }
        if let Some((_, held)) = st.standby.iter_mut().find(|(g, _)| *g == generation) {
            if held.len() == MESSAGE_CAPACITY {
                held.pop_front();
            }
            held.push_back(pmt);
            return;
        }
        let waiting = st.deliver([pmt]);
        drop(st);
        wake(waiting);
    }

    /// Hold what publisher `generation` posts until it is committed.
    pub(crate) fn standby_publisher(&self, generation: u64) {
        self.lock().standby.push((generation, VecDeque::new()));
    }

    /// Deliver what standby publisher `generation` posted, and from now on
    /// what it posts.
    pub(crate) fn commit_publisher(&self, generation: u64) {
        let mut st = self.lock();
        let Some(at) = st.standby.iter().position(|(g, _)| *g == generation) else {
            return;
        };
        let (_, held) = st.standby.swap_remove(at);
        let waiting = st.deliver(held);
        drop(st);
        wake(waiting);
    }

    /// Drop what standby publisher `generation` posted, and what it posts.
    pub(crate) fn withdraw_publisher(&self, generation: u64) {
        let mut st = self.lock();
        let len = st.standby.len();
        st.standby.retain(|(g, _)| *g != generation);
        if st.standby.len() != len {
            st.withdrawn.push(generation);
        }
    }

    /// Publisher `generation` is gone.
    pub(crate) fn publisher_finished(&self, generation: u64) {
        let mut st = self.lock();
        st.withdrawn.retain(|g| *g != generation);
    }

    /// A new subscription; see [`Pipe::subscribe`](crate::bridge::Pipe::subscribe).
    pub(crate) fn subscribe(&self, parked: bool, tap: bool) -> u64 {
        let id = next_generation();
        self.lock().subscriptions.push(Subscription {
            id,
            tap,
            queue: VecDeque::new(),
            reader: if tap { id } else { NOBODY },
            parked,
            dropped: 0,
            waiting: Vec::new(),
        });
        id
    }

    pub(crate) fn unsubscribe(&self, sub: u64) {
        let mut st = self.lock();
        if let Some(at) = st.subscriptions.iter().position(|s| s.id == sub) {
            let removed = st.subscriptions.swap_remove(at);
            drop(st);
            wake(removed.waiting);
        }
    }

    fn with_subscription(&self, sub: u64, f: impl FnOnce(&mut Subscription)) {
        let mut st = self.lock();
        if let Some(s) = st.subscription(sub) {
            f(s);
            let waiting = std::mem::take(&mut s.waiting);
            drop(st);
            wake(waiting);
        }
    }

    pub(crate) fn set_reader(&self, sub: u64, generation: u64) {
        self.with_subscription(sub, |s| s.reader = generation);
    }

    pub(crate) fn restart_reader(&self, sub: u64, generation: u64) {
        self.with_subscription(sub, |s| {
            s.reader = generation;
            s.queue.clear();
        });
    }

    pub(crate) fn park(&self, sub: u64, hold: Hold) {
        self.with_subscription(sub, |s| {
            s.parked = true;
            if hold == Hold::Discard {
                s.queue.clear();
            }
        });
    }

    pub(crate) fn unpark(&self, sub: u64) {
        self.with_subscription(sub, |s| s.parked = false);
    }

    /// Unpark `sub` and park the other links (not the taps).
    pub(crate) fn select(&self, sub: u64, hold: Hold) {
        let mut st = self.lock();
        let mut waiting = Vec::new();
        for s in st.subscriptions.iter_mut().filter(|s| !s.tap) {
            s.parked = s.id != sub;
            if s.parked && hold == Hold::Discard {
                s.queue.clear();
            }
            waiting.append(&mut s.waiting);
        }
        drop(st);
        wake(waiting);
    }

    /// No more messages: readers end once they have taken what is queued.
    pub(crate) fn close(&self) {
        let mut st = self.lock();
        st.closed = true;
        let mut waiting = Vec::new();
        for s in &mut st.subscriptions {
            waiting.append(&mut s.waiting);
        }
        drop(st);
        wake(waiting);
    }

    /// Take the next message of `sub` for reader `generation`; `active` as
    /// for streams.
    pub(crate) fn take(&self, sub: u64, generation: u64, active: &mut bool) -> Take {
        let mut st = self.lock();
        let closed = st.closed;
        let Some(s) = st.subscription(sub) else {
            return Take::End;
        };
        if s.reader != generation {
            return if *active { Take::End } else { Take::Empty };
        }
        *active = true;
        match s.queue.pop_front() {
            Some(pmt) => Take::Message(pmt),
            None if closed => Take::End,
            None => Take::Empty,
        }
    }

    fn ready(&self, sub: u64, generation: u64, was_reader: bool, cx: &mut Context<'_>) -> Poll<()> {
        let mut st = self.lock();
        let closed = st.closed;
        let Some(s) = st.subscription(sub) else {
            return Poll::Ready(());
        };
        let is_reader = s.reader == generation;
        if is_reader != was_reader || (is_reader && (!s.queue.is_empty() || closed)) {
            return Poll::Ready(());
        }
        s.waiting.push(cx.waker().clone());
        Poll::Pending
    }

    /// Messages queued and dropped by `sub`.
    pub(crate) fn stats(&self, sub: u64) -> Option<(usize, u64, bool)> {
        let mut st = self.lock();
        let s = st.subscription(sub)?;
        Some((s.queue.len(), s.dropped, s.parked))
    }
}

/// Resolves when a subscription's reader may go on.
pub(crate) struct TopicReady {
    topic: Arc<Topic>,
    sub: u64,
    generation: u64,
    was_reader: bool,
}

impl Future for TopicReady {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.topic
            .ready(self.sub, self.generation, self.was_reader, cx)
    }
}

/// Publishes what the block it is connected to posts. Ends with that block.
#[derive(Block)]
#[message_inputs(r#in)]
pub(crate) struct TopicSink {
    topic: Arc<Topic>,
    generation: u64,
}

impl TopicSink {
    pub(crate) fn new(topic: Arc<Topic>, generation: u64) -> Self {
        Self { topic, generation }
    }

    async fn r#in(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        if matches!(p, Pmt::Finished) {
            io.finished = true;
        } else {
            self.topic.publish(self.generation, p);
        }
        Ok(Pmt::Ok)
    }
}

impl Kernel for TopicSink {
    async fn deinit(&mut self, _mo: &mut MessageOutputs, _meta: &BlockMeta) -> Result<()> {
        self.topic.publisher_finished(self.generation);
        Ok(())
    }
}

/// Posts the messages of a subscription to the block it is connected to.
#[derive(Block)]
#[message_outputs(out)]
pub(crate) struct TopicSource {
    topic: Arc<Topic>,
    sub: u64,
    generation: u64,
    active: bool,
    wait: Option<TopicReady>,
}

impl TopicSource {
    pub(crate) fn new(topic: Arc<Topic>, sub: u64, generation: u64) -> Self {
        Self {
            topic,
            sub,
            generation,
            active: false,
            wait: None,
        }
    }
}

impl Kernel for TopicSource {
    type BlockOn = TopicReady;

    fn block_on(&mut self) -> Option<Pin<&mut TopicReady>> {
        self.wait.as_mut().map(Pin::new)
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        self.wait = None;
        loop {
            match self.topic.take(self.sub, self.generation, &mut self.active) {
                Take::Message(pmt) => mo.post("out", pmt).await?,
                Take::Empty => {
                    self.wait = Some(TopicReady {
                        topic: self.topic.clone(),
                        sub: self.sub,
                        generation: self.generation,
                        was_reader: self.active,
                    });
                    return Ok(());
                }
                Take::End => {
                    io.finished = true;
                    return Ok(());
                }
            }
        }
    }
}

/// The messages a flowgraph output posts, for the application; see
/// [`Controller::tap`](crate::Controller::tap).
///
/// It follows the output across replacements, and ends when the controller
/// is dropped. Dropping it unsubscribes.
pub struct Tap {
    topic: Arc<Topic>,
    sub: u64,
    name: String,
}

impl Tap {
    pub(crate) fn new(topic: Arc<Topic>, name: String) -> Self {
        let sub = topic.subscribe(false, true);
        Self { topic, sub, name }
    }

    /// The output it taps, `"flowgraph.port"`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The next message, or `None` once the controller is gone and every
    /// message was taken.
    pub async fn recv(&mut self) -> Option<Pmt> {
        let mut active = true;
        loop {
            match self.topic.take(self.sub, self.sub, &mut active) {
                Take::Message(pmt) => return Some(pmt),
                Take::End => return None,
                Take::Empty => {
                    TopicReady {
                        topic: self.topic.clone(),
                        sub: self.sub,
                        generation: self.sub,
                        was_reader: true,
                    }
                    .await
                }
            }
        }
    }

    /// The next message, if one is queued.
    pub fn try_recv(&mut self) -> Option<Pmt> {
        match self.topic.take(self.sub, self.sub, &mut true) {
            Take::Message(pmt) => Some(pmt),
            _ => None,
        }
    }

    /// Messages waiting, and messages dropped because too many were
    /// waiting.
    pub fn stats(&self) -> (usize, u64) {
        self.topic
            .stats(self.sub)
            .map_or((0, 0), |(queued, dropped, _)| (queued, dropped))
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.topic.unsubscribe(self.sub);
    }
}

impl std::fmt::Debug for Tap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tap").field("name", &self.name).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take_all(topic: &Topic, sub: u64, reader: u64, active: &mut bool) -> Vec<u64> {
        let mut got = Vec::new();
        while let Take::Message(Pmt::U64(v)) = topic.take(sub, reader, active) {
            got.push(v);
        }
        got
    }

    #[test]
    fn subscriptions_get_what_is_published_and_readers_take_turns() {
        let topic = Topic::new();
        let (a, b) = (topic.subscribe(false, false), topic.subscribe(true, false));
        topic.set_reader(a, 10);
        topic.set_reader(b, 20);
        for v in 0..3 {
            topic.publish(1, Pmt::U64(v));
        }
        let (mut ra, mut rb) = (false, false);
        assert!(
            matches!(topic.take(a, 11, &mut false), Take::Empty),
            "not its turn"
        );
        assert_eq!(take_all(&topic, a, 10, &mut ra), [0, 1, 2]);
        assert!(take_all(&topic, b, 20, &mut rb).is_empty(), "parked");
        assert_eq!(topic.stats(b), Some((0, 0, true)));

        topic.publish(1, Pmt::U64(3));
        topic.set_reader(a, 11);
        assert!(matches!(topic.take(a, 10, &mut ra), Take::End), "replaced");
        assert_eq!(take_all(&topic, a, 11, &mut false), [3]);
        topic.publish(1, Pmt::U64(4));
        topic.restart_reader(a, 12);
        assert!(take_all(&topic, a, 12, &mut false).is_empty(), "restarted");

        topic.unsubscribe(b);
        topic.unsubscribe(b);
        assert!(matches!(topic.take(b, 20, &mut rb), Take::End), "gone");
        assert!(topic.stats(b).is_none());
    }

    #[test]
    fn a_full_queue_drops_the_oldest() {
        let topic = Topic::new();
        let sub = topic.subscribe(false, false);
        topic.set_reader(sub, 1);
        for v in 0..MESSAGE_CAPACITY as u64 + 2 {
            topic.publish(7, Pmt::U64(v));
        }
        assert_eq!(topic.stats(sub), Some((MESSAGE_CAPACITY, 2, false)));
        assert_eq!(take_all(&topic, sub, 1, &mut false)[0], 2);
    }

    #[test]
    fn standby_publishers_wait_for_their_commit() {
        let topic = Topic::new();
        let sub = topic.subscribe(false, false);
        topic.set_reader(sub, 1);
        topic.standby_publisher(5);
        topic.standby_publisher(6);
        for v in 0..MESSAGE_CAPACITY as u64 + 1 {
            topic.publish(5, Pmt::U64(v));
        }
        topic.publish(6, Pmt::U64(99));
        topic.publish(7, Pmt::U64(70));
        assert_eq!(take_all(&topic, sub, 1, &mut false), [70]);
        topic.commit_publisher(5);
        topic.commit_publisher(5);
        let got = take_all(&topic, sub, 1, &mut false);
        assert_eq!(
            (got.len(), got[0]),
            (MESSAGE_CAPACITY, 1),
            "the newest held"
        );
        topic.withdraw_publisher(6);
        topic.publish(6, Pmt::U64(98));
        topic.withdraw_publisher(8);
        topic.publish(8, Pmt::U64(80));
        assert_eq!(
            take_all(&topic, sub, 1, &mut false),
            [80],
            "only the withdrawn is dropped"
        );
        topic.publisher_finished(6);
        topic.publish(6, Pmt::U64(97));
        assert_eq!(take_all(&topic, sub, 1, &mut false), [97]);
    }

    #[test]
    fn selection_parks_links_but_not_taps() {
        let topic = Topic::new();
        let (a, b) = (topic.subscribe(false, false), topic.subscribe(false, false));
        let mut tap = Tap::new(topic.clone(), "fg.port".into());
        topic.set_reader(a, 1);
        topic.set_reader(b, 2);
        topic.publish(9, Pmt::U64(0));
        topic.select(b, Hold::Discard);
        topic.publish(9, Pmt::U64(1));
        assert!(take_all(&topic, a, 1, &mut false).is_empty(), "discarded");
        assert_eq!(take_all(&topic, b, 2, &mut false), [0, 1]);
        topic.park(b, Hold::Keep);
        topic.publish(9, Pmt::U64(2));
        topic.unpark(a);
        topic.publish(9, Pmt::U64(3));
        topic.select(a, Hold::Keep);
        topic.park(a, Hold::Discard);
        topic.publish(9, Pmt::U64(4));
        assert!(take_all(&topic, a, 1, &mut false).is_empty());
        assert!(take_all(&topic, b, 2, &mut false).is_empty());
        let got: Vec<Pmt> = std::iter::from_fn(|| tap.try_recv()).collect();
        assert_eq!(got, (0..5).map(Pmt::U64).collect::<Vec<_>>());
        assert_eq!(tap.stats(), (0, 0));
        assert!(format!("{tap:?}").contains("fg.port"));

        // Closed: what is queued is still taken, then the end.
        topic.publish(9, Pmt::U64(5));
        topic.close();
        assert_eq!(futuresdr::runtime::block_on(tap.recv()), Some(Pmt::U64(5)));
        assert_eq!(futuresdr::runtime::block_on(tap.recv()), None);
        assert!(tap.try_recv().is_none());
        drop(tap);
        assert_eq!(topic.lock().subscriptions.len(), 2, "the tap unsubscribed");
    }
}
