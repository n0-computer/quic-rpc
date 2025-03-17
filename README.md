# Quic-Rpc

A streaming rpc system based on quic

[<img src="https://img.shields.io/badge/github-quic_rpc-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20" >][repo link] [![Latest Version]][crates.io] [![Docs Badge]][docs.rs] ![license badge] [![status badge]][status link]

[Latest Version]: https://img.shields.io/crates/v/quic-rpc.svg
[crates.io]: https://crates.io/crates/quic-rpc
[Docs Badge]: https://img.shields.io/badge/docs-docs.rs-green
[docs.rs]: https://docs.rs/quic-rpc
[license badge]: https://img.shields.io/crates/l/quic-rpc
[status badge]: https://github.com/n0-computer/quic-rpc/actions/workflows/rust.yml/badge.svg
[status link]: https://github.com/n0-computer/quic-rpc/actions/workflows/rust.yml
[repo link]: https://github.com/n0-computer/quic-rpc

## Goals

### Interaction patterns

Provide not just request/response RPC, but also streaming in both directions, similar to [grpc].

- 1 req -> 1 res
- 1 req, update stream -> 1 res
- 1 req -> res stream
- 1 req, update stream -> res stream

It is still a RPC system in the sense that interactions get initiated by the client.

### Transports

- memory transport with very low overhead. In particular, no ser/deser, currently using [tokio channels]
   when using tokio channels, there is zero overhead compared to just manually including backchannels in messages
- quic transport via the [iroh-quinn] crate

### API

- The API should be similar to the quinn api. Basically "quinn with types".

## Non-Goals

- Cross language interop. This is for talking from rust to rust
- Any kind of versioning. You have to do this yourself
- Making remote message passing look like local async function calls
- Being runtime agnostic. This is for tokio

## Example

[computation service](https://github.com/n0-computer/quic-rpc/blob/main/tests/math.rs)

## Why?

The purpose of quic-rpc is to serve as an *optional* rpc framework. One of the
main goals is to be able to use it as an *in process* way to have well specified
protocols and boundaries between subsystems, including an async boundary.

It should not have noticeable overhead compared to what you would do anyway to
isolate subsystems in a complex single process app, but should have the *option*
to also send messages over a process boundary via one of the non mem transports.

What do you usually do in rust to have isolation between subsystems, e.g.
between a database and a networking layer? You have some kind of
channel between the systems and define messages flowing back and forth over that
channel. For almost all interactions these messages itself will again contain
(oneshot or mpsc) channels for independent async communication between the
subsystems.

Quic-rpc with the mem channel does exactly the same thing, except that it
distinguishes between the serializable protocol of a service and the full
messages of a service that include unserializable channels.

Instead of having a message that explicitly contains some data and the send side
of a oneshot or mpsc channel for the response, it allows you to specify the
channel kind and message type of the channels in both directions.

For the case where you have a process boundary, the overhead is very low for
transports that already have a concept of cheap substreams (http2, quic, ...).
Quic is the poster child of a network transport that has built in cheap
substreams including per substream backpressure.

[iroh-quinn]: https://docs.rs/iroh-quinn/
[tokio channels]: https://docs.rs/tokio/latest/tokio/sync/index.html
[grpc]: https://grpc.io/

# Docs

Properly building docs for this crate is quite complex. For all the gory details,
see [DOCS.md].