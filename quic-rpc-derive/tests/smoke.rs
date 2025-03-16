use quic_rpc::channel::{
    mpsc,
    none::{NoReceiver, NoSender},
    oneshot,
};
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
        #[rpc(tx=NoSender)]
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
