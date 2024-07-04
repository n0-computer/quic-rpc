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
