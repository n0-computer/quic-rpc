//! Custom quinn transport that uses the interprocess crate to provide
//! local interprocess communication via either Unix domain sockets or
//! Windows named pipes.
use std::{
    fmt::{self, Debug},
    io,
    net::SocketAddr,
    ops::Deref,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{self, Context, Poll, Waker},
};

use bytes::{Buf, Bytes, BytesMut};
use futures::{SinkExt, Stream, StreamExt};
use quinn::{AsyncUdpSocket, Endpoint};
use quinn_udp::{RecvMeta, Transmit};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    select,
    sync::mpsc,
    task::JoinHandle,
};
use tracing::{info, trace};

#[derive(Debug)]
pub(super) enum NetworkReadResult {
    Error(io::Error),
    Ok {
        meta: quinn_udp::RecvMeta,
        bytes: Bytes,
    },
}

///
#[derive(Debug)]
pub struct Inner {
    /// Sends network messages.
    network_sender: mpsc::Sender<Vec<quinn_udp::Transmit>>,
    /// Used for receiving DERP messages.
    network_recv_ch: flume::Receiver<NetworkReadResult>,
    /// Stores wakers, to be called when derp_recv_ch receives new data.
    network_recv_wakers: std::sync::Mutex<Option<Waker>>,
    pub(super) network_send_wakers: std::sync::Mutex<Option<Waker>>,
    local_addr: SocketAddr,
    name: String,
}

///
#[derive(Clone, Debug)]
pub struct Conn {
    inner: Arc<Inner>,
}

impl Deref for Conn {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// A simple iterator to group [`Transmit`]s by destination.
struct TransmitIter<'a> {
    transmits: &'a [quinn_udp::Transmit],
    offset: usize,
}

impl<'a> TransmitIter<'a> {
    fn new(transmits: &'a [quinn_udp::Transmit]) -> Self {
        TransmitIter {
            transmits,
            offset: 0,
        }
    }
}

impl Iterator for TransmitIter<'_> {
    type Item = Vec<quinn_udp::Transmit>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset == self.transmits.len() {
            return None;
        }
        let current_dest = &self.transmits[self.offset].destination;
        let mut end = self.offset;
        for t in &self.transmits[self.offset..] {
            if current_dest != &t.destination {
                break;
            }
            end += 1;
        }

        let out = self.transmits[self.offset..end].to_vec();
        self.offset = end;
        Some(out)
    }
}

impl AsyncUdpSocket for Conn {
    fn poll_send(
        &self,
        _udp_state: &quinn_udp::UdpState,
        cx: &mut Context,
        transmits: &[quinn_udp::Transmit],
    ) -> Poll<io::Result<usize>> {
        let mut n = 0;
        if transmits.is_empty() {
            return Poll::Ready(Ok(n));
        }

        // Split up transmits by destination, as the rest of the code assumes single dest.
        let groups = TransmitIter::new(transmits);
        for group in groups {
            match self.network_sender.try_reserve() {
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // TODO: add counter?
                    self.network_send_wakers
                        .lock()
                        .unwrap()
                        .replace(cx.waker().clone());
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "connection closed",
                    )));
                }
                Ok(permit) => {
                    n += group.len();
                    permit.send(group);
                }
            }
        }
        if n > 0 {
            return Poll::Ready(Ok(n));
        }

        Poll::Pending
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        metas: &mut [quinn_udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        // FIXME: currently ipv4 load results in ipv6 traffic being ignored
        debug_assert_eq!(bufs.len(), metas.len(), "non matching bufs & metas");
        trace!("{} poll_recv called {}", self.name, bufs.len());

        let mut num_msgs = 0;
        for (buf_out, meta_out) in bufs.iter_mut().zip(metas.iter_mut()) {
            match self.network_recv_ch.try_recv() {
                Err(flume::TryRecvError::Empty) => {
                    self.network_recv_wakers
                        .lock()
                        .unwrap()
                        .replace(cx.waker().clone());
                    break;
                }
                Err(flume::TryRecvError::Disconnected) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "connection closed",
                    )));
                }
                Ok(dm) => {
                    match dm {
                        NetworkReadResult::Error(err) => {
                            return Poll::Ready(Err(err));
                        }
                        NetworkReadResult::Ok { bytes, meta } => {
                            buf_out[..bytes.len()].copy_from_slice(&bytes);
                            *meta_out = meta;
                        }
                    }

                    num_msgs += 1;
                }
            }
        }

        // If we have any msgs to report, they are in the first `num_msgs_total` slots
        if num_msgs > 0 {
            trace!("{} received {} msgs", self.name, num_msgs);
            return Poll::Ready(Ok(num_msgs));
        }

        trace!("{} poll_recv pending", self.name);
        Poll::Pending
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local_addr)
    }
}

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
        tracing::debug!("poll_send");
        let res = self.0.lock().unwrap().poll_send(state, cx, transmits);
        tracing::debug!("end poll_send");
        res
    }

    fn poll_recv(
        &self,
        cx: &mut task::Context,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> task::Poll<io::Result<usize>> {
        tracing::debug!("poll_recv");
        let res = self.0.lock().unwrap().poll_recv(cx, bufs, meta);
        tracing::debug!("end poll_recv");
        res
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.lock().unwrap().local_addr()
    }
}

impl FlumeSocketInner {
    /// Create a pair of connected sockets.
    fn pair(local: SocketAddr, remote: SocketAddr) -> (Self, Self) {
        let (tx1, rx1) = flume::bounded(16);
        let (tx2, rx2) = flume::bounded(16);

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
        tracing::debug!("{} poll_send", self.local);
        if transmits.is_empty() {
            return task::Poll::Ready(Ok(0));
        }
        for transmit in transmits.iter() {
            tracing::debug!(
                "{} sending packet to {} with ECN {:?}",
                self.local,
                transmit.destination,
                transmit.ecn,
            );
        }
        let mut offset = 0;
        let mut pending = false;
        for transmit in transmits {
            trace!("{} send {}", self.local, hex::encode(&transmit.contents));
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
            tracing::debug!("{} poll_send returned ready {}", self.local, offset);
            task::Poll::Ready(Ok(offset))
        } else if pending {
            // only return pending if we got a pending for the first slot
            tracing::debug!("{} poll_send returned pending", self.local);
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
        tracing::debug!("{} poll_recv", self.local);
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
                trace!("{} recv {}", self.local, hex::encode(&packet.data));
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
            for i in 0..offset {
                tracing::debug!(
                    "{} received packet to {:?} with ecn {:?}",
                    self.local,
                    meta[i].dst_ip,
                    meta[i].ecn
                );
            }
            tracing::debug!("{} poll_recv returned ready {}", self.local, offset);
            task::Poll::Ready(Ok(offset))
        } else if pending {
            // only return pending if we got a pending for the first slot
            tracing::debug!("{} poll_recv returned pending", self.local);
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

fn make_endpoint_conn(
    socket: Conn,
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
pub fn endpoint_pair_conn(
    a: SocketAddr,
    ac: Option<quinn::ServerConfig>,
    b: SocketAddr,
    bc: Option<quinn::ServerConfig>,
) -> io::Result<(quinn::Endpoint, quinn::Endpoint)> {
    let (a_out_sender, mut a_out_receiver) = mpsc::channel::<Vec<Transmit>>(32);
    let (b_out_sender, mut b_out_receiver) = mpsc::channel::<Vec<Transmit>>(32);
    let (a_in_sender, a_in_receiver) = flume::bounded(32);
    let (b_in_sender, b_in_receiver) = flume::bounded(32);
    let socket_a = Conn {
        inner: Arc::new(Inner {
            network_sender: a_out_sender,
            network_recv_ch: a_in_receiver,
            network_recv_wakers: Mutex::new(None),
            network_send_wakers: Mutex::new(None),
            local_addr: a,
            name: "a".to_string(),
        }),
    };
    let socket_b = Conn {
        inner: Arc::new(Inner {
            network_sender: b_out_sender,
            network_recv_ch: b_in_receiver,
            network_recv_wakers: Mutex::new(None),
            network_send_wakers: Mutex::new(None),
            local_addr: b,
            name: "b".to_string(),
        }),
    };
    let socket_b2 = socket_b.clone();
    tokio::spawn(async move {
        while let Some(msg) = a_out_receiver.recv().await {
            for transmit in msg {
                let len = transmit.contents.len();
                trace!("a -> b {}", len);
                b_in_sender
                    .send(NetworkReadResult::Ok {
                        meta: RecvMeta {
                            addr: a,
                            len,
                            stride: len,
                            ecn: None,
                            dst_ip: Some(b.ip()),
                        },
                        bytes: transmit.contents,
                    })
                    .unwrap();
            }
            if let Some(waker) = socket_b2.inner.network_recv_wakers.lock().unwrap().take() {
                waker.wake();
            }
        }
    });
    let socket_a2 = socket_a.clone();
    tokio::spawn(async move {
        while let Some(msg) = b_out_receiver.recv().await {
            for transmit in msg {
                trace!("b -> a {}", transmit.contents.len());
                let len = transmit.contents.len();
                a_in_sender
                    .send(NetworkReadResult::Ok {
                        meta: RecvMeta {
                            addr: b,
                            len,
                            stride: len,
                            ecn: None,
                            dst_ip: Some(a.ip()),
                        },
                        bytes: transmit.contents,
                    })
                    .unwrap();
            }
            if let Some(waker) = socket_a2.inner.network_recv_wakers.lock().unwrap().take() {
                waker.wake();
            }
        }
    });
    Ok((
        make_endpoint_conn(socket_a, ac)?,
        make_endpoint_conn(socket_b, bc)?,
    ))
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
