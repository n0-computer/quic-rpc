<<<<<<< HEAD
use quic_rpc_derive::rpc_requests;
use serde::{Deserialize, Serialize};

#[test]
fn simple() {
    #[derive(Debug, Serialize, Deserialize)]
    struct RpcRequest;

    #[derive(Debug, Serialize, Deserialize)]
    struct ServerStreamingRequest;

    #[derive(Debug, Serialize, Deserialize)]
    struct ClientStreamingRequest;

    #[derive(Debug, Serialize, Deserialize)]
    struct BidiStreamingRequest;

    #[derive(Debug, Serialize, Deserialize)]
    struct Update1;

    #[derive(Debug, Serialize, Deserialize)]
    struct Update2;

    #[derive(Debug, Serialize, Deserialize)]
    struct Response1;

    #[derive(Debug, Serialize, Deserialize)]
    struct Response2;

    #[derive(Debug, Serialize, Deserialize)]
    struct Response3;

    #[derive(Debug, Serialize, Deserialize)]
    struct Response4;

    #[rpc_requests(Service)]
    #[derive(Debug, Serialize, Deserialize, derive_more::From, derive_more::TryInto)]
    enum Request {
        #[rpc(response=Response1)]
        Rpc(RpcRequest),
        #[server_streaming(response=Response2)]
        ServerStreaming(ServerStreamingRequest),
        #[bidi_streaming(update= Update1, response = Response3)]
        BidiStreaming(BidiStreamingRequest),
        #[client_streaming(update = Update2, response = Response4)]
        ClientStreaming(ClientStreamingRequest),
        Update1(Update1),
        Update2(Update2),
    }

    #[derive(Debug, Serialize, Deserialize, derive_more::From, derive_more::TryInto)]
    enum Response {
        Response1(Response1),
        Response2(Response2),
        Response3(Response3),
        Response4(Response4),
    }

    #[derive(Debug, Clone)]
    struct Service;

    impl quic_rpc::Service for Service {
        type Req = Request;
        type Res = Response;
    }

    let _ = Service;
}

/// Use
///
/// TRYBUILD=overwrite cargo test --test smoke
///
/// to update the snapshots
#[test]
fn compile_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
||||||| parent of 66d32e2 (deps: Upgrade to Quinn 0.11 and Rustls 0.23)
=======
use quic_rpc::pattern::{bidi_streaming, server_streaming};
use quic_rpc_derive::rpc_requests;
use serde::{Deserialize, Serialize};

#[test]
fn simple() {
    #[derive(Debug, Serialize, Deserialize)]
    struct RpcRequest;

    #[derive(Debug, Serialize, Deserialize)]
    struct ServerStreamingRequest;

    #[derive(Debug, Serialize, Deserialize)]
    struct ClientStreamingRequest;

    #[derive(Debug, Serialize, Deserialize)]
    struct BidiStreamingRequest;

    #[rpc_requests(Service)]
    #[derive(Debug, Serialize, Deserialize, derive_more::From, derive_more::TryInto)]
    enum Request {
        #[rpc(response=())]
        Rpc(RpcRequest),
        #[server_streaming(response=())]
        ServerStreaming(ServerStreamingRequest),
        #[bidi_streaming(update=(), response=())]
        BidiStreaming(BidiStreamingRequest),
        #[client_streaming(update=(), response=())]
        ClientStreaming(ClientStreamingRequest),
        GenericUpdate(()),
    }

    #[derive(Debug, Serialize, Deserialize, derive_more::From, derive_more::TryInto)]
    enum Response {
        Void(()),
    }

    #[derive(Debug, Clone)]
    struct Service;

    impl quic_rpc::Service for Service {
        type Req = Request;
        type Res = Response;
    }
}
>>>>>>> 66d32e2 (deps: Upgrade to Quinn 0.11 and Rustls 0.23)
