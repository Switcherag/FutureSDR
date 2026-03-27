# Flowgraph & Connections

A `Flowgraph` is a directed graph where nodes are blocks and edges are either
**stream connections** (typed sample buffers) or **message connections**
(asynchronous PMT messages).

## Structure

```rust
pub struct Flowgraph {
    blocks: Vec<Arc<Mutex<dyn Block>>>,
    stream_edges: Vec<(BlockId, PortId, BlockId, PortId)>,
    message_edges: Vec<(BlockId, PortId, BlockId, PortId)>,
}
```

- **`blocks`**: All registered blocks, stored as trait objects behind
  `Arc<Mutex<dyn Block>>`. The index in this vector is the block's `BlockId`.
- **`stream_edges`**: `(src_block, src_port, dst_block, dst_port)` tuples for
  high-throughput sample connections.
- **`message_edges`**: Same layout for asynchronous message connections.

## Adding Blocks

```rust
let mut fg = Flowgraph::new();

// Typed: returns a BlockRef<K> with compile-time access to the kernel
let src = fg.add_block(NullSource::<f32>::new());

// Dynamic (plugin feature): accepts a closure that returns Box<dyn Block>
let blk_id = fg.add_block_dyn(plugin.prepare(Box::new(config)));
```

`add_block` wraps the kernel in a `WrappedKernel<K>`, assigns it a `BlockId`
(its index in the `blocks` vector), and returns a `BlockRef<K>`.

### BlockRef

`BlockRef<K>` is a typed handle to a block inside the flowgraph:

```rust
pub struct BlockRef<K: Kernel> {
    id: BlockId,
    block: Arc<Mutex<WrappedKernel<K>>>,
}
```

It implements `Into<BlockId>`, so you can pass it directly to connection methods.
The `get()` method returns a `MutexGuard` for accessing the kernel before the
flowgraph starts running.

## Stream Connections

Stream connections carry typed sample data through circular buffers.

### Typed Connections

```rust
let src = fg.add_block(NullSource::<f32>::new());
let sink = fg.add_block(NullSink::<f32>::new());

// Connect src's output port to sink's input port
fg.connect_stream(&mut src.get()?.output, &mut sink.get()?.input);
```

`connect_stream` is fully typed: the compiler verifies that the writer's sample
type matches the reader's sample type. The buffer writer's `connect()` method
wires up the shared circular buffer state between the two blocks.

### Dynamic Connections

When block types are erased (plugins, runtime-constructed graphs), use
`connect_dyn`:

```rust
fg.connect_dyn(src_id, "output", dst_id, "input")?;
```

This resolves ports by name at runtime. It calls `connect_stream_output` on the
source block and `stream_input` on the destination block, then wires them via
`connect_dyn` on the buffer writer. If the sample types don't match (different
`TypeId`), this returns an error.

## Message Connections

Message connections route `Pmt` values between blocks asynchronously:

```rust
fg.connect_message(src_id, "tx_status", mac_id, "rx_status")?;
```

This registers the destination block's inbox sender on the source block's
`MessageOutput` for the given port. When the source block calls
`mio.post("tx_status", pmt)`, the message is delivered to all connected
handlers.

Message connections are many-to-many: a single output port can fan out to
multiple destination handlers, and a single input handler can receive from
multiple sources.

## Topology Validation

Validation happens in two stages:

1. **At connection time**: `connect_dyn` checks that the named ports exist on
   both blocks. Missing ports produce `Error::InvalidStreamPort` or
   `Error::InvalidMessagePort`.

2. **At startup**: When the runtime initializes a flowgraph, each block's
   `stream_ports_validate()` method is called. This checks that all declared
   stream ports have been connected. Unconnected ports are an error.

## Flowgraph Lifecycle

```
    new()  →  add blocks  →  connect  →  hand to Runtime
                                              │
                                              ▼
                                     validate & initialize
                                              │
                                              ▼
                                        run work loops
                                              │
                                              ▼
                                     all blocks finish
```

Once a flowgraph is handed to the runtime (via `rt.run(fg)` or
`rt.start(fg)`), it is consumed. The user interacts with the running flowgraph
through the `FlowgraphHandle` returned by `start`/`start_sync`.
