#![cfg(feature = "http2")]
use std::{net::SocketAddr, result};

use derive_more::{From, TryInto};
use flume::Receiver;
use hyper::Uri;
use quic_rpc::{
    client::RpcClientError,
    message::{Msg, Rpc},
    server::RpcServerError,
    transport::http2::{
        self, Http2ChannelTypes, Http2ClientChannel, Http2ServerChannel, RecvError,
    },
    RpcClient, RpcServer, Service,
};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

mod math;
use math::*;
mod util;

fn run_server(addr: &SocketAddr) -> JoinHandle<anyhow::Result<()>> {
    let channel = Http2ServerChannel::<ComputeRequest, ComputeResponse>::serve(addr).unwrap();
    let server = RpcServer::<ComputeService, Http2ChannelTypes>::new(channel);
    tokio::spawn(async move {
        loop {
            let server = server.clone();
            ComputeService::server(server).await?;
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    })
}

#[tokio::test]
async fn http2_channel_bench() -> anyhow::Result<()> {
    type C = Http2ChannelTypes;
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let uri: Uri = "http://127.0.0.1:3000".parse()?;
    let server_handle = run_server(&addr);
    let client = Http2ClientChannel::new(uri);
    let client = RpcClient::<ComputeService, C>::new(client);
    bench(client, 50000).await?;
    println!("terminating server");
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

#[tokio::test]
async fn http2_channel_smoke() -> anyhow::Result<()> {
    type C = Http2ChannelTypes;
    let addr: SocketAddr = "127.0.0.1:3001".parse()?;
    let uri: Uri = "http://127.0.0.1:3001".parse()?;
    let server_handle = run_server(&addr);
    let client = Http2ClientChannel::new(uri);
    smoke_test::<C>(client).await?;
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

#[tokio::test]
async fn http2_channel_errors() -> anyhow::Result<()> {
    /// request that can be too big
    #[derive(Debug, Serialize, Deserialize)]
    pub struct BigRequest(Vec<u8>);

    /// request that looks serializable but isn't
    #[derive(Debug, Serialize, Deserialize)]
    pub struct NoSerRequest(NoSer);

    /// request that looks deserializable but isn't
    #[derive(Debug, Serialize, Deserialize)]
    pub struct NoDeserRequest(NoDeser);

    /// request where the response is not serializable
    #[derive(Debug, Serialize, Deserialize)]
    pub struct NoSerResponseRequest;

    /// request where the response is not deserializable
    #[derive(Debug, Serialize, Deserialize)]
    pub struct NoDeserResponseRequest;

    /// request that can produce a response that is too big
    #[derive(Debug, Serialize, Deserialize)]
    pub struct BigResponseRequest(usize);

    /// helper struct that implements serde::Serialize but errors on serialization
    #[derive(Debug, Deserialize)]
    pub struct NoSer;

    impl serde::Serialize for NoSer {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom("nope"))
        }
    }

    /// helper struct that implements serde::Deserialize but errors on deserialization
    #[derive(Debug, Serialize)]
    pub struct NoDeser;

    impl<'de> serde::Deserialize<'de> for NoDeser {
        fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Err(serde::de::Error::custom("nope"))
        }
    }

    #[allow(clippy::enum_variant_names)]
    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    enum TestRequest {
        BigRequest(BigRequest),
        NoSerRequest(NoSerRequest),
        NoDeserRequest(NoDeserRequest),
        NoSerResponseRequest(NoSerResponseRequest),
        NoDeserResponseRequest(NoDeserResponseRequest),
        BigResponseRequest(BigResponseRequest),
    }

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    enum TestResponse {
        Unit(()),
        Big(Vec<u8>),
        NoSer(NoSer),
        NoDeser(NoDeser),
    }

    #[derive(Debug, Clone)]
    struct TestService;

    impl Service for TestService {
        type Req = TestRequest;
        type Res = TestResponse;
    }

    impl TestService {
        async fn big(self, _req: BigRequest) {}

        async fn noser(self, _req: NoSerRequest) {}

        async fn nodeser(self, _req: NoDeserRequest) {}

        async fn noserresponse(self, _req: NoSerResponseRequest) -> NoSer {
            NoSer
        }

        async fn nodeserresponse(self, _req: NoDeserResponseRequest) -> NoDeser {
            NoDeser
        }

        async fn bigresponse(self, req: BigResponseRequest) -> Vec<u8> {
            vec![0; req.0]
        }
    }

    impl Msg<TestService> for BigRequest {
        type Response = ();
        type Update = Self;
        type Pattern = Rpc;
    }

    impl Msg<TestService> for NoSerRequest {
        type Response = ();
        type Update = Self;
        type Pattern = Rpc;
    }

    impl Msg<TestService> for NoDeserRequest {
        type Response = ();
        type Update = Self;
        type Pattern = Rpc;
    }

    impl Msg<TestService> for NoSerResponseRequest {
        type Response = NoSer;
        type Update = Self;
        type Pattern = Rpc;
    }

    impl Msg<TestService> for NoDeserResponseRequest {
        type Response = NoDeser;
        type Update = Self;
        type Pattern = Rpc;
    }

    impl Msg<TestService> for BigResponseRequest {
        type Response = Vec<u8>;
        type Update = Self;
        type Pattern = Rpc;
    }

    #[allow(clippy::type_complexity)]
    fn run_test_server(
        addr: &SocketAddr,
    ) -> (
        JoinHandle<anyhow::Result<()>>,
        Receiver<result::Result<(), RpcServerError<C>>>,
    ) {
        let channel = Http2ServerChannel::serve(addr).unwrap();
        let server = RpcServer::<TestService, Http2ChannelTypes>::new(channel);
        let (res_tx, res_rx) = flume::unbounded();
        let handle = tokio::spawn(async move {
            loop {
                let x = server.accept_one().await;
                let res = match x {
                    Ok((req, chan)) => match req {
                        TestRequest::BigRequest(req) => {
                            server.rpc(req, chan, TestService, TestService::big).await
                        }
                        TestRequest::NoSerRequest(req) => {
                            server.rpc(req, chan, TestService, TestService::noser).await
                        }
                        TestRequest::NoDeserRequest(req) => {
                            server
                                .rpc(req, chan, TestService, TestService::nodeser)
                                .await
                        }
                        TestRequest::NoSerResponseRequest(req) => {
                            server
                                .rpc(req, chan, TestService, TestService::noserresponse)
                                .await
                        }
                        TestRequest::NoDeserResponseRequest(req) => {
                            server
                                .rpc(req, chan, TestService, TestService::nodeserresponse)
                                .await
                        }
                        TestRequest::BigResponseRequest(req) => {
                            server
                                .rpc(req, chan, TestService, TestService::bigresponse)
                                .await
                        }
                    },
                    Err(e) => Err(e),
                };
                res_tx.send_async(res).await.unwrap();
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        });
        (handle, res_rx)
    }

    type C = Http2ChannelTypes;
    let addr: SocketAddr = "127.0.0.1:3002".parse()?;
    let uri: Uri = "http://127.0.0.1:3002".parse()?;
    let (server_handle, server_results) = run_test_server(&addr);
    let client = Http2ClientChannel::new(uri);
    let client = RpcClient::<TestService, C>::new(client);

    macro_rules! assert_matches {
        ($e:expr, $p:pat) => {
            assert!(
                matches!($e, $p),
                "expected {} to match {}",
                stringify!($e),
                stringify!($p)
            );
        };
    }
    macro_rules! assert_server_result {
        ($p:pat) => {
            let server_result = server_results.recv_async().await.unwrap();
            assert!(
                matches!(server_result, $p),
                "expected server result to match {}",
                stringify!($p)
            );
            assert!(server_results.is_empty());
        };
    }

    // small enough - should succeed
    let res = client.rpc(BigRequest(vec![0; 10_000_000])).await;
    assert_matches!(res, Ok(()));
    assert_server_result!(Ok(()));

    // too big - should fail immediately after opening a connection
    let res = client.rpc(BigRequest(vec![0; 20_000_000])).await;
    assert_matches!(
        res,
        Err(RpcClientError::Send(http2::SendError::SizeError(_)))
    );
    assert_server_result!(Err(RpcServerError::EarlyClose));

    // not serializable - should fail immediately after opening a connection
    let res = client.rpc(NoSerRequest(NoSer)).await;
    assert_matches!(
        res,
        Err(RpcClientError::Send(http2::SendError::SerializeError(_)))
    );
    assert_server_result!(Err(RpcServerError::EarlyClose));

    // not deserializable - should fail on the server side
    let res = client.rpc(NoDeserRequest(NoDeser)).await;
    assert_matches!(res, Err(RpcClientError::EarlyClose));
    assert_server_result!(Err(RpcServerError::RecvError(
        http2::RecvError::DeserializeError(_)
    )));

    // response not serializable - should fail on the server side
    let res = client.rpc(NoSerResponseRequest).await;
    assert_matches!(res, Err(RpcClientError::EarlyClose));
    assert_server_result!(Err(RpcServerError::SendError(
        http2::SendError::SerializeError(_)
    )));

    // response not deserializable - should succeed on the server side fail on the client side
    let res = client.rpc(NoDeserResponseRequest).await;
    assert_matches!(
        res,
        Err(RpcClientError::RecvError(RecvError::DeserializeError(_)))
    );
    assert_server_result!(Ok(()));

    // response small - should succeed
    let res = client.rpc(BigResponseRequest(10_000_000)).await;
    assert_matches!(res, Ok(_));
    assert_server_result!(Ok(()));

    // response big - should fail
    let res = client.rpc(BigResponseRequest(20_000_000)).await;
    assert_matches!(res, Err(RpcClientError::EarlyClose));
    assert_server_result!(Err(RpcServerError::SendError(http2::SendError::SizeError(
        _
    ))));

    println!("terminating server");
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}
