use quic_rpc_derive::rpc_requests;

#[rpc_requests(Service)]
enum Enum {
    #[rpc(response = Bla, fnord = Foo)]
    A(u8),
}

fn main() {}