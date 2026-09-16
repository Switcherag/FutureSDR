# vmcircbuffer 0.0.15, patched

Copied from crates.io `vmcircbuffer` 0.0.15 (Apache-2.0, Bastian Bloessl) and
used by the plugin workspace through `[patch.crates-io]`.

One change, in `src/double_mapped_buffer/unix.rs`: dropped double mappings go
to a pool (at most 64 MiB) and new buffers of the same size reuse them.
Creating a mapping costs a temporary file, six system calls and a page fault
per page on first use, and unmapping it interrupts every core the process ran
on. A flowgraph replacement creates and drops a few buffers; with the pool,
starting a flowgraph on an idle machine no longer pays for that.

`DoubleMappedBuffer::new` still writes `T::default()` over a reused buffer, so
no item type sees another type's bytes.
