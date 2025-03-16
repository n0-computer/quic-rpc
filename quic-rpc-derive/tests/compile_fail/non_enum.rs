use quic_rpc_derive::rpc_requests;

#[rpc_requests(Service, Msg)]
struct Foo;

fn main() {}