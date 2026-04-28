# iceoryx2 Shared Memory Example

## Introduction

This example demonstrates how to stream data between two FutureSDR applications using iceoryx2 zero-copy shared memory. Unlike ZeroMQ which uses TCP sockets, iceoryx2 transfers data via shared memory, avoiding serialization and network overhead for inter-process communication on the same machine.

## How It Works

The example consists of a sender flowgraph and a receiver flowgraph communicating through a named iceoryx2 service.

* iox2-sender:
    - NullSource: Generates a stream of null bytes.
    - Head: Limits the stream to 1,000,000 samples.
    - Throttle: Regulates the flow to 100 kHz.
    - PubSink: Publishes the data via iceoryx2 shared memory service `futuresdr/iox2-example`.

* iox2-receiver:
    - SubSource: Subscribes to the iceoryx2 service and receives the stream.
    - FileSink: Records the incoming data into a local binary file (`iox2-log.bin`).

## How to Run

To run the shared memory IPC example, use two separate terminals.

Start the receiver first:

```sh
FUTURESDR_CTRLPORT_BIND=127.0.0.1:1338 cargo run --release --bin iox2-receiver
```

In a second terminal, start the sender:

```sh
cargo run --release --bin iox2-sender
```

Once the sender finishes sending its 1 million samples, the transfer will complete and the `iox2-log.bin` file will be written to your project directory.
