use futures::{Future, Sink, SinkExt, Stream, StreamExt};
use std::{fmt::Debug, result};
use sugar::ClientChannel;
pub mod mem;
pub mod mem_and_quinn;
pub mod mem_or_quinn;
pub mod quinn;
pub mod sugar;

/// An abstract channel to a service
///
/// This assumes cheap streams, so every interaction uses a new stream.
pub trait Channel<Req, Res> {
    /// Sink type
    type ReqSink: Sink<Req, Error = Self::SendError>;
    /// Stream type
    type ResStream: Stream<Item = result::Result<Res, Self::RecvError>>;
    /// Error you might get while sending messages from a stream
    type SendError: Debug;
    /// Error you might get while receiving messages from a stream
    type RecvError: Debug;
    /// Error you might get when opening a new stream
    type OpenBiError: Debug;
    /// Future returned by open_bi
    type OpenBiFuture<'a>: Future<Output = result::Result<(Self::ReqSink, Self::ResStream), Self::OpenBiError>>
        + 'a
    where
        Self: 'a;
    /// Open a bidirectional stream
    fn open_bi(&mut self) -> Self::OpenBiFuture<'_>;
    /// Error you might get when waiting for new streams
    type AcceptBiError: Debug;
    /// Future returned by accept_bi
    type AcceptBiFuture<'a>: Future<Output = result::Result<(Self::ReqSink, Self::ResStream), Self::AcceptBiError>>
        + 'a
    where
        Self: 'a;
    /// Accept a bidirectional stream
    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_>;
}

async fn main_sugared() -> anyhow::Result<()> {
    let (mut server, mut client) = mem::connection::<String, u64>(1);
    // let client = ClientChannel::new(client);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (mut server, mut client) = mem::connection::<String, u64>(1);
    let to_string_service = tokio::spawn(async move {
        let (mut send, mut recv) = server.accept_bi().await?;
        while let Some(item) = recv.next().await {
            let item = item?;
            println!("server got: {:?}", item);
            send.send(item.to_string()).await?;
        }
        anyhow::Ok(())
    });
    let (mut send, mut recv) = client.open_bi().await?;
    let print_result_service = tokio::spawn(async move {
        while let Some(item) = recv.next().await {
            let item = item?;
            println!("got result: {}", item);
        }
        anyhow::Ok(())
    });
    for i in 0..100 {
        send.send(i).await?;
    }
    drop(send);
    to_string_service.await??;
    print_result_service.await??;
    Ok(())
}
