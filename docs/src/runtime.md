# Runtime Engine

The `Runtime` is the top-level object that owns the async executor and manages
flowgraph lifecycles.

## Structure

```rust
pub struct Runtime<S = SmolScheduler> {
    scheduler: S,
    flowgraphs: Arc<Mutex<Vec<FlowgraphHandle>>>,
    ctrl_port: ControlPort,
}
```

- **`scheduler`**: Executes async tasks. The default `SmolScheduler` uses the
  `smol` crate's multi-threaded executor. Custom schedulers can be plugged in.
- **`flowgraphs`**: Running flowgraph handles, accessible via REST API.
- **`ctrl_port`**: HTTP/WebSocket server for runtime introspection and control.

## Creating a Runtime

```rust
// Default: SmolScheduler, no custom routes
let rt = Runtime::new();

// Custom scheduler
let rt = Runtime::with_scheduler(MyScheduler::new());

// Custom REST routes
let rt = Runtime::with_custom_routes(my_router);
```

`Runtime::new()` also calls `init()` internally, which sets up logging via
`tracing-subscriber` (console output with ANSI colors, configured by
`RUST_LOG` environment variable).

## Running Flowgraphs

There are several ways to execute a flowgraph:

### `run(fg)` — Blocking, Run to Completion

```rust
rt.run(fg)?;  // Blocks until all blocks finish
```

This is the simplest mode. The flowgraph runs until all blocks signal
`finished`, then `run` returns.

### `start_sync(fg)` — Non-blocking Start

```rust
let (task, mut handle) = rt.start_sync(fg)?;
```

Returns immediately with:
- `task`: A `JoinHandle` for the flowgraph's background task
- `handle`: A `FlowgraphHandle` for sending messages and controlling the
  flowgraph

This is the mode used in the PHY swap example, where we need to send frames
and then terminate the flowgraph programmatically.

### `start(fg)` — Async Start

```rust
let (task, handle) = rt.start(fg).await?;
```

Same as `start_sync` but for use inside async contexts.

## FlowgraphHandle

The `FlowgraphHandle` is the control interface to a running flowgraph:

```rust
// Send a message to a block's handler (fire-and-forget)
handle.call(block_id, "tx", Pmt::Blob(data)).await?;

// Send a message and wait for response
let response = handle.callback(block_id, "stats", Pmt::Null).await?;

// Get flowgraph topology description
let desc = handle.description().await?;

// Terminate the flowgraph
handle.terminate_and_wait().await?;
```

## Flowgraph Execution Internals

When a flowgraph is started, the runtime:

1. **Validates** all stream and message connections (checks port existence,
   buffer type compatibility).

2. **Initializes** each block by sending `BlockMessage::Initialize`. Each block
   calls its kernel's `init()` method, sets up buffer connections, and reports
   back with `FlowgraphMessage::Initialized`.

3. **Kicks off** the work loop by sending `BlockMessage::Notify` to all blocks.

4. **Enters the main loop**: dispatches incoming `FlowgraphMessage`s:
   - `BlockDone { block_id }` — decrements active block count
   - `BlockError { block_id }` — logs error, decrements count
   - `BlockCall/BlockCallback` — forwards to the target block
   - `Terminate` — sends `BlockMessage::Terminate` to all blocks
   - `FlowgraphDescription/BlockDescription` — introspection queries

5. **Terminates** when the active block count reaches zero.

## Task Spawning

The runtime exposes task spawning for use in blocks or application code:

```rust
rt.spawn(async { /* ... */ });           // Spawn async task
rt.spawn_blocking(|| { /* ... */ });     // Spawn on blocking thread pool
rt.spawn_background(async { /* ... */ }); // Detached (no join handle)
rt.block_on(async { /* ... */ });        // Block current thread
```
