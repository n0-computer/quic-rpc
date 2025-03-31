use std::{io, sync::Arc};

use iroh::endpoint::{ConnectionError, RecvStream, SendStream};
use quic_rpc::{
    RequestError,
    rpc::{Handler, RemoteConnection, RemoteRead, RemoteWrite},
    util::AsyncReadVarintExt,
};

/// A connection to a remote service.
///
/// Initially this does just have the endpoint and the address. Once a
/// connection is established, it will be stored.
#[derive(Debug, Clone)]
pub struct IrohRemoteConnection(Arc<IrohRemoteConnectionInner>);

#[derive(Debug)]
struct IrohRemoteConnectionInner {
    endpoint: iroh::Endpoint,
    addr: iroh::NodeAddr,
    connection: tokio::sync::Mutex<Option<iroh::endpoint::Connection>>,
    alpn: Vec<u8>,
}

impl IrohRemoteConnection {
    pub fn new(endpoint: iroh::Endpoint, addr: iroh::NodeAddr, alpn: Vec<u8>) -> Self {
        Self(Arc::new(IrohRemoteConnectionInner {
            endpoint,
            addr,
            connection: Default::default(),
            alpn,
        }))
    }
}

impl RemoteConnection for IrohRemoteConnection {
    fn clone_boxed(&self) -> Box<dyn RemoteConnection> {
        Box::new(self.clone())
    }

    fn open_bi(&self) -> BoxedFuture<std::result::Result<(SendStream, RecvStream), RequestError>> {
        let this = self.0.clone();
        Box::pin(async move {
            let mut guard = this.connection.lock().await;
            let pair = match guard.as_mut() {
                Some(conn) => {
                    // try to reuse the connection
                    match conn.open_bi().await {
                        Ok(pair) => pair,
                        Err(_) => {
                            // try with a new connection, just once
                            *guard = None;
                            connect_and_open_bi(&this.endpoint, &this.addr, &this.alpn, guard)
                                .await
                                .map_err(RequestError::Other)?
                        }
                    }
                }
                None => connect_and_open_bi(&this.endpoint, &this.addr, &this.alpn, guard)
                    .await
                    .map_err(RequestError::Other)?,
            };
            Ok(pair)
        })
    }
}

async fn connect_and_open_bi(
    endpoint: &iroh::Endpoint,
    addr: &iroh::NodeAddr,
    alpn: &[u8],
    mut guard: tokio::sync::MutexGuard<'_, Option<iroh::endpoint::Connection>>,
) -> anyhow::Result<(SendStream, RecvStream)> {
    let conn = endpoint.connect(addr.clone(), alpn).await?;
    let (send, recv) = conn.open_bi().await?;
    *guard = Some(conn);
    Ok((send, recv))
}

mod wasm_browser {
    #![allow(dead_code)]
    pub(crate) type BoxedFuture<'a, T> =
        std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;
}
mod multithreaded {
    #![allow(dead_code)]
    pub(crate) type BoxedFuture<'a, T> =
        std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
}
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
use multithreaded::*;
use serde::de::DeserializeOwned;
use tokio::task::JoinSet;
use tracing::{Instrument, trace, trace_span, warn};
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
use wasm_browser::*;

/// Utility function to listen for incoming connections and handle them with the provided handler
pub async fn listen<R: DeserializeOwned + 'static>(endpoint: iroh::Endpoint, handler: Handler<R>) {
    let mut request_id = 0u64;
    let mut tasks = JoinSet::new();
    while let Some(incoming) = endpoint.accept().await {
        let handler = handler.clone();
        let fut = async move {
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(cause) => {
                    warn!("failed to accept connection {cause:?}");
                    return io::Result::Ok(());
                }
            };
            loop {
                let (send, mut recv) = match connection.accept_bi().await {
                    Ok((s, r)) => (s, r),
                    Err(ConnectionError::ApplicationClosed(cause))
                        if cause.error_code.into_inner() == 0 =>
                    {
                        trace!("remote side closed connection {cause:?}");
                        return Ok(());
                    }
                    Err(cause) => {
                        warn!("failed to accept bi stream {cause:?}");
                        return Err(cause.into());
                    }
                };
                let size = recv.read_varint_u64().await?.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "failed to read size")
                })?;
                let mut buf = vec![0; size as usize];
                recv.read_exact(&mut buf)
                    .await
                    .map_err(|e| io::Error::new(io::ErrorKind::UnexpectedEof, e))?;
                let msg: R = postcard::from_bytes(&buf)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                let rx = RemoteRead::new(recv);
                let tx = RemoteWrite::new(send);
                handler(msg, rx, tx).await?;
            }
        };
        let span = trace_span!("rpc", id = request_id);
        tasks.spawn(fut.instrument(span));
        request_id += 1;
    }
}
