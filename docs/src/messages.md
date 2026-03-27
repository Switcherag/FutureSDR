# Message Passing & PMT

Messages provide an asynchronous, low-throughput control plane alongside the
high-throughput stream data plane. They are used for configuration, control,
status reporting, and inter-block coordination.

## PMT (Polymorphic Message Type)

`Pmt` is an enum covering common SDR data types:

```rust
#[non_exhaustive]
pub enum Pmt {
    // Signals
    Ok,
    InvalidValue,
    Null,
    Finished,

    // Scalars
    Bool(bool),
    Usize(usize),
    Isize(isize),
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    String(String),

    // Vectors
    VecF32(Vec<f32>),
    VecCF32(Vec<Complex32>),
    VecU64(Vec<u64>),
    Blob(Vec<u8>),
    VecPmt(Vec<Pmt>),

    // Structured
    MapStrPmt(HashMap<String, Pmt>),

    // Type-erased
    Any(Box<dyn PmtAny>),
}
```

### PmtKind

Each `Pmt` variant has a corresponding `PmtKind` discriminant, useful for
runtime type checking without accessing the value:

```rust
let kind: PmtKind = pmt.kind();
match kind {
    PmtKind::F32 => { /* ... */ },
    PmtKind::Blob => { /* ... */ },
    _ => { /* ... */ },
}
```

### Conversions

`Pmt` implements `From<T>` for common types:

```rust
let p: Pmt = 42u64.into();           // Pmt::U64(42)
let p: Pmt = 3.14f32.into();         // Pmt::F32(3.14)
let p: Pmt = "hello".to_string().into(); // Pmt::String("hello")
let p: Pmt = vec![1.0f32, 2.0].into();  // Pmt::VecF32(...)
```

And `TryFrom<Pmt>` for extracting values:

```rust
let val: f64 = pmt.try_into()?;      // fails if not Pmt::F64
let val: usize = pmt.try_into()?;    // fails if not Pmt::Usize
```

### Pmt::Any

For types not covered by the standard variants, `Pmt::Any` wraps any type
that implements `PmtAny`:

```rust
pub trait PmtAny: Any + DynClone + Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn to_any(self: Box<Self>) -> Box<dyn Any>;
}
```

This is automatically implemented for any `T: Any + Clone + Send + Sync + 'static`.

```rust
// Sending
let msg = Pmt::Any(Box::new(MyConfig { freq: 915e6 }));

// Receiving
if let Pmt::Any(ref any) = msg {
    if let Some(cfg) = any.downcast_ref::<MyConfig>() {
        // use cfg.freq
    }
}
```

**ABI warning**: `Pmt::Any` relies on `TypeId`, which is not stable across
different compilation units. When using plugins, both sides must link against
the same `libfuturesdr.so` for `TypeId` to match.

## Message Ports

### Declaring Ports

Message ports are declared via attributes on the kernel struct:

```rust
#[derive(Block)]
#[message_inputs(rx, ctrl)]     // input handlers
#[message_outputs(tx, status)]  // output ports
pub struct MyBlock { /* ... */ }
```

### Message Handlers

Each input port name corresponds to a method on the kernel:

```rust
impl MyBlock {
    async fn rx(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        meta: &mut BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        // Process incoming message
        // Optionally post to output ports
        mio.post("tx", Pmt::Ok).await?;
        Ok(Pmt::Ok)
    }

    async fn ctrl(
        &mut self, _io: &mut WorkIo, _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta, p: Pmt,
    ) -> Result<Pmt> {
        // Handle control messages
        Ok(Pmt::Ok)
    }
}
```

The `#[derive(Block)]` macro generates the dispatch table that routes incoming
`BlockMessage::Call` / `BlockMessage::Callback` to the correct handler method.

### Sending Messages

From inside a block's `work()` or message handler:

```rust
// Post to a named output port (fire-and-forget)
mio.post("tx", Pmt::Blob(frame_data)).await?;
```

`post` sends `BlockMessage::Call` to every block connected to this output port.

### Message Connections

In the flowgraph:

```rust
fg.connect_message(src_id, "tx", dst_id, "rx")?;
```

This registers the destination block's inbox on the source's `MessageOutput`
for port `"tx"`. Multiple destinations can be connected to the same output
(fan-out), and a single input handler can receive from multiple sources.

## MessageOutput / MessageOutputs

```rust
pub struct MessageOutput {
    name: String,
    handlers: Vec<(PortId, Sender<BlockMessage>)>,
}

pub struct MessageOutputs {
    block_id: BlockId,
    outputs: Vec<MessageOutput>,
}
```

`MessageOutputs` is the `mio` parameter passed to `work()`, `init()`, and
message handlers. It provides:

- `post(port_name, pmt)` — send to all connected handlers
- `connect(src_port, dst_inbox, dst_port)` — wire up a connection
- `notify_finished()` — send `Pmt::Finished` to all connected handlers

## External Message Access (FlowgraphHandle)

From outside the flowgraph, messages are sent through the `FlowgraphHandle`:

```rust
// Fire-and-forget
handle.call(block_id, "ctrl", Pmt::F64(915e6)).await?;

// Request-response
let response = handle.callback(block_id, "stats", Pmt::Null).await?;
```

These go through the flowgraph's main loop:
1. `FlowgraphHandle` sends `FlowgraphMessage::BlockCall` to the flowgraph
2. The flowgraph forwards it as `BlockMessage::Call` to the target block
3. For `callback`, the response travels back through a oneshot channel

## Common Patterns

### Configuration Update

```rust
// Application code
handle.call(filter_id, "set_freq", Pmt::F64(new_freq)).await?;

// Inside the filter block
async fn set_freq(&mut self, ..., p: Pmt) -> Result<Pmt> {
    if let Pmt::F64(f) = p {
        self.center_freq = f;
        self.recalculate_taps();
        Ok(Pmt::Ok)
    } else {
        Ok(Pmt::InvalidValue)
    }
}
```

### Frame Transmission

```rust
// Send a frame as a blob
handle.call(mac_id, "tx", Pmt::Blob(frame_bytes)).await?;
```

### Status Query

```rust
let stats = handle.callback(block_id, "stats", Pmt::Null).await?;
if let Pmt::MapStrPmt(map) = stats {
    println!("packets: {:?}", map.get("packets"));
}
```

### Finished Signal

The `Pmt::Finished` value is a convention for signaling end-of-stream through
the message plane. Blocks that receive it typically set `io.finished = true`.
