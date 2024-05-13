//! Custom quinn transport that uses the interprocess crate to provide
//! local interprocess communication via either Unix domain sockets or
//! Windows named pipes.
use std::{
    fmt::{self, Debug},
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
    task,
};

use bytes::Bytes;
use futures::{StreamExt, SinkExt};
use quinn::{AsyncUdpSocket, EndpointConfig};
use quinn_udp::RecvMeta;

struct FlumeSocketInner {
    local: SocketAddr,
    receiver: flume::r#async::RecvStream<'static, Packet>,
    sender: flume::r#async::SendSink<'static, Packet>,
}

impl fmt::Debug for FlumeSocketInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlumeSocketInner")
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

/// A packet for the flume socket.
#[derive(Debug)]
pub struct Packet {
    /// The address the packet was sent from.
    pub from: SocketAddr,
    /// The address the packet is for.
    pub to: SocketAddr,
    /// The data in the packet.
    pub contents: Bytes,
    /// The segment size for the packet.
    pub segment_size: Option<usize>,
}

/// An implementation of [quinn::AsyncUdpSocket] that uses flume channels
#[derive(Debug)]
pub struct FlumeSocket(Arc<Mutex<FlumeSocketInner>>);

impl FlumeSocket {
    /// Create a new flume socket, with the given local address.
    ///
    /// Sent packets will have from set to the local address, and received packets
    /// with to not set to the local address will be ignored.
    pub fn new(local: SocketAddr, tx: flume::Sender<Packet>, rx: flume::Receiver<Packet>) -> Self {
        let inner = FlumeSocketInner {
            receiver: rx.into_stream(),
            sender: tx.into_sink(),
            local,
        };
        Self(Arc::new(Mutex::new(inner)))
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
    fn poll_send(
        &mut self,
        _state: &quinn_udp::UdpState,
        cx: &mut task::Context,
        transmits: &[quinn_udp::Transmit],
    ) -> task::Poll<Result<usize, std::io::Error>> {
        if transmits.is_empty() {
            return task::Poll::Ready(Ok(0));
        }
        let mut offset = 0;
        let mut pending = false;
        tracing::debug!("S {} transmits", transmits.len());
        for transmit in transmits {
            let item = Packet {
                from: self.local,
                to: transmit.destination,
                contents: transmit.contents.clone(),
                segment_size: transmit.segment_size,
            };
            tracing::debug!(
                "S sending {} {:?}",
                transmit.contents.len(),
                transmit.segment_size
            );
            let res = self.sender.poll_ready_unpin(cx);
            match res {
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
            // call poll_flush_unpin only once.
            if let task::Poll::Ready(Err(_)) = self.sender.poll_flush_unpin(cx) {
                // disconnected
                return task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "all receivers dropped",
                )));
            }
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
            let packet = match self.receiver.poll_next_unpin(cx) {
                task::Poll::Ready(Some(recv)) => recv,
                task::Poll::Ready(None) => break,
                task::Poll::Pending => {
                    pending = true;
                    break;
                }
            };
            if packet.to == self.local {
                let len = packet.contents.len();
                let m = quinn_udp::RecvMeta {
                    addr: packet.from,
                    len,
                    stride: packet.segment_size.unwrap_or(len),
                    ecn: None,
                    dst_ip: Some(self.local.ip()),
                };
                tracing::debug!("R bufs {} bytes, {} slots", bufs[offset].len(), n);
                bufs[offset][..len].copy_from_slice(&packet.contents);
                meta[offset] = m;
                offset += 1;
            } else {
                // not for us, ignore
                continue;
            }
        }
        if offset > 0 {
            // report how many slots we filled
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

pub(crate) fn make_endpoint(
    socket: FlumeSocket,
    config: EndpointConfig,
    server_config: Option<quinn::ServerConfig>,
) -> io::Result<quinn::Endpoint> {
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
    server_addr: SocketAddr,
    client_addr: SocketAddr,
    server_config: quinn::ServerConfig,
) -> io::Result<(quinn::Endpoint, quinn::Endpoint)> {
    let (tx1, rx1) = flume::bounded(16);
    let (tx2, rx2) = flume::bounded(16);
    let server = FlumeSocket::new(server_addr, tx1, rx2);
    let client = FlumeSocket::new(client_addr, tx2, rx1);
    let ac = EndpointConfig::default();
    let bc = EndpointConfig::default();
    Ok((
        make_endpoint(server, ac, Some(server_config))?,
        make_endpoint(client, bc, None)?,
    ))
}
