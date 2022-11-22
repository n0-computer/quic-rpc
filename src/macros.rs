//! Macros to reduce boilerplate for RPC implementations.

/// Derive a set of RPC types and message implementation from a declaration.
///
/// See [./examples/macro.rs](examples/macro.rs) for an example.
#[macro_export]
macro_rules! derive_rpc_service {
    (
        service $target:ident {
            Request = $request:ident;
            Response = $response:ident;
            Service = $service:ident;
            RequestHandler = $handler:ident;
            $($m_pattern:ident $m_name:ident = $m_input:ident, $m_update:tt -> $m_output:ident);+$(;)?
        }
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

        pub async fn $handler<C: $crate::ChannelTypes>(
            server: $crate::server::RpcServer<$service, C>,
            msg: <$service as $crate::Service>::Req,
            chan: (C::SendSink<<$service as $crate::Service>::Res>, C::RecvStream<<$service as $crate::Service>::Req>),
            target: $target,
        ) -> Result<$crate::server::RpcServer<$service, C>, $crate::server::RpcServerError<C>> {
            match msg {
                $(
                    $request::$m_input(msg) => { $crate::__rpc_invoke!($m_pattern, $m_name, $target, server, msg, chan, target) },
                )*
                _ => Err($crate::server::RpcServerError::<C>::UnexpectedStartMessage)?,
            }?;
            Ok(server)
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
