# vmcircbuffer 0.0.15, patched

`src/` is vmcircbuffer 0.0.15 (Apache-2.0, Bastian Bloessl,
https://github.com/futuresdr/vmcircbuffer) with two commits on top, used by
this workspace through `[patch.crates-io]`:

1. **Reuse the mappings of dropped buffers.** Dropped double mappings go to a
   process-wide pool (at most 64 MiB, `set_pool_limit` changes that) and new
   buffers of the same size take them. Creating a mapping costs several
   system calls and a page fault per page, releasing it interrupts every
   core; a flowgraph replacement creates and drops a few buffers.
2. **Use memfd_create on Linux**, with the temporary file as a fallback.

Creating and dropping a 32768-item buffer after 100 ms idle: 187 + 13 us
before, 5.6 + 0.7 us after.

The same two commits, rebased on upstream `main` (0.0.16), are meant for
upstream. Once a release contains them and FutureSDR depends on it, delete
this directory and the `[patch.crates-io]` entry.
