use futures::{Future, Sink, Stream, StreamExt, SinkExt};
use std::result;
pub mod mem;
pub mod mem_and_quinn;
pub mod mem_or_quinn;
pub mod quinn;

trait InteractionPattern {}

trait Msg<Service, Req, Res>: Into<Req> {
    type Request: Send + Into<Req>;
    type Response: Send + TryFrom<Res>;
    type Pattern: InteractionPattern;
}

struct FireAndForget;
impl InteractionPattern for FireAndForget {}
struct Rpc;
impl InteractionPattern for Rpc {}
struct ClientStreaming;
impl InteractionPattern for ClientStreaming {}
struct ServerStreaming;
impl InteractionPattern for ServerStreaming {}
struct BidiStreaming;
impl InteractionPattern for BidiStreaming {}

pub trait Channel<Req, Res> {
    /// Sink type
    type ReqSink: Sink<Req, Error = Self::SendError>;
    /// Stream type
    type ResStream: Stream<Item = result::Result<Res, Self::RecvError>>;
    /// Error you might get while sending messages from a stream
    type SendError;
    /// Error you might get while receiving messages from a stream
    type RecvError;
    /// Error you might get when opening a new stream
    type OpenBiError;
    /// Future returned by open_bi
    type OpenBiFuture<'a>: Future<Output = result::Result<(Self::ReqSink, Self::ResStream), Self::OpenBiError>>
        + 'a
    where
        Self: 'a;
    /// Open a bidirectional stream
    fn open_bi(&mut self) -> Self::OpenBiFuture<'_>;
    /// Error you might get when waiting for new streams
    type AcceptBiError;
    /// Future returned by accept_bi
    type AcceptBiFuture<'a>: Future<Output = result::Result<(Self::ReqSink, Self::ResStream), Self::AcceptBiError>>
        + 'a
    where
        Self: 'a;
    /// Accept a bidirectional stream
    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_>;
}

// trait Service {}
// struct MyService;

// enum Req {
//     FireAndForget(FireAndForget),
// }

// enum Res {

// }

// impl Msg<MyService, Req, Res> for FireAndForget {
//     type Request = Never;
//     type Response = ();
//     type Pattern = FireAndForget;
// }

// trait Client<S: Service, Req, Res> {
//     fn send<M>(&self, msg: M)
//     where
//         M: Msg<S, Req, Res, Pattern = FireAndForget>;

//     type RpcFuture<T>: Future<Output = T> + Send;
//     fn rpc<M>(msg: M) -> Self::RpcFuture<M::Response>
//     where
//         M: Msg<S, Req, Res, Pattern = Rpc>;

//     type ClientSink<T>: Sink<T> + Send;
//     type ClientFuture<T>: Future<Output = T> + Send;
//     fn client_streaming<M>(
//         msg: M,
//     ) -> (
//         Self::ClientSink<M::Request>,
//         Self::ClientFuture<M::Response>,
//     )
//     where
//         M: Msg<S, Req, Res, Pattern = ClientStreaming>;

//     type ServerStream<T>: Stream<Item = T> + Send;
//     fn server_streaming<M>(msg: M) -> Self::ServerStream<M::Response>
//     where
//         M: Msg<S, Req, Res, Pattern = ServerStreaming>;

//     type BidiSink<T>: Sink<T> + Send;
//     type BidiStream<T>: Stream<Item = T> + Send;
//     fn bidi_streaming<M>(
//         msg: M,
//     ) -> (
//         Self::BidiSink<M::Request>,
//         Self::BidiStream<M::Response>,
//     )
//     where
//         M: Msg<S, Req, Res, Pattern = BidiStreaming>;
// }

// enum BackChannel<Req, Res> {
//     None,
//     Rpc(Rpc),
//     ClientStreaming(ClientStreaming),
//     ServerStreaming(ServerStreaming),
//     BidiStreaming(BidiStreaming),
// }

// impl<U, Req, Res, S: Service> Client<S, Req, Res> for tokio::sync::mpsc::Sender<Req> {
//     fn send<M>(&self, msg: M)
//     where
//         M: Msg<S, Req, Res, Pattern = FireAndForget> {
//         tokio::sync::mpsc::Sender::send(self, msg.into()).ok();
//     }

//     type RpcFuture<T> = BoxFuture<'static, T>;

//     fn rpc<M>(msg: M) -> Self::RpcFuture<M::Response>
//     where
//         M: Msg<S, Req, Res, Pattern = Rpc>,
//     {
//         // send and sift through responses
//         todo!()
//     }

//     type ClientSink<T>;

//     type ClientFuture<T>;

//     fn client_streaming<M>(
//         msg: M,
//     ) -> (
//         Self::ClientSink<M::Request>,
//         Self::ClientFuture<M::Response>,
//     )
//     where
//         M: Msg<S, Pattern = ClientStreaming> {
//         // send and send more, sift through responses for one msg
//         todo!()
//     }

//     type ServerStream<T>;

//     fn server_streaming<M>(msg: M) -> Self::ServerStream<M::Response>
//     where
//         M: Msg<S, Pattern = ServerStreaming> {
//         todo!()
//         // send and sift through responses until end
//     }

//     type BidiSink<T>;

//     type BidiStream<T>;

//     fn bidi_streaming<M>(
//         msg: M,
//     ) -> (
//         Self::BidiSink<M::Request>,
//         Self::BidiStream<M::Response>,
//     )
//     where
//         M: Msg<S, Pattern = BidiStreaming> {
//         // send and send more, sift through responses until end
//         todo!()
//     }
// }

#[tokio::main]
async fn main() {
    let (mut server, mut client) = mem::connection::<String, u64>(1);
    let to_string_service = tokio::spawn(async move {
        let (mut send, mut recv) = server.accept_bi().await.unwrap();
        while let Some(item) = recv.next().await {
            let item = item.unwrap();
            println!("server got: {:?}", item);
            send.send(item.to_string()).await.unwrap();
        }
    });
    let (mut send, mut recv) = client.open_bi().await.unwrap();
    let print_result_service = tokio::spawn(async move {
        while let Some(item) = recv.next().await {
            let item = item.unwrap();
            println!("got result: {}", item);
        }
    });
    for i in 0..100 {
        send.send(i).await.unwrap();
    }
    drop(send);
    to_string_service.await.unwrap();
    print_result_service.await.unwrap();
}
