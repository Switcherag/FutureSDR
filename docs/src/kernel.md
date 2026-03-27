# Kernel & the Derive Macro

A `Kernel` is a user-defined struct that implements the actual signal
processing. The `#[derive(Block)]` macro generates all the boilerplate for port
management, message routing, and type metadata.

## The Kernel Trait

```rust
pub trait Kernel: Send {
    fn work(
        &mut self,
        io: &mut WorkIo,
        mio: &mut MessageOutputs,
        meta: &mut BlockMeta,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    fn init(
        &mut self,
        mio: &mut MessageOutputs,
        meta: &mut BlockMeta,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    fn deinit(
        &mut self,
        mio: &mut MessageOutputs,
        meta: &mut BlockMeta,
    ) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }
}
```

All three methods have default no-op implementations, so you only override what
you need.

- **`work()`**: Called repeatedly by the work loop. Read from input buffers,
  process, write to output buffers, and control flow via `WorkIo`.
- **`init()`**: Called once before the first `work()`. Open files, allocate
  resources, set instance names.
- **`deinit()`**: Called once after the last `work()`. Close files, flush
  buffers, report statistics.

## WorkIo

The `WorkIo` struct controls the work loop's behavior:

```rust
pub struct WorkIo {
    pub call_again: bool,
    pub finished: bool,
    pub block_on: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
}
```

- **`call_again`**: If `true` after `work()` returns, `work()` is called again
  immediately without waiting for a `Notify`. Useful when a block has more data
  to produce than fits in one buffer slice.
- **`finished`**: Set to `true` to signal that this block is done. Triggers the
  shutdown sequence.
- **`block_on`**: Set to `Some(future)` to suspend the work loop until the
  future completes (while still processing messages).

## The KernelInterface Trait

`KernelInterface` is the companion trait that handles port plumbing:

```rust
pub trait KernelInterface {
    fn is_blocking() -> bool;
    fn type_name() -> &'static str;
    fn stream_inputs(&self) -> Vec<String>;
    fn stream_outputs(&self) -> Vec<String>;
    fn stream_ports_init(&mut self, block_id: BlockId, inbox: Sender<BlockMessage>);
    fn stream_ports_validate(&self) -> Result<(), Error>;
    fn stream_input(&mut self, name: &str) -> Option<&mut dyn BufferReader>;
    fn connect_stream_output(
        &mut self, name: &str, reader: &mut dyn BufferReader,
    ) -> Result<(), Error>;
    fn message_inputs() -> &'static [&'static str];
    fn message_outputs() -> &'static [&'static str];
    fn call_handler(
        &mut self, io: &mut WorkIo, mio: &mut MessageOutputs,
        meta: &mut BlockMeta, id: PortId, p: Pmt,
    ) -> impl Future<Output = Result<Pmt, Error>> + Send;
    // ...
}
```

You never implement `KernelInterface` by hand. The `#[derive(Block)]` macro
generates it entirely.

## The `#[derive(Block)]` Macro

### Attributes

| Attribute | Purpose |
|-----------|---------|
| `#[input]` | Marks a field as a stream input port |
| `#[output]` | Marks a field as a stream output port |
| `#[message_inputs(handler1, handler2)]` | Declares message input handlers |
| `#[message_outputs(port1, port2)]` | Declares message output ports |
| `#[blocking]` | Block runs `work()` on a blocking thread |
| `#[type_name = "MyBlock"]` | Custom type name for introspection |
| `#[null_kernel]` | Generate an empty `Kernel` impl (message-only blocks) |

### Example: Filter Block

```rust
#[derive(Block)]
pub struct Copy<
    T: Send + Sync + 'static,
    I: CpuBufferReader<Item = T> = DefaultCpuReader<T>,
    O: CpuBufferWriter<Item = T> = DefaultCpuWriter<T>,
> {
    #[input]
    input: I,
    #[output]
    output: O,
}

impl<T, I, O> Kernel for Copy<T, I, O>
where
    T: Send + Sync + Clone + 'static,
    I: CpuBufferReader<Item = T>,
    O: CpuBufferWriter<Item = T>,
{
    async fn work(
        &mut self, io: &mut WorkIo, _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let o = self.output.slice();
        let m = std::cmp::min(i.len(), o.len());
        if m > 0 {
            o[..m].copy_from_slice(&i[..m]);
            self.input.consume(m);
            self.output.produce(m);
        }
        if self.input.finished() && m == i.len() {
            io.finished = true;
        }
        Ok(())
    }
}
```

### What the Macro Generates

For the `Copy` block above, the macro generates:

1. **Port accessor methods**: `pub fn input(&mut self) -> &mut I` and
   `pub fn output(&mut self) -> &mut O`.

2. **`stream_inputs()`**: Returns `vec!["input".to_string()]`.

3. **`stream_outputs()`**: Returns `vec!["output".to_string()]`.

4. **`stream_ports_init()`**: Calls `self.input.init(block_id, port_id, inbox)`
   and `self.output.init(...)`.

5. **`stream_input(name)`**: Matches `"input"` and returns
   `Some(&mut self.input as &mut dyn BufferReader)`.

6. **`connect_stream_output(name, reader)`**: Matches `"output"` and calls
   `self.output.connect_dyn(reader)`.

7. **`stream_ports_validate()`**: Checks that all ports have been connected.

8. **`call_handler(id, p)`**: Matches port names to message handler methods.

### Message Handlers

Message handlers are async methods with this exact signature:

```rust
async fn handler_name(
    &mut self,
    io: &mut WorkIo,
    mio: &mut MessageOutputs,
    meta: &mut BlockMeta,
    p: Pmt,
) -> Result<Pmt>
```

Example:

```rust
#[derive(Block)]
#[message_inputs(freq)]
#[message_outputs(status)]
#[null_kernel]
pub struct Controller {
    current_freq: f64,
}

impl Controller {
    async fn freq(
        &mut self, _io: &mut WorkIo, mio: &mut MessageOutputs,
        _meta: &mut BlockMeta, p: Pmt,
    ) -> Result<Pmt> {
        if let Pmt::F64(f) = p {
            self.current_freq = f;
            mio.post("status", Pmt::String(format!("tuned to {f}"))).await?;
            Ok(Pmt::Ok)
        } else {
            Ok(Pmt::InvalidValue)
        }
    }
}
```

The `#[null_kernel]` attribute generates an empty `impl Kernel` so you don't
have to write one for message-only blocks.

### Container Ports

The macro supports multiple ports via containers:

```rust
#[derive(Block)]
pub struct Interleaver<T: Send + 'static> {
    #[input]
    inputs: Vec<DefaultCpuReader<T>>,   // ports: "inputs[0]", "inputs[1]", ...
    #[output]
    output: DefaultCpuWriter<T>,
}
```

Supported containers: `Vec<T>`, `[T; N]`, and tuples `(T1, T2, ...)`.

## The Generic Pattern

Most FutureSDR blocks are generic over their buffer types:

```rust
pub struct MyBlock<
    T: Send + 'static,
    I: CpuBufferReader<Item = T> = DefaultCpuReader<T>,
    O: CpuBufferWriter<Item = T> = DefaultCpuWriter<T>,
> { ... }
```

The defaults (`DefaultCpuReader<T>`, `DefaultCpuWriter<T>`) resolve to
`circular::Reader<T>` on native and `slab::Reader<T>` on WASM. Users can
substitute custom buffer types (GPU, FPGA) without changing the block's logic.
