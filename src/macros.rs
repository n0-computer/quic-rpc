//! Macros to reduce boilerplate for RPC implementations.

/// Derive a set of RPC types and message implementation from a declaration.
///
/// The macros are completely optional. They generate the request and response
/// message enums, the service zerosized struct.
/// Optionally, a function can be created to dispatch RPC calls to methods
/// on a struct of your choice.
/// Finally, it can also create a type-safe RPC client for the service.
///
/// Usage is as follows:
///
/// ```no_run
/// # use serde::{Serialize,Deserialize};
/// # use quic_rpc::*;
/// #[derive(Debug, Serialize, Deserialize)]
/// struct Add(pub i32, pub i32);
/// #[derive(Debug, Serialize, Deserialize)]
/// pub struct Sum(pub i32);
/// #[derive(Debug, Serialize, Deserialize)]
/// pub struct Multiply(pub i32);
/// #[derive(Debug, Serialize, Deserialize)]
/// pub struct MultiplyUpdate(pub i32);
/// #[derive(Debug, Serialize, Deserialize)]
/// pub struct MultiplyOutput(pub i32);
///
/// derive_rpc_service! {
///     // Name of the created request enum.
///     Request = MyRequest;
///     // Name of the created response enum.
///     Response = MyResponse;
///     // Name of the created service struct enum.
///     Service = MyService;
///     // Name of the macro to create a dispatch function.
///     // Optional, if not needed pass _ (underscore) as name.
///     CreateDispatch = create_my_dispatch;
///     // Name of the macro to create an RPC client.
///     // Optional, if not needed pass _ (underscore) as name.
///     CreateClient = create_my_client;
///
///     Rpc add = Add, _ -> Sum;
///     BidiStreaming multiply = Multiply, MultiplyUpdate -> MultiplyOutput
/// }
/// ```
///
/// This will generate a request enum `MyRequest`, a response enum `MyRespone`
/// and a service declaration `MyService`.
///
/// It will also generate two macros to create an RPC client and a dispatch function.
///
/// To use the client, invoke the macro with a name. The macro will generate a struct that
/// takes a client channel and exposes typesafe methods for each RPC method.
///
/// ```ignore
/// # use quic_rpc::{*, quin::*, client::*};
/// create_store_client!(MyClient);
/// let client = quic_rpc::quinn::Channel::new(client);
/// let client = quic_rpc::client::RpcClient::<MyService, QuinnChannelTypes>::new(client);
/// let mut client = MyClient(client);
/// let sum = client.add(Add(3, 4)).await?;
/// // Sum(7)
/// let (send, mut recv) = client.multiply(Multiply(2));
/// send(Update(3));
/// let res = recv.next().await?;
/// // Some(MultiplyOutput(6))
/// ```
///
/// To use the dispatch function, invoke the macro with a struct that implements your RPC
/// methods and the name of the generated function. You can then use this dispatch function
/// to dispatch the RPC calls to the methods on your target struct.
///
/// ```ignore
/// # use futures::stream::{Stream, StreamExt};
/// # use async_stream::stream;
/// #[derive(Clone)]
/// pub struct Calculator;
/// impl Calculator {
///     async fn add(self, req: Add) -> Sum {
///         Sum(req.0 + req.1)
///     }
///     async fn multiply(
///         self,
///         req: Multiply,
///         updates: impl Stream<Item = MultiplyUpdate>
///     ) -> impl Stream<Item = MultiplyOutput> {
///        stream! {
///            tokio::pin!(updates);
///            while let Some(MultiplyUpdate(n)) = updates.next().await {
///                yield MultiplyResponse(req.0 * n);
///            }
///        }
///     }
/// }
///
/// create_my_dispatch!(Calculator, dispatch_calculator_request);
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///    let server_addr: std::net::SocketAddr = "127.0.0.1:12345".parse()?;
///    let (server, _server_certs) = make_server_endpoint(server_addr)?;
///    let accept = server.accept().await.context("accept failed")?.await?;
///    let connection = quic_rpc::quinn::Channel::new(accept);
///    let calculator = Calculator;
///    let server_handle = spawn_server(
///        StoreService,
///        quic_rpc::quinn::QuinnChannelTypes,
///        connection,
///        calculator,
///        dispatch_calculator_request,
///    );
///    server_handle.await??;
///    Ok(())
/// }
///
/// ```
///
/// The generation of the macros in `CreateDispatch` and `CreateClient`
/// is optional. If you don't need them, pass `_` instead:
///
/// ```ignore
/// # use quic_rpc::*;
/// derive_rpc_service! {
///     Request = MyRequest;
///     Response = MyResponse;
///     Service = MyService;
///     CreateDispatch = _;
///     CreateClient = _;
///
///     Rpc add = Add, _ -> Sum;
///     ClientStreaming stream = Input, Update -> Output;
/// }
/// ```
/// `
#[macro_export]
macro_rules! derive_rpc_service {
    (
        Request = $request:ident;
        Response = $response:ident;
        Service = $service:ident;
        CreateDispatch = $create_dispatch:tt;
        CreateClient = $create_client:tt;

        $($m_pattern:ident $m_name:ident = $m_input:ident, $m_update:tt -> $m_output:ident);+$(;)?
    ) => {
        $crate::__request_enum! {
            $request {
                $($m_input,)*
                $($m_update,)*
            }
        }

        #[allow(clippy::enum_variant_names)]
        #[derive(::std::fmt::Debug, ::derive_more::From, ::derive_more::TryInto, ::serde::Serialize, ::serde::Deserialize)]
        pub enum $response {
            $($m_output($m_output),)*
        }

        $(
            $crate::__rpc_message!($service, $m_pattern, $m_input, $m_update, $m_output);
        )*

        #[derive(::std::clone::Clone, ::std::fmt::Debug)]
        pub struct $service;

        impl $crate::Service for $service {
            type Req = $request;
            type Res = $response;
        }

        $crate::__derive_create_dispatch!(
            $service,
            $request,
            $create_dispatch,
            [ $($m_pattern $m_name = $m_input, $m_update -> $m_output);+ ]
        );

        $crate::__derive_create_client!(
            $service,
            $create_client,
            [ $($m_pattern $m_name = $m_input, $m_update -> $m_output);+ ]
        );
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __derive_create_dispatch {
    (
        $service:ident,
        $request:ident,
        _,
        [ $($tt:tt)* ]
    ) => {};
    (
        $service:ident,
        $request:ident,
        $create_dispatch:ident,
        [ $($m_pattern:ident $m_name:ident = $m_input:ident, $m_update:tt -> $m_output:ident);+ ]
    ) => {
        /// Create a dispatch function that forwards RPC call to a method on a target struct.
        ///
        /// The created function can be passed into [quic-rpc::server::spawn_server] directly.
        ///
        /// See the docs for [derive_rpc_service] for details.
        #[macro_export]
        macro_rules! $create_dispatch {
            ($target:ident, $handler:ident) => {
                pub async fn $handler<C: $crate::ChannelTypes>(
                    server: $crate::server::RpcServer<$service, C>,
                    msg: <$service as $crate::Service>::Req,
                    chan: (C::SendSink<<$service as $crate::Service>::Res>, C::RecvStream<<$service as $crate::Service>::Req>),
                    target: $target,
                ) -> Result<$crate::server::RpcServer<$service, C>, $crate::server::RpcServerError<C>> {
                    let res = match msg {
                        $(
                            $request::$m_input(msg) => { $crate::__rpc_invoke!($m_pattern, $m_name, $target, server, msg, chan, target) },
                        )*
                        _ => Err($crate::server::RpcServerError::<C>::UnexpectedStartMessage),
                    };
                    res?;
                    Ok(server)
                }
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __request_enum {
    // User entry points.
    ($enum_name:ident { $variant_name:ident $($tt:tt)* }) => {
        $crate::__request_enum!(@ {[$enum_name] [$variant_name]} $($tt)*);
    };

    // Internal rules to categorize each value
    // This also filters out _ placeholders from non-streaming methods.
    (@ {[$enum_name:ident] [$($agg:ident)*]} $(,)? $(_$(,)?)* $variant_name:ident $($tt:tt)*) => {
        $crate::__request_enum!(@ {[$enum_name] [$($agg)* $variant_name]} $($tt)*);
    };

    // Internal rules to categorize each value
    (@ {[$enum_name:ident] [$($agg:ident)*]} $(,)? $variant_name:ident $($tt:tt)*) => {
        $crate::__request_enum!(@ {[$enum_name] [$($agg)* $variant_name]} $($tt)*);
    };

    // Final internal rule that generates the enum from the categorized input
    (@ {[$enum_name:ident] [$($n:ident)*]} $(,)? $(_$(,)?)*) => {
        #[derive(::std::fmt::Debug, ::derive_more::From, ::derive_more::TryInto, ::serde::Serialize, ::serde::Deserialize)]
        pub enum $enum_name {
            $($n($n),)*
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __rpc_message {
    ($service:ident, Rpc, $m_input:ident, _, $m_output:ident) => {
        impl $crate::message::RpcMsg<$service> for $m_input {
            type Response = $m_output;
        }
    };
    ($service:ident, ServerStreaming, $m_input:ident, _, $m_output:ident) => {
        impl $crate::message::Msg<$service> for $m_input {
            type Pattern = $crate::message::ServerStreaming;
            type Response = $m_output;
            type Update = $m_input;
        }
    };
    ($service:ident, $m_pattern:ident, $m_input:ident, $m_update:ident, $m_output:ident) => {
        impl $crate::message::Msg<$service> for $m_input {
            type Pattern = $crate::message::$m_pattern;
            type Response = $m_output;
            type Update = $m_update;
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __rpc_invoke {
    (Rpc, $m_name:ident, $target_ty:ident, $server:ident, $msg:ident, $chan:ident, $target:ident) => {
        $server.rpc($msg, $chan, $target, $target_ty::$m_name).await
    };
    (ClientStreaming, $m_name:ident, $target_ty:ident, $server:ident, $msg:ident, $chan:ident, $target:ident) => {
        $server
            .client_streaming($msg, $chan, $target, $target_ty::$m_name)
            .await
    };
    (ServerStreaming, $m_name:ident, $target_ty:ident, $server:ident, $msg:ident, $chan:ident, $target:ident) => {
        $server
            .server_streaming($msg, $chan, $target, $target_ty::$m_name)
            .await
    };
    (BidiStreaming, $m_name:ident, $target_ty:ident, $server:ident, $msg:ident, $chan:ident, $target:ident) => {
        $server
            .bidi_streaming($msg, $chan, $target, $target_ty::$m_name)
            .await
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __derive_create_client{
    (
        $service:ident,
        _,
        [ $($tt:tt)* ]
    ) => {};
    (
        $service:ident,
        $create_client:tt,
        [ $($m_pattern:ident $m_name:ident = $m_input:ident, $m_update:tt -> $m_output:ident);+ ]
    ) => {
        /// Create a dispatch function that forwards RPC call to a method on a target struct.
        ///
        /// The created function can be passed into [quic-rpc::server::spawn_server] directly.
        ///
        /// See the docs for [derive_rpc_service] for details.
        #[macro_export]
        macro_rules! $create_client {
            ($struct:ident) => {
                #[derive(::std::clone::Clone)]
                pub struct $struct<C: $crate::ChannelTypes>(pub $crate::client::RpcClient<$service, C>);

                impl<C: $crate::ChannelTypes> $struct<C> {
                    $(
                        $crate::__rpc_method!($m_pattern, $service, $m_name, $m_input, $m_output, $m_update);
                    )*
                }
            };
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __rpc_method {
    (Rpc, $service:ident, $m_name:ident, $m_input:ident, $m_output:ident, _) => {
        pub async fn $m_name(
            &mut self,
            input: $m_input,
        ) -> ::std::result::Result<$m_output, $crate::client::RpcClientError<C>> {
            self.0.rpc(input).await
        }
    };
    (ClientStreaming, $service:ident, $m_name:ident, $m_input:ident, $m_output:ident, $m_update:ident) => {
        pub async fn $m_name(
            &mut self,
            input: $m_input,
        ) -> ::std::result::Result<
            (
                $crate::client::UpdateSink<$service, C, $m_input>,
                ::futures::future::BoxFuture<
                    'static,
                    ::std::result::Result<$m_output, $crate::client::ClientStreamingItemError<C>>,
                >,
            ),
            $crate::client::ClientStreamingError<C>,
        > {
            self.0.client_streaming(input).await
        }
    };
    (ServerStreaming, $service:ident, $m_name:ident, $m_input:ident, $m_output:ident, _) => {
        pub async fn $m_name(
            &mut self,
            input: $m_input,
        ) -> ::std::result::Result<
            ::futures::stream::BoxStream<
                'static,
                ::std::result::Result<$m_output, $crate::client::StreamingResponseItemError<C>>,
            >,
            $crate::client::StreamingResponseError<C>,
        > {
            self.0.server_streaming(input).await
        }
    };
    (BidiStreaming, $service:ident, $m_name:ident, $m_input:ident, $m_output:ident, $m_update:ident) => {
        pub async fn $m_name(
            &mut self,
            input: $m_input,
        ) -> ::std::result::Result<
            (
                $crate::client::UpdateSink<$service, C, $m_input>,
                ::futures::stream::BoxStream<
                    'static,
                    ::std::result::Result<$m_output, $crate::client::BidiItemError<C>>,
                >,
            ),
            $crate::client::BidiError<C>,
        > {
            self.0.bidi(input).await
        }
    };
}
