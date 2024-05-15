//! Custom quinn transport that uses the interprocess crate to provide
//! local interprocess communication via either Unix domain sockets or
//! Windows named pipes.
use std::{io, net::SocketAddr, path::Path};

use super::quinn_flume_socket::{make_endpoint, FlumeSocket, Packet};
use bytes::{Buf, Bytes, BytesMut};
use futures::StreamExt;
use interprocess::local_socket::{GenericFilePath, GenericNamespaced, Name, ToFsName, ToNsName};
use quinn::{Endpoint, EndpointConfig};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    task::JoinHandle,
};

struct FrameIter<'a>(&'a mut BytesMut);

impl<'a> Iterator for FrameIter<'a> {
    type Item = Bytes;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.len() < 2 {
            return None;
        }
        let len = u16::from_le_bytes([self.0[0], self.0[1]]) as usize;
        if self.0.len() < len + 2 {
            return None;
        }
        self.0.advance(2);
        Some(self.0.split_to(len).freeze())
    }
}

/// Automatically chooses name type based on OS support and preference.
pub fn new_socket_name(root: impl AsRef<Path>, id: &str) -> io::Result<Name<'static>> {
    if cfg!(windows) {
        format!("@quic-rpc-socket-{}.sock", id).to_ns_name::<GenericNamespaced>()
    } else if cfg!(unix) {
        root.as_ref()
            .join(format!("{id}.sock"))
            .to_fs_name::<GenericFilePath>()
    } else {
        panic!("unsupported OS");
    }
}

/// Wrap a tokio read/write pair as a quinn endpoint.
///
/// The connection is assumed to be from `local` to `remote`. If you try to
/// connect to any other address, packets will be dropped.
#[allow(clippy::type_complexity)]
pub fn tokio_io_endpoint<R, W>(
    mut r: R,
    mut w: W,
    local: SocketAddr,
    remote: SocketAddr,
    server_config: Option<quinn::ServerConfig>,
) -> io::Result<(
    Endpoint,
    JoinHandle<io::Result<()>>,
    JoinHandle<io::Result<()>>,
)>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (out_send, out_recv) = flume::bounded::<Packet>(32);
    let (in_send, in_recv) = flume::bounded::<Packet>(32);
    let mut out_recv = out_recv.into_stream().ready_chunks(16);
    let sender = tokio::task::spawn(async move {
        tracing::debug!("{} running forwarder task to {}", local, remote);
        while let Some(packets) = out_recv.next().await {
            for packet in packets {
                if packet.to == remote {
                    let contents = packet.contents.as_ref();
                    if let Some(segment_size) = packet.segment_size {
                        for min in (0..contents.len()).step_by(segment_size) {
                            let max = (min + segment_size).min(contents.len());
                            let len: u16 = (max - min).try_into().unwrap();
                            w.write_all(&len.to_le_bytes()).await?;
                            w.write_all(&contents[min..max]).await?;
                        }
                    } else {
                        let len: u16 = contents.len().try_into().unwrap();
                        tracing::debug!("sending {}bytes", len);
                        w.write_all(&len.to_le_bytes()).await?;
                        w.write_all(contents).await?;
                    }
                } else {
                    // not for us, ignore
                    continue;
                }
            }
        }
        Ok(())
    });
    let receiver = tokio::task::spawn(async move {
        let mut buffer = BytesMut::with_capacity(65535);
        loop {
            // read more data and split into frames
            let n = r.read_buf(&mut buffer).await;
            let n = n?;
            tracing::debug!("read {}bytes", n);
            if n == 0 {
                // eof
                break;
            }
            // split into frames and send all full frames
            for item in FrameIter(&mut buffer) {
                let packet = Packet {
                    from: remote,
                    to: local,
                    contents: item,
                    segment_size: None,
                };
                if in_send.send_async(packet).await.is_err() {
                    // in_recv dropped
                    break;
                }
            }
        }
        Ok(())
    });
    let socket = FlumeSocket::new(local, out_send, in_recv);
    let config = EndpointConfig::default();
    let endpoint = make_endpoint(socket, config, server_config)?;
    Ok((endpoint, sender, receiver))
}
