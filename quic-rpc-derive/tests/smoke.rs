use quic_rpc::channel::{none::NoSender, oneshot};
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

    #[rpc_requests(Service, RequestWithChannels)]
    #[derive(Debug, Serialize, Deserialize)]
    enum Request {
        #[rpc(tx=oneshot::Sender<()>)]
        Rpc(RpcRequest),
        #[rpc(tx=NoSender)]
        ServerStreaming(ServerStreamingRequest),
        #[rpc(tx=NoSender)]
        BidiStreaming(BidiStreamingRequest),
        #[rpc(tx=NoSender)]
        ClientStreaming(ClientStreamingRequest),
    }

    #[derive(Debug, Clone)]
    struct Service;

    impl quic_rpc::Service for Service {}
}

/// Use
///
/// TRYBUILD=overwrite cargo test --test smoke
///
/// to update the snapshots
#[test]
#[ignore = "stupid diffs depending on rustc version"]
fn compile_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
