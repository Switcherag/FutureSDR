# Reference: Key Types

Quick reference for the most important types in FutureSDR.

## Runtime Layer

### Runtime

```rust
pub struct Runtime<S = SmolScheduler> {
    scheduler: S,
    flowgraphs: Arc<Mutex<Vec<FlowgraphHandle>>>,
    ctrl_port: ControlPort,
}
```

| Method | Description |
|--------|-------------|
| `Runtime::new()` | Create with default scheduler, init logging |
| `run(fg)` | Block until flowgraph completes |
| `start(fg).await` | Async start, returns `(JoinHandle, FlowgraphHandle)` |
| `start_sync(fg)` | Sync start, returns `(JoinHandle, FlowgraphHandle)` |
| `spawn(future)` | Spawn async task |
| `spawn_blocking(closure)` | Spawn on blocking thread pool |
| `block_on(future)` | Block current thread on future |

### FlowgraphHandle

| Method | Description |
|--------|-------------|
| `call(block_id, port, pmt).await` | Fire-and-forget message |
| `callback(block_id, port, pmt).await` | Message with response |
| `description().await` | Get flowgraph topology |
| `terminate_and_wait().await` | Graceful shutdown |

## Flowgraph Layer

### Flowgraph

| Method | Description |
|--------|-------------|
| `new()` | Create empty flowgraph |
| `add_block(kernel)` | Add typed block, returns `BlockRef<K>` |
| `add_block_dyn(closure)` | Add dynamic block (plugins), returns `BlockId` |
| `connect_stream(writer, reader)` | Typed stream connection |
| `connect_dyn(src, port, dst, port)` | Dynamic stream connection by name |
| `connect_message(src, port, dst, port)` | Message connection |

### BlockRef\<K\>

| Method | Description |
|--------|-------------|
| `get()` | Lock and access the `WrappedKernel<K>` |
| `Into<BlockId>` | Convert to block ID for connections |

### BlockId / PortId

```rust
pub struct BlockId(usize);   // Index in flowgraph's block vector
pub struct PortId(String);   // Port name (e.g., "input", "output")
```

`PortId` implements `From<&str>` and `From<String>`.

## Block Layer

### Block (trait)

```rust
#[async_trait]
pub trait Block: Send + Any {
    fn id(&self) -> BlockId;
    fn type_name(&self) -> &str;
    fn instance_name(&self) -> Option<&str>;
    fn is_blocking(&self) -> bool;
    fn inbox(&self) -> Sender<BlockMessage>;
    async fn run(&mut self, main_inbox: Sender<FlowgraphMessage>);
    // + stream/message port methods
}
```

### Kernel (trait)

```rust
pub trait Kernel: Send {
    async fn work(&mut self, io: &mut WorkIo, mio: &mut MessageOutputs,
                  meta: &mut BlockMeta) -> Result<()>;
    async fn init(&mut self, mio: &mut MessageOutputs,
                  meta: &mut BlockMeta) -> Result<()>;
    async fn deinit(&mut self, mio: &mut MessageOutputs,
                    meta: &mut BlockMeta) -> Result<()>;
}
```

### WorkIo

| Field | Type | Description |
|-------|------|-------------|
| `call_again` | `bool` | Call `work()` again immediately |
| `finished` | `bool` | Signal block shutdown |
| `block_on` | `Option<Pin<Box<dyn Future>>>` | Suspend until future completes |

### BlockMeta

| Method | Description |
|--------|-------------|
| `instance_name()` | Get block's instance name |
| `set_instance_name(name)` | Set block's instance name |

### MessageOutputs

| Method | Description |
|--------|-------------|
| `post(port, pmt).await` | Send message to all connected handlers |
| `notify_finished()` | Send `Pmt::Finished` to all connections |

## Buffer Layer

### CpuBufferReader

```rust
pub trait CpuBufferReader: BufferReader + Default + Send {
    type Item: CpuSample;
    fn slice(&mut self) -> &[Self::Item];
    fn slice_with_tags(&mut self) -> (&[Self::Item], &Vec<ItemTag>);
    fn consume(&mut self, n: usize);
    fn set_min_items(&mut self, n: usize);
    fn max_items(&self) -> usize;
}
```

### CpuBufferWriter

```rust
pub trait CpuBufferWriter: BufferWriter + Default + Send {
    type Item: CpuSample;
    fn slice(&mut self) -> &mut [Self::Item];
    fn slice_with_tags(&mut self) -> (&mut [Self::Item], Tags<'_>);
    fn produce(&mut self, n: usize);
    fn set_min_items(&mut self, n: usize);
    fn max_items(&self) -> usize;
}
```

### CpuSample

```rust
// Alias for:
Default + Clone + Debug + Send + Sync + 'static
```

Common types: `u8`, `u16`, `u32`, `u64`, `f32`, `f64`, `Complex32`.

### Default Buffers

| Platform | Reader | Writer |
|----------|--------|--------|
| Native | `circular::Reader<D>` | `circular::Writer<D>` |
| WASM | `slab::Reader<D>` | `slab::Writer<D>` |

## Message Layer

### Pmt

```rust
pub enum Pmt {
    Ok, InvalidValue, Null, Finished,
    Bool(bool), Usize(usize), Isize(isize),
    U32(u32), U64(u64), F32(f32), F64(f64),
    String(String),
    VecF32(Vec<f32>), VecCF32(Vec<Complex32>),
    VecU64(Vec<u64>), Blob(Vec<u8>), VecPmt(Vec<Pmt>),
    MapStrPmt(HashMap<String, Pmt>),
    Any(Box<dyn PmtAny>),
}
```

### Tag

```rust
pub enum Tag {
    Id(u64),
    String(String),
    Data(Pmt),
    NamedUsize(String, usize),
    NamedF32(String, f32),
    NamedAny(String, Box<dyn TagAny>),
}
```

### ItemTag

```rust
pub struct ItemTag {
    pub index: usize,
    pub tag: Tag,
}
```

## Plugin Layer

### BlockFactory (trait)

```rust
pub trait BlockFactory: Send + Sync {
    fn create_block(&self, id: BlockId, config: Box<dyn Any + Send>) -> Box<dyn Block>;
    fn block_name(&self) -> &'static str;
    fn block_description(&self) -> &'static str;
}
```

### LoadedPlugin

| Method | Description |
|--------|-------------|
| `load(path)` | Load `.so`, validate ABI, panic on error |
| `try_load(path)` | Load `.so`, validate ABI, return `Result` |
| `load_unchecked(path)` | Load `.so`, skip ABI check |
| `prepare(config)` | Return closure for `add_block_dyn` |
| `block_name()` | Plugin's block name |
| `block_description()` | Plugin's description |

### ABI Fingerprint

```rust
// src/runtime/abi_fingerprint.rs
pub const ABI_FINGERPRINT: &str;        // 16-char hex hash
pub const ABI_FINGERPRINT_DETAIL: &str; // "rustc=...;futuresdr=...;target=...;seify=..."
```

## Error

```rust
#[non_exhaustive]
pub enum Error {
    InvalidBlock(BlockId),
    FlowgraphTerminated,
    InvalidMessagePort(BlockPortCtx, PortId),
    InvalidStreamPort(BlockPortCtx, PortId),
    InvalidParameter,
    HandlerError(String),
    BlockTerminated,
    RuntimeError(String),
    ValidationError(String),
    PmtConversionError,
    DuplicateBlockName(String),
    LockError,
    SeifyArgsConversionError,
    SeifyError(String),
}
```

## Derive Attributes

| Attribute | Target | Description |
|-----------|--------|-------------|
| `#[derive(Block)]` | struct | Generate `KernelInterface` impl |
| `#[input]` | field | Mark as stream input port |
| `#[output]` | field | Mark as stream output port |
| `#[message_inputs(h1, h2)]` | struct | Declare message input handlers |
| `#[message_outputs(p1, p2)]` | struct | Declare message output ports |
| `#[blocking]` | struct | Run `work()` on blocking thread |
| `#[type_name = "..."]` | struct | Custom block type name |
| `#[null_kernel]` | struct | Generate empty `Kernel` impl |
