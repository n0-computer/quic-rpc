pub mod store {
    use quic_rpc::rpc_service;
    use serde::{Deserialize, Serialize};
    use std::fmt::Debug;

    pub type Cid = [u8; 32];

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Put(pub Vec<u8>);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct PutResponse(pub Cid);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Get(pub Cid);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct GetResponse(pub Vec<u8>);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct PutFile;
    #[derive(Debug, Serialize, Deserialize)]
    pub struct PutFileUpdate(pub Vec<u8>);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct PutFileResponse(pub Cid);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct GetFile(pub Cid);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct GetFileResponse(pub Vec<u8>);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct ConvertFile;
    #[derive(Debug, Serialize, Deserialize)]
    pub struct ConvertFileUpdate(pub Vec<u8>);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct ConvertFileResponse(pub Vec<u8>);

    rpc_service! {
        Request = StoreRequest;
        Response = StoreResponse;
        Service = StoreService;
        CreateDispatch = create_store_dispatch;
        CreateClient = create_store_client;

        Rpc put = Put, _ -> PutResponse;
        Rpc get = Get, _ -> GetResponse;
        ClientStreaming put_file = PutFile, PutFileUpdate -> PutFileResponse;
        ServerStreaming get_file = GetFile, _ -> GetFileResponse;
        BidiStreaming convert_file = ConvertFile, ConvertFileUpdate -> ConvertFileResponse;
    }
}
