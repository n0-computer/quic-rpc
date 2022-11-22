use anyhow::Context;
use async_stream::stream;
use futures::stream::{Stream, StreamExt};
use quic_rpc::{quinn::QuinnChannelTypes, server::spawn_server};
use quinn::{Endpoint, ServerConfig};
use std::net::SocketAddr;
use std::sync::Arc;

use types::store::*;

#[derive(Clone)]
pub struct Store;

types::create_store_dispatch!(Store, dispatch_store_request);

impl Store {
    async fn put(self, _put: Put) -> PutResponse {
        PutResponse([0; 32])
    }

    async fn get(self, _get: Get) -> GetResponse {
        GetResponse(vec![])
    }

    async fn put_file(
        self,
        _put: PutFile,
        updates: impl Stream<Item = PutFileUpdate>,
    ) -> PutFileResponse {
        tokio::pin!(updates);
        while let Some(_update) = updates.next().await {}
        PutFileResponse([0; 32])
    }

    fn get_file(self, _get: GetFile) -> impl Stream<Item = GetFileResponse> + Send + 'static {
        stream! {
            for i in 0..3 {
                yield GetFileResponse(vec![i]);
            }
        }
    }

    fn convert_file(
        self,
        _convert: ConvertFile,
        updates: impl Stream<Item = ConvertFileUpdate> + Send + 'static,
    ) -> impl Stream<Item = ConvertFileResponse> + Send + 'static {
        stream! {
            tokio::pin!(updates);
            while let Some(msg) = updates.next().await {
                yield ConvertFileResponse(msg.0);
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server_addr: SocketAddr = "127.0.0.1:12345".parse()?;
    let (server, _server_certs) = make_server_endpoint(server_addr)?;
    let target = Store;
    let accept = server.accept().await.context("accept failed")?.await?;
    let connection = quic_rpc::quinn::Channel::new(accept);
    let server_handle = spawn_server(
        StoreService,
        QuinnChannelTypes,
        connection,
        target,
        dispatch_store_request,
    );
    server_handle.await??;
    Ok(())
}

fn make_server_endpoint(bind_addr: SocketAddr) -> anyhow::Result<(Endpoint, Vec<u8>)> {
    let (server_config, server_cert) = configure_server()?;
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    Ok((endpoint, server_cert))
}

fn configure_server() -> anyhow::Result<(ServerConfig, Vec<u8>)> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_der = cert.serialize_der()?;
    let priv_key = cert.serialize_private_key_der();
    let priv_key = rustls::PrivateKey(priv_key);
    let cert_chain = vec![rustls::Certificate(cert_der.clone())];

    let mut server_config = ServerConfig::with_single_cert(cert_chain, priv_key)?;
    Arc::get_mut(&mut server_config.transport)
        .unwrap()
        .max_concurrent_uni_streams(0_u8.into());

    Ok((server_config, cert_der))
}
