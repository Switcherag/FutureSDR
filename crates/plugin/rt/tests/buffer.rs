//! The buffer whose ring is kept for the next flowgraph.
//!
//! What a buffer owes its blocks — items in order, tags on the items they
//! were put on, and an end — must hold just as well on a ring a previous
//! flowgraph used.

extern crate futuresdr_plugin_rt as futuresdr;

use std::sync::Mutex;
use std::sync::MutexGuard;

use futuresdr::buffer::DEFAULT_POOL_LIMIT;
use futuresdr::buffer::pool_stats;
use futuresdr::buffer::set_pool_limit;
use futuresdr::prelude::*;

/// The pool is process-wide, so its tests take turns, each starting from an
/// empty pool that keeps what it is given.
fn alone() -> MutexGuard<'static, ()> {
    static ALONE: Mutex<()> = Mutex::new(());
    let guard = ALONE.lock().unwrap_or_else(|e| e.into_inner());
    set_pool_limit(0);
    set_pool_limit(DEFAULT_POOL_LIMIT);
    guard
}

/// Copies its input, tagging the first item of every batch with the number
/// of items it had already produced.
#[derive(Block)]
struct Tagger {
    #[input]
    input: ReuseCpuReader<u32>,
    #[output]
    output: ReuseCpuWriter<u32>,
    produced: u64,
}

impl Tagger {
    fn new() -> Self {
        Self {
            input: ReuseCpuReader::default(),
            output: ReuseCpuWriter::default(),
            produced: 0,
        }
    }
}

impl Kernel for Tagger {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let (o, mut tags) = self.output.slice_with_tags();
        let (i_len, n) = (i.len(), i.len().min(o.len()));
        if n > 0 {
            o[..n].copy_from_slice(&i[..n]);
            tags.add_tag(0, Tag::Id(self.produced));
            self.produced += n as u64;
            self.input.consume(n);
            self.output.produce(n);
        }
        if self.input.finished() && n == i_len {
            io.finished = true;
        }
        Ok(())
    }
}

/// Keeps what it was given, the tags with the index of the item they came
/// on, and what the pool held while it ran.
#[derive(Block)]
struct Record {
    #[input]
    input: ReuseCpuReader<u32>,
    items: Vec<u32>,
    tags: Vec<ItemTag>,
    pool: Option<(usize, usize)>,
}

impl Record {
    fn new() -> Self {
        Self {
            input: ReuseCpuReader::default(),
            items: Vec::new(),
            tags: Vec::new(),
            pool: None,
        }
    }

    fn items(&self) -> &[u32] {
        &self.items
    }

    fn tags(&self) -> &[ItemTag] {
        &self.tags
    }

    /// What the pool held while this ran: a ring the flowgraph did not take
    /// is still in it.
    fn pool(&self) -> Option<(usize, usize)> {
        self.pool
    }
}

impl Kernel for Record {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        self.pool.get_or_insert_with(pool_stats);
        let (i, tags) = self.input.slice_with_tags();
        let n = i.len();
        if n > 0 {
            let start = self.items.len();
            self.items.extend_from_slice(i);
            self.tags.extend(tags.iter().map(|t| ItemTag {
                index: start + t.index,
                tag: t.tag.clone(),
            }));
            self.input.consume(n);
        }
        if self.input.finished() {
            io.finished = true;
        }
        Ok(())
    }
}

/// What a run recorded, once its flowgraph is gone.
struct Recorded {
    items: Vec<u32>,
    tags: Vec<ItemTag>,
    pool: Option<(usize, usize)>,
}

/// `n` items straight from a source into a record, over one connection.
fn run_direct(n: u32) -> Result<Recorded> {
    let mut fg = Flowgraph::new();
    let src = fg.add(blocks::VectorSource::<u32, ReuseCpuWriter<u32>>::new(
        (0..n).collect(),
    ))?;
    let snk = fg.add(Record::new())?;
    fg.stream_dyn(src.id(), "output", snk.id(), "input")?;
    recorded(Runtime::new().run(fg)?, &snk)
}

/// `n` items through source > tagger > record; returns what was recorded.
fn run(n: u32) -> Result<Recorded> {
    let mut fg = Flowgraph::new();
    let src = fg.add(blocks::VectorSource::<u32, ReuseCpuWriter<u32>>::new(
        (0..n).collect(),
    ))?;
    let tag = fg.add(Tagger::new())?;
    let snk = fg.add(Record::new())?;
    fg.stream_dyn(src.id(), "output", tag.id(), "input")?;
    fg.stream_dyn(tag.id(), "output", snk.id(), "input")?;
    recorded(Runtime::new().run(fg)?, &snk)
}

/// What the record of a flowgraph that is over holds, the flowgraph dropped
/// so that it gives up its rings.
fn recorded(done: TerminatedFlowgraph, snk: &BlockRef<Record>) -> Result<Recorded> {
    let snk = done.block(snk)?;
    Ok(Recorded {
        items: snk.items().to_vec(),
        tags: snk.tags().to_vec(),
        pool: snk.pool(),
    })
}

#[test]
fn items_and_tags_go_through() -> Result<()> {
    let _alone = alone();
    let snk = run(10_000)?;
    assert_eq!(snk.items, (0..10_000).collect::<Vec<_>>());
    assert!(!snk.tags.is_empty());
    for tag in &snk.tags {
        assert_eq!(
            tag.tag,
            Tag::Id(tag.index as u64),
            "a tag put on item {} arrived on it",
            tag.index
        );
    }
    Ok(())
}

#[test]
fn the_ring_is_kept_and_taken_again() -> Result<()> {
    let _alone = alone();
    let first = run_direct(10_000)?;
    assert_eq!(
        pool_stats().1,
        1,
        "one ring per connection, once it is over"
    );
    assert_eq!(first.pool, Some((0, 0)), "it had none to take");

    let again = run_direct(10_000)?;
    assert_eq!(again.items, first.items, "reading it again gives the same");
    assert_eq!(
        again.pool,
        Some((0, 0)),
        "the second flowgraph took the ring the first one left"
    );
    assert_eq!(pool_stats().1, 1, "and gave it back");
    Ok(())
}

#[test]
fn a_flowgraph_that_still_runs_keeps_its_rings() -> Result<()> {
    let _alone = alone();
    let mut fg = Flowgraph::new();
    let src = fg.add(blocks::VectorSource::<u32, ReuseCpuWriter<u32>>::new(
        (0..100).collect(),
    ))?;
    let snk = fg.add(Record::new())?;
    fg.stream_dyn(src.id(), "output", snk.id(), "input")?;
    let done = Runtime::new().run(fg)?;
    assert_eq!(pool_stats(), (0, 0), "its ports still hold the ring");
    let _ = done.block(&snk)?.items();
    drop(done);
    assert_eq!(pool_stats().1, 1, "which it gives up with them");
    Ok(())
}

#[test]
fn a_limit_of_zero_keeps_nothing() -> Result<()> {
    let _alone = alone();
    set_pool_limit(0);
    let first = run_direct(1_000)?;
    assert_eq!(pool_stats(), (0, 0));
    let again = run_direct(1_000)?;
    assert_eq!(again.items, first.items, "and everything still works");
    set_pool_limit(DEFAULT_POOL_LIMIT);
    Ok(())
}

#[test]
fn an_output_feeding_two_inputs_keeps_its_ring_until_both_are_gone() -> Result<()> {
    let _alone = alone();
    let mut fg = Flowgraph::new();
    let src = fg.add(blocks::VectorSource::<u32, ReuseCpuWriter<u32>>::new(
        (0..1_000).collect(),
    ))?;
    let one = fg.add(Record::new())?;
    let two = fg.add(Record::new())?;
    fg.stream_dyn(src.id(), "output", one.id(), "input")?;
    fg.stream_dyn(src.id(), "output", two.id(), "input")?;
    let done = Runtime::new().run(fg)?;
    let items = (0..1_000).collect::<Vec<_>>();
    assert_eq!(done.block(&one)?.items(), items, "both read everything");
    assert_eq!(done.block(&two)?.items(), items);
    drop(done);
    assert_eq!(pool_stats().1, 1, "the ring the three of them shared");

    // And it is sound to use again: a reader of the flowgraph that is gone
    // would have been left reading what this one writes.
    let again = run_direct(1_000)?;
    assert_eq!(again.pool, Some((0, 0)), "taken again");
    assert_eq!(again.items, items);
    Ok(())
}

#[test]
fn flowgraphs_in_several_threads_share_the_pool() -> Result<()> {
    let _alone = alone();
    const THREADS: usize = 8;
    const RUNS: usize = 4;
    let items = (0..10_000).collect::<Vec<_>>();
    std::thread::scope(|s| {
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                s.spawn(|| {
                    for _ in 0..RUNS {
                        assert_eq!(run(10_000).unwrap().items, items);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    });
    let (bytes, kept) = pool_stats();
    assert!(kept > 0, "rings are kept");
    assert!(bytes <= DEFAULT_POOL_LIMIT, "within the limit: {bytes}");
    Ok(())
}
