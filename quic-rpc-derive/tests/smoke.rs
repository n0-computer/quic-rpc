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
        #[rpc(response=u32)]
        Rpc(RpcRequest),
        #[server_streaming(response=())]
        ServerStreaming(ServerStreamingRequest),
        #[bidi_streaming(update=(), response = ())]
        BidiStreaming(BidiStreamingRequest),
        #[client_streaming(update = (), response = ())]
        ClientStreaming(ClientStreamingRequest),
        // an update, you will never get this as the first message
        GenericUpdate(()),
    }

    #[derive(Debug, Serialize, Deserialize, derive_more::From, derive_more::TryInto)]
    enum Response {
        Void(()),
        Rpc(u32),
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
