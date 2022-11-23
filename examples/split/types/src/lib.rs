pub mod compute {
    use quic_rpc::rpc_service;
    use serde::{Deserialize, Serialize};
    use std::fmt::Debug;

    /// compute the square of a number
    #[derive(Debug, Serialize, Deserialize)]
    pub struct Sqr(pub u64);
    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct SqrResponse(pub u128);

    /// sum a stream of numbers
    #[derive(Debug, Serialize, Deserialize)]
    pub struct Sum;
    #[derive(Debug, Serialize, Deserialize)]
    pub struct SumUpdate(pub u64);
    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct SumResponse(pub u128);

    /// compute the fibonacci sequence as a stream
    #[derive(Debug, Serialize, Deserialize)]
    pub struct Fibonacci(pub u64);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct FibonacciResponse(pub u128);

    /// multiply a stream of numbers, returning a stream
    #[derive(Debug, Serialize, Deserialize)]
    pub struct Multiply(pub u64);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MultiplyUpdate(pub u64);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MultiplyResponse(pub u128);

    rpc_service! {
        Request = ComputeRequest;
        Response = ComputeResponse;
        Service = ComputeService;
        CreateDispatch = create_compute_dispatch;
        CreateClient = create_compute_client;

        Rpc square = Sqr, _ -> SqrResponse;
        ClientStreaming sum = Sum, SumUpdate -> SumResponse;
        ServerStreaming fibonacci = Fibonacci, _ -> FibonacciResponse;
        BidiStreaming multiply = Multiply, MultiplyUpdate -> MultiplyResponse;
    }
}
