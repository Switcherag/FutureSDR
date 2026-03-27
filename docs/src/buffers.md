# Buffer System

Buffers are the data transport layer between stream-connected blocks. They
provide zero-copy, typed access to contiguous sample slices.

## Buffer Trait Hierarchy

```
                    BufferReader          BufferWriter
                   (Any + async)         (type Reader)
                    /         \            /         \
          CpuBufferReader   InplaceReader  CpuBufferWriter  InplaceWriter
           (slice/consume)  (get/put buf)  (slice/produce)  (get/put buf)
```

### BufferReader

The base trait for all buffer readers:

```rust
#[async_trait]
pub trait BufferReader: Any {
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn init(&mut self, block_id: BlockId, port_id: PortId, inbox: Sender<BlockMessage>);
    fn validate(&self) -> Result<(), Error>;
    async fn notify_finished(&mut self);
    fn finish(&mut self);
    fn finished(&self) -> bool;
}
```

### BufferWriter

The base trait for all buffer writers:

```rust
pub trait BufferWriter {
    type Reader: BufferReader;
    fn init(&mut self, block_id: BlockId, port_id: PortId, inbox: Sender<BlockMessage>);
    fn validate(&self) -> Result<(), Error>;
    fn connect(&mut self, dest: &mut Self::Reader);
    fn connect_dyn(&mut self, dest: &mut dyn BufferReader) -> Result<(), Error>;
}
```

`connect_dyn` uses `Any::downcast_mut` to recover the concrete reader type. If
the types don't match, it returns an error — this is the mechanism that catches
sample type mismatches at connection time.

### CpuBufferReader / CpuBufferWriter

The CPU-specific traits add the slice-based API:

```rust
pub trait CpuBufferReader: BufferReader + Default + Send {
    type Item: CpuSample;
    fn slice(&mut self) -> &[Self::Item];
    fn slice_with_tags(&mut self) -> (&[Self::Item], &Vec<ItemTag>);
    fn consume(&mut self, n: usize);
    fn set_min_items(&mut self, n: usize);
    fn max_items(&self) -> usize;
}

pub trait CpuBufferWriter: BufferWriter + Default + Send {
    type Item: CpuSample;
    fn slice(&mut self) -> &mut [Self::Item];
    fn slice_with_tags(&mut self) -> (&mut [Self::Item], Tags<'_>);
    fn produce(&mut self, n: usize);
    fn set_min_items(&mut self, n: usize);
    fn max_items(&self) -> usize;
}
```

`CpuSample` is `Default + Clone + Debug + Send + Sync + 'static`. Common
types: `u8`, `f32`, `Complex32`.

## Default Buffers

```rust
// Native (Linux, Windows, macOS)
type DefaultCpuReader<D> = circular::Reader<D>;
type DefaultCpuWriter<D> = circular::Writer<D>;

// WebAssembly
type DefaultCpuReader<D> = slab::Reader<D>;
type DefaultCpuWriter<D> = slab::Writer<D>;
```

## Circular Buffer (`circular`)

The default buffer on native platforms. Uses `vmcircbuffer` — a double-mapped
virtual memory circular buffer.

### How Double Mapping Works

The buffer allocates `2 * capacity` bytes of virtual address space but maps
the same physical pages to both halves:

```
Virtual addresses:  [ page A | page B | page A | page B ]
Physical pages:     [ page A | page B ]
```

This means a read or write that wraps around the end of the buffer still
appears as a contiguous slice in memory. No copies are needed, no special
wrap-around handling.

### Writer

```rust
pub struct Writer<D: CpuSample> {
    writer: vmcircbuffer::Writer<D>,
    readers: Vec<circular::Reader<D>>,
    tags: Vec<ItemTag>,
    // ...
}
```

- `slice()` returns a mutable slice of available space for writing.
- `produce(n)` advances the write pointer by `n` items and sends
  `BlockMessage::Notify` to connected downstream blocks.
- Tags are attached to produced items via `slice_with_tags()`.

### Reader

```rust
pub struct Reader<D: CpuSample> {
    reader: vmcircbuffer::Reader<D>,
    tags: Vec<ItemTag>,
    // ...
}
```

- `slice()` returns an immutable slice of available items to read.
- `consume(n)` advances the read pointer, freeing space for the writer.
- `finished()` returns `true` when the upstream writer has signaled completion
  and all remaining items have been consumed.

### Buffer Sizing

`set_min_buffer_size_in_items(n)` ensures the buffer is at least `n` items
large. `set_min_items(n)` means `slice()` won't return fewer than `n` items
(the block won't be woken until enough data is available).

## Slab Buffer (`slab`)

Used on WASM (where `mmap` is unavailable). Allocates fixed-size slabs from a
pool and passes them between writer and reader via async channels.

```
Writer:  get empty slab → fill → put full slab
Reader:  get full slab  → read → put empty slab
```

Each slab is a contiguous `Vec<D>`, so slices are naturally contiguous without
double mapping.

## Circuit Buffer (`circuit`)

An in-place buffer for blocks that process data without changing the number of
items (e.g., applying a gain). The same buffer object is passed from writer to
reader and back in a closed loop:

```
Writer → put_full_buffer → Reader → get_full_buffer → process → put_empty_buffer → Writer
```

This avoids all copies — the block modifies samples in place.

### InplaceBuffer / InplaceReader / InplaceWriter

```rust
pub trait InplaceBuffer {
    type Item: CpuSample;
    fn set_valid(&mut self, valid: usize);
    fn slice(&mut self) -> &mut [Self::Item];
    fn slice_with_tags(&mut self) -> (&mut [Self::Item], &mut Vec<ItemTag>);
}
```

## GPU and FPGA Buffers

FutureSDR provides buffer backends for heterogeneous computing:

### WGPU Buffer

For GPU compute shaders via the WebGPU API:
- Host-to-device (H2D) and device-to-host (D2H) transfer
- `InputBufferFull<D>` / `OutputBufferFull<D>` carry GPU buffer handles
- `Instance` holds the wgpu `Device`, `Queue`, and `Adapter`

### Vulkan Buffer

For GPU compute via Vulkano:
- `Buffer<T>` wraps a Vulkano `Subbuffer`
- `Instance` provides `Device`, `Queue`, and `MemoryAllocator`

### Zynq DMA Buffer

For FPGA accelerators on Xilinx Zynq platforms, using DMA transfers between
the ARM CPU and programmable logic.

## Tags

Tags are metadata attached to specific sample positions in a buffer:

```rust
pub struct ItemTag {
    pub index: usize,    // sample index within the current slice
    pub tag: Tag,
}

pub enum Tag {
    Id(u64),
    String(String),
    Data(Pmt),
    NamedUsize(String, usize),
    NamedF32(String, f32),
    NamedAny(String, Box<dyn TagAny>),
}
```

Writers attach tags via `slice_with_tags()`:

```rust
let (samples, mut tags) = self.output.slice_with_tags();
// ... fill samples ...
tags.add_tag(42, Tag::String("burst_start".into()));
self.output.produce(n);
```

Readers retrieve them:

```rust
let (samples, tags) = self.input.slice_with_tags();
for tag in tags {
    if tag.index < n_consumed {
        // process tag
    }
}
self.input.consume(n);
```

Tags flow downstream with the sample stream. The buffer infrastructure
adjusts tag indices as samples are consumed and produced.
