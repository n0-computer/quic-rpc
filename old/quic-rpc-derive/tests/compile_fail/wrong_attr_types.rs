use quic_rpc_derive::rpc_requests;

#[rpc_requests(Service)]
enum Enum {
    #[rpc(fnord = Bla)]
    A(u8),
}

fn main() {}