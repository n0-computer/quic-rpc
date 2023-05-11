//! Custom quinn transport that uses the interprocess crate to provide
//! local interprocess communication via either Unix domain sockets or
//! Windows named pipes.
use std::{
    fmt::{self, Debug},
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task,
};

use bytes::{Buf, Bytes, BytesMut};
use futures::{SinkExt, Stream, StreamExt};
use quinn::{AsyncUdpSocket, Endpoint};
use quinn_udp::RecvMeta;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    select,
    task::JoinHandle,
};

struct FlumeSocketInner {
    local: SocketAddr,
    receiver: flume::r#async::RecvStream<'static, Packet>,
    sender: flume::r#async::SendSink<'static, Packet>,
}

impl fmt::Debug for FlumeSocketInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamSocketInner")
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

/// A packet for the flume socket.
struct Packet {
    from: SocketAddr,
    to: SocketAddr,
    data: Bytes,
}

#[derive(Debug)]
pub(crate) struct FlumeSocket(Arc<Mutex<FlumeSocketInner>>);

#[derive(Debug)]
pub(crate) struct LocalAddrHandle(Arc<Mutex<FlumeSocketInner>>);

impl LocalAddrHandle {
    pub fn set(&self, addr: SocketAddr) {
        self.0.lock().unwrap().local = addr;
    }

    pub fn get(&self) -> SocketAddr {
        self.0.lock().unwrap().local
    }
}

impl AsyncUdpSocket for FlumeSocket {
    fn poll_send(
        &self,
        state: &quinn_udp::UdpState,
        cx: &mut task::Context,
        transmits: &[quinn_udp::Transmit],
    ) -> task::Poll<Result<usize, io::Error>> {
        self.0.lock().unwrap().poll_send(state, cx, transmits)
    }

    fn poll_recv(
        &self,
        cx: &mut task::Context,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> task::Poll<io::Result<usize>> {
        self.0.lock().unwrap().poll_recv(cx, bufs, meta)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.lock().unwrap().local_addr()
    }
}

impl FlumeSocketInner {
    /// Create a pair of connected sockets.
    fn pair(local: SocketAddr, remote: SocketAddr) -> (Self, Self) {
        let (tx1, rx1) = flume::unbounded();
        let (tx2, rx2) = flume::unbounded();

        let a = Self {
            receiver: rx1.into_stream(),
            sender: tx2.into_sink(),
            local,
        };

        let b = Self {
            receiver: rx2.into_stream(),
            sender: tx1.into_sink(),
            local: remote,
        };

        (a, b)
    }
}

impl FlumeSocketInner {
    fn poll_send(
        &mut self,
        _state: &quinn_udp::UdpState,
        cx: &mut task::Context,
        transmits: &[quinn_udp::Transmit],
    ) -> task::Poll<Result<usize, std::io::Error>> {
        if transmits.is_empty() {
            return task::Poll::Ready(Ok(0));
        }
        tracing::debug!("sending {} packets", transmits.len());
        let mut offset = 0;
        let mut pending = false;
        for transmit in transmits {
            let item = Packet {
                from: self.local,
                to: transmit.destination,
                data: transmit.contents.clone(),
            };
            match self.sender.poll_ready_unpin(cx) {
                task::Poll::Ready(Ok(())) => {
                    // ready to send
                    if self.sender.start_send_unpin(item).is_err() {
                        // disconnected
                        break;
                    }
                }
                task::Poll::Ready(Err(_)) => {
                    // disconneced
                    break;
                }
                task::Poll::Pending => {
                    // remember the offset of the first pending transmit
                    pending = true;
                    break;
                }
            }
            offset += 1;
        }
        if offset > 0 {
            // report how many transmits we sent
            task::Poll::Ready(Ok(offset))
        } else if pending {
            // only return pending if we got a pending for the first slot
            task::Poll::Pending
        } else {
            task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "all receivers dropped",
            )))
        }
    }

    fn poll_recv(
        &mut self,
        cx: &mut std::task::Context,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [quinn_udp::RecvMeta],
    ) -> task::Poll<io::Result<usize>> {
        let n = bufs.len().min(meta.len());
        if n == 0 {
            return task::Poll::Ready(Ok(0));
        }
        let mut offset = 0;
        let mut pending = false;
        // try to fill as many slots as possible
        while offset < n {
            let packet = match Pin::new(&mut self.receiver).poll_next(cx) {
                task::Poll::Ready(Some(recv)) => recv,
                task::Poll::Ready(None) => break,
                task::Poll::Pending => {
                    pending = true;
                    break;
                }
            };
            if packet.to == self.local {
                let len = packet.data.len();
                bufs[offset][..len].copy_from_slice(&packet.data);
                meta[offset] = quinn_udp::RecvMeta {
                    addr: packet.from,
                    len,
                    stride: len,
                    ecn: None,
                    dst_ip: Some(self.local.ip()),
                };
                offset += 1;
            } else {
                // not for us, ignore
                continue;
            }
        }
        if offset > 0 {
            // report how many slots we filled
            tracing::debug!("received {} packets", n);
            task::Poll::Ready(Ok(offset))
        } else if pending {
            // only return pending if we got a pending for the first slot
            task::Poll::Pending
        } else {
            task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "all senders dropped",
            )))
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.local)
    }
}

fn make_endpoint(
    socket: FlumeSocket,
    server_config: Option<quinn::ServerConfig>,
) -> io::Result<quinn::Endpoint> {
    let config = quinn::EndpointConfig::default();
    quinn::Endpoint::new_with_abstract_socket(
        config,
        server_config,
        socket,
        Arc::new(quinn::TokioRuntime),
    )
}

/// Create a pair of directly connected endpoints.
///
/// Useful for testing.
pub fn endpoint_pair(
    a: SocketAddr,
    ac: Option<quinn::ServerConfig>,
    b: SocketAddr,
    bc: Option<quinn::ServerConfig>,
) -> io::Result<(quinn::Endpoint, quinn::Endpoint)> {
    let (socket_a, socket_b) = FlumeSocketInner::pair(a, b);
    let socket_a = FlumeSocket(Arc::new(Mutex::new(socket_a)));
    let socket_b = FlumeSocket(Arc::new(Mutex::new(socket_b)));
    Ok((make_endpoint(socket_a, ac)?, make_endpoint(socket_b, bc)?))
}

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

/// Wrap a tokio read/write pair as a quinn endpoint.
///
/// The connection is assumed to be from `local` to `remote`. If you try to
/// connect to any other address, packets will be dropped.
pub fn tokio_io_endpoint<R, W>(
    mut r: R,
    mut w: W,
    local: SocketAddr,
    remote: SocketAddr,
    server_config: Option<quinn::ServerConfig>,
) -> io::Result<(Endpoint, JoinHandle<io::Result<()>>)>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (out_send, out_recv) = flume::bounded::<Packet>(32);
    let (in_send, in_recv) = flume::bounded::<Packet>(32);
    let mut out_recv = out_recv.into_stream().ready_chunks(16);
    let task = tokio::task::spawn(async move {
        let mut buffer = BytesMut::with_capacity(65535);
        loop {
            buffer.reserve(1024 * 32);
            select! {
                biased;
                // try to send all pending packets before reading more
                Some(packets) = out_recv.next() => {
                    for packet in packets {
                        if packet.to == remote {
                            let len: u16 = packet.data.len().try_into().unwrap();
                            w.write_all(&len.to_le_bytes()).await?;
                            w.write_all(&packet.data).await?;
                        } else {
                            // not for us, ignore
                            continue;
                        }
                    }
                }
                // read more data and split into frames
                n = r.read_buf(&mut buffer) => {
                    if n? == 0 {
                        // eof
                        break;
                    }
                    // split into frames and send all full frames
                    for item in FrameIter(&mut buffer) {
                        let packet = Packet {
                            from: remote,
                            to: local,
                            data: item,
                        };
                        if in_send.send_async(packet).await.is_err() {
                            // in_recv dropped
                            break;
                        }
                    }
                }
                else => {
                    // out_recv returned None, so out_send was dropped
                    break;
                }
            }
        }
        Ok(())
    });
    let socket = FlumeSocket(Arc::new(Mutex::new(FlumeSocketInner {
        receiver: in_recv.into_stream(),
        sender: out_send.into_sink(),
        local,
    })));
    let endpoint = make_endpoint(socket, server_config)?;
    Ok((endpoint, task))
}
