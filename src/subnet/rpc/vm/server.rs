use std::sync::Arc;

use crate::{
    ids,
    proto::{
        grpcutil::timestamp_from_time,
        pb::{
            self,
            aliasreader::alias_reader_client::AliasReaderClient,
            google::protobuf::Empty,
            keystore::keystore_client::KeystoreClient,
            messenger::{messenger_client::MessengerClient, NotifyRequest},
            sharedmemory::shared_memory_client::SharedMemoryClient,
            subnetlookup::subnet_lookup_client::SubnetLookupClient,
            vm,
        },
    },
    subnet::{
        self,
        rpc::{
            common::{appsender, message::Message},
            context::Context,
            database::manager::{versioned_database, DatabaseManager},
            database::rpcdb::{client::DatabaseClient, error_to_error_code},
            http::server::Server as HttpServer,
            snow::State,
            utils,
        },
    },
};
use chrono::{TimeZone, Utc};
use prost::bytes::Bytes;
use semver::Version;
use tokio::sync::{broadcast, mpsc, RwLock};
use tonic::{transport::Endpoint, Request, Response};

pub struct Server {
    /// Underlying Vm implementation.
    pub vm: Arc<RwLock<Box<dyn subnet::rpc::vm::Vm + Send + Sync>>>,

    /// Stop channel broadcast producer.
    pub stop_ch: broadcast::Sender<()>,
}

impl Server {
    pub fn new(
        vm: Box<dyn subnet::rpc::vm::Vm + Send + Sync>,
        stop_ch: broadcast::Sender<()>,
    ) -> impl pb::vm::vm_server::Vm {
        Server {
            vm: Arc::new(RwLock::new(vm)),
            stop_ch,
        }
    }
}

#[tonic::async_trait]
impl pb::vm::vm_server::Vm for Server {
    async fn initialize(
        &self,
        req: Request<vm::InitializeRequest>,
    ) -> std::result::Result<Response<vm::InitializeResponse>, tonic::Status> {
        log::info!("initialize called");

        let req = req.into_inner();
        let client_conn = Endpoint::from_shared(format!("http://{}", req.server_addr))
            .unwrap()
            .connect()
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        // Multiplexing in tonic is done by cloning the client which is very cheap.
        // ref. https://docs.rs/tonic/latest/tonic/transport/struct.Channel.html#multiplexing-requests
        let mut message = MessengerClient::new(client_conn.clone());
        let keystore = KeystoreClient::new(client_conn.clone());
        let shared_memory = SharedMemoryClient::new(client_conn.clone());
        let bc_lookup = AliasReaderClient::new(client_conn.clone());
        let sn_lookup = SubnetLookupClient::new(client_conn.clone());
        let app_sender = appsender::client::Client::new(client_conn.clone());

        let ctx = Some(Context {
            network_id: req.network_id,
            subnet_id: ids::Id::from_slice(&req.subnet_id),
            chain_id: ids::Id::from_slice(&req.chain_id),
            node_id: ids::node::Id::from_slice(&req.node_id),
            x_chain_id: ids::Id::from_slice(&req.x_chain_id),
            avax_asset_id: ids::Id::from_slice(&req.avax_asset_id),
            keystore,
            shared_memory,
            bc_lookup,
            sn_lookup,
        });

        let mut versioned_dbs: Vec<versioned_database::VersionedDatabase> =
            Vec::with_capacity(req.db_servers.len());
        for db_server in req.db_servers.iter() {
            let semver = db_server.version.trim_start_matches('v');
            let version =
                Version::parse(semver).map_err(|e| tonic::Status::unknown(e.to_string()))?;
            let server_addr = db_server.server_addr.clone();

            // Create a client connection with the server address
            let client_conn = Endpoint::from_shared(format!("http://{}", server_addr))
                .map_err(|e| tonic::Status::unknown(e.to_string()))?
                .connect()
                .await
                .map_err(|e| tonic::Status::unknown(e.to_string()))?;

            let vdb = versioned_database::VersionedDatabase::new(
                DatabaseClient::new(client_conn),
                version,
            );
            versioned_dbs.push(vdb)
        }
        let db_manager = DatabaseManager::new_from_databases(versioned_dbs);

        let (tx_engine, mut rx_engine): (mpsc::Sender<Message>, mpsc::Receiver<Message>) =
            mpsc::channel(100);
        tokio::spawn(async move {
            loop {
                match rx_engine.recv().await {
                    Some(msg) => {
                        log::debug!("message received: {:?}", msg);
                        match message
                            .notify(NotifyRequest {
                                message: msg as u32,
                            })
                            .await
                        {
                            Ok(_) => continue,
                            Err(e) => {
                                return tonic::Status::unknown(e.to_string());
                            }
                        }
                    }
                    None => {
                        log::error!("engine receiver closed unexpectedly");
                        return tonic::Status::unknown("engine receiver closed unexpectedly");
                    }
                }
            }
        });

        let mut inner_vm = self.vm.write().await;
        inner_vm
            .initialize(
                ctx,
                db_manager,
                &req.genesis_bytes,
                &req.upgrade_bytes,
                &req.config_bytes,
                tx_engine,
                &[()],
                app_sender,
            )
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        // Get last accepted block on the chain
        let last_accepted = inner_vm.last_accepted().await?;

        let last_accepted_block = inner_vm
            .get_block(last_accepted)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        log::debug!("last_accepted_block id: {:?}", last_accepted);

        Ok(Response::new(vm::InitializeResponse {
            last_accepted_id: Bytes::from(last_accepted.to_vec()),
            last_accepted_parent_id: Bytes::from(last_accepted_block.parent().await.to_vec()),
            bytes: Bytes::from(last_accepted_block.bytes().await.to_vec()),
            height: last_accepted_block.height().await,
            timestamp: Some(timestamp_from_time(
                &Utc.timestamp(last_accepted_block.timestamp().await as i64, 0),
            )),
        }))
    }

    async fn shutdown(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("shutdown called");

        // notify all gRPC servers to shutdown
        self.stop_ch
            .send(())
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }

    /// Creates the HTTP handlers for custom chain network calls.
    /// This creates and exposes handlers that the outside world can use to communicate
    /// with the chain. Each handler has the path:
    // [Address of node]/ext/bc/[chain ID]/[extension]
    ///
    /// Returns a mapping from [extension]s to HTTP handlers.
    /// Each extension can specify how locking is managed for convenience.
    ///
    /// For example, if this VM implements an account-based payments system,
    /// it have an extension called `accounts`, where clients could get
    /// information about their accounts.
    async fn create_handlers(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::CreateHandlersResponse>, tonic::Status> {
        log::debug!("create_handlers called");

        // get handlers from underlying vm
        let mut inner_vm = self.vm.write().await;
        let handlers = inner_vm.create_handlers().await.map_err(|e| {
            tonic::Status::unknown(format!("failed to create handlers: {:?}", e.to_string()))
        })?;

        // create and start gRPC server serving HTTP service for each handler
        let mut resp_handlers: Vec<vm::Handler> = Vec::with_capacity(handlers.keys().len());
        for (prefix, handler) in handlers {
            let server_addr = utils::new_socket_addr();
            if handler.handler.clone().is_none() {
                log::error!("handler did not provide an IoHandler: {}", prefix);
                continue;
            }
            let http_service = HttpServer::new(handler.handler.clone().expect("IoHandler"));

            let server = utils::grpc::Server::new(server_addr, self.stop_ch.subscribe());
            server
                .serve(pb::http::http_server::HttpServer::new(http_service))
                .map_err(|e| {
                    tonic::Status::unknown(format!(
                        "failed to create http service: {:?}",
                        e.to_string()
                    ))
                })?;

            let resp_handler = vm::Handler {
                prefix,
                lock_options: handler.lock_option as u32,
                server_addr: server_addr.to_string(),
            };
            resp_handlers.push(resp_handler);
        }

        Ok(Response::new(vm::CreateHandlersResponse {
            handlers: resp_handlers,
        }))
    }

    /// Creates the HTTP handlers for custom VM network calls.
    ///
    /// This creates and exposes handlers that the outside world can use to communicate
    /// with a static reference to the VM. Each handler has the path:
    /// [Address of node]/ext/VM/[VM ID]/[extension]
    ///
    /// Returns a mapping from [extension]s to HTTP handlers.
    ///
    /// Each extension can specify how locking is managed for convenience.
    ///
    /// For example, it might make sense to have an extension for creating
    /// genesis bytes this VM can interpret.
    async fn create_static_handlers(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::CreateStaticHandlersResponse>, tonic::Status> {
        log::debug!("create_handlers called");

        // get handlers from underlying vm
        let mut inner_vm = self.vm.write().await;
        let handlers = inner_vm.create_static_handlers().await.map_err(|e| {
            tonic::Status::unknown(format!("failed to create handlers: {:?}", e.to_string()))
        })?;

        // create and start gRPC server serving HTTP service for each handler
        let mut resp_handlers: Vec<vm::Handler> = Vec::with_capacity(handlers.keys().len());
        for (prefix, handler) in handlers {
            let server_addr = utils::new_socket_addr();
            if handler.handler.clone().is_none() {
                log::error!("handler did not provide an IoHandler: {}", prefix);
                continue;
            }
            let http_service = HttpServer::new(handler.handler.clone().expect("IoHandler"));

            let server = utils::grpc::Server::new(server_addr, self.stop_ch.subscribe());
            server
                .serve(pb::http::http_server::HttpServer::new(http_service))
                .map_err(|e| {
                    tonic::Status::unknown(format!(
                        "failed to create http service: {:?}",
                        e.to_string()
                    ))
                })?;

            let resp_handler = vm::Handler {
                prefix,
                lock_options: handler.lock_option as u32,
                server_addr: server_addr.to_string(),
            };
            resp_handlers.push(resp_handler);
        }

        Ok(Response::new(vm::CreateStaticHandlersResponse {
            handlers: resp_handlers,
        }))
    }

    async fn build_block(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::BuildBlockResponse>, tonic::Status> {
        log::debug!("build_block called");

        let inner_vm = self.vm.write().await;
        let block = inner_vm
            .build_block()
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(vm::BuildBlockResponse {
            id: Bytes::from(block.id().await.to_vec()),
            parent_id: Bytes::from(block.parent().await.to_vec()),
            bytes: Bytes::from(block.bytes().await.to_vec()),
            height: block.height().await,
            timestamp: Some(timestamp_from_time(
                &Utc.timestamp(block.timestamp().await as i64, 0),
            )),
        }))
    }

    async fn parse_block(
        &self,
        req: Request<vm::ParseBlockRequest>,
    ) -> std::result::Result<Response<vm::ParseBlockResponse>, tonic::Status> {
        log::debug!("parse_block called");

        let req = req.into_inner();
        let inner_vm = self.vm.write().await;
        let block = inner_vm
            .parse_block(req.bytes.as_ref())
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(vm::ParseBlockResponse {
            id: Bytes::from(block.id().await.to_vec()),
            parent_id: Bytes::from(block.parent().await.to_vec()),
            status: block.status().await.to_u32(),
            height: block.height().await,
            timestamp: Some(timestamp_from_time(
                &Utc.timestamp(block.timestamp().await as i64, 0),
            )),
        }))
    }

    /// Attempt to load a block.
    ///
    /// If the block does not exist, an empty GetBlockResponse is returned with
    /// an error code.
    ///
    /// It is expected that blocks that have been successfully verified should be
    /// returned correctly. It is also expected that blocks that have been
    /// accepted by the consensus engine should be able to be fetched. It is not
    /// required for blocks that have been rejected by the consensus engine to be
    /// able to be fetched.
    /// ref: https://pkg.go.dev/github.com/ava-labs/avalanchego/snow/engine/snowman/block#Getter
    async fn get_block(
        &self,
        req: Request<vm::GetBlockRequest>,
    ) -> std::result::Result<Response<vm::GetBlockResponse>, tonic::Status> {
        log::debug!("get_block called");

        let req = req.into_inner();
        let inner_vm = self.vm.read().await;

        // determine if response is an error or not
        match inner_vm.get_block(ids::Id::from_slice(&req.id)).await {
            Ok(block) => Ok(Response::new(vm::GetBlockResponse {
                parent_id: Bytes::from(block.parent().await.to_vec()),
                bytes: Bytes::from(block.bytes().await.to_vec()),
                status: block.status().await.to_u32(),
                height: block.height().await,
                timestamp: Some(timestamp_from_time(
                    &Utc.timestamp(block.timestamp().await as i64, 0),
                )),
                err: 0, // return 0 indicating no error
            })),
            // if an error was found, generate empty response with ErrNotFound code
            // ref: https://github.com/ava-labs/avalanchego/blob/master/vms/
            Err(e) => {
                log::debug!("Error getting block");
                Ok(Response::new(vm::GetBlockResponse {
                    parent_id: Bytes::new(),
                    bytes: Bytes::new(),
                    status: 0,
                    height: 0,
                    timestamp: Some(timestamp_from_time(&Utc.timestamp(0, 0))),
                    err: error_to_error_code(&e.to_string()).unwrap(),
                }))
            }
        }
    }

    async fn set_state(
        &self,
        req: Request<vm::SetStateRequest>,
    ) -> std::result::Result<Response<vm::SetStateResponse>, tonic::Status> {
        log::debug!("set_state called");

        let req = req.into_inner();
        let inner_vm = self.vm.write().await;
        let state = State::try_from(req.state)
            .map_err(|_| tonic::Status::unknown("failed to convert to vm state"))?;

        inner_vm
            .set_state(state)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        let last_accepted_id = inner_vm
            .last_accepted()
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        let block = inner_vm
            .get_block(last_accepted_id)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(vm::SetStateResponse {
            last_accepted_id: Bytes::from(last_accepted_id.to_vec()),
            last_accepted_parent_id: Bytes::from(block.parent().await.to_vec()),
            height: block.height().await,
            bytes: Bytes::from(block.bytes().await.to_vec()),
            timestamp: Some(timestamp_from_time(
                &Utc.timestamp(block.timestamp().await as i64, 0),
            )),
        }))
    }

    async fn set_preference(
        &self,
        req: Request<vm::SetPreferenceRequest>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("set_preference called");

        let req = req.into_inner();
        let inner_vm = self.vm.read().await;
        inner_vm
            .set_preference(ids::Id::from_slice(&req.id))
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }

    async fn health(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::HealthResponse>, tonic::Status> {
        log::debug!("health called");

        let inner_vm = self.vm.read().await;
        let resp = inner_vm
            .health_check()
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(vm::HealthResponse {
            details: Bytes::from(resp),
        }))
    }

    async fn version(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::VersionResponse>, tonic::Status> {
        log::debug!("version called");

        let inner_vm = self.vm.read().await;
        let version = inner_vm
            .version()
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(vm::VersionResponse { version }))
    }

    async fn connected(
        &self,
        req: Request<vm::ConnectedRequest>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("connected called");

        let req = req.into_inner();
        let inner_vm = self.vm.read().await;
        let node_id = ids::node::Id::from_slice(&req.node_id);
        inner_vm
            .connected(&node_id)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }

    async fn disconnected(
        &self,
        req: Request<vm::DisconnectedRequest>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("disconnected called");

        let req = req.into_inner();
        let inner_vm = self.vm.read().await;
        let node_id = ids::node::Id::from_slice(&req.node_id);

        inner_vm
            .disconnected(&node_id)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }

    async fn app_request(
        &self,
        req: Request<vm::AppRequestMsg>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("app_request called");

        let req = req.into_inner();
        let node_id = ids::node::Id::from_slice(&req.node_id);
        let inner_vm = self.vm.read().await;

        let ts = req.deadline.as_ref().expect("timestamp");
        let deadline = Utc.timestamp(ts.seconds, ts.nanos as u32);

        inner_vm
            .app_request(&node_id, req.request_id, deadline, &req.request)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }

    async fn app_request_failed(
        &self,
        req: Request<vm::AppRequestFailedMsg>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("app_request_failed called");

        let req = req.into_inner();
        let node_id = ids::node::Id::from_slice(&req.node_id);
        let inner_vm = self.vm.read().await;

        inner_vm
            .app_request_failed(&node_id, req.request_id)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }

    async fn app_response(
        &self,
        req: Request<vm::AppResponseMsg>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("app_response called");

        let req = req.into_inner();
        let node_id = ids::node::Id::from_slice(&req.node_id);
        let inner_vm = self.vm.read().await;

        inner_vm
            .app_response(&node_id, req.request_id, &req.response)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }

    async fn app_gossip(
        &self,
        req: Request<vm::AppGossipMsg>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("app_gossip called");

        let req = req.into_inner();
        let node_id = ids::node::Id::from_slice(&req.node_id);
        let inner_vm = self.vm.read().await;

        inner_vm
            .app_gossip(&node_id, &req.msg)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }

    async fn block_verify(
        &self,
        req: Request<vm::BlockVerifyRequest>,
    ) -> std::result::Result<Response<vm::BlockVerifyResponse>, tonic::Status> {
        log::debug!("block_verify called");

        let req = req.into_inner();
        let inner_vm = self.vm.read().await;

        let mut block = inner_vm
            .parse_block(&req.bytes)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        block
            .verify()
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(vm::BlockVerifyResponse {
            timestamp: Some(timestamp_from_time(
                &Utc.timestamp(block.timestamp().await as i64, 0),
            )),
        }))
    }

    async fn block_accept(
        &self,
        req: Request<vm::BlockAcceptRequest>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("block_accept called");

        let req = req.into_inner();
        let inner_vm = self.vm.read().await;
        let id = ids::Id::from_slice(&req.id);

        let mut block = inner_vm
            .get_block(id)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        block
            .accept()
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }
    async fn block_reject(
        &self,
        req: Request<vm::BlockRejectRequest>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("block_reject called");

        let req = req.into_inner();
        let inner_vm = self.vm.read().await;
        let id = ids::Id::from_slice(&req.id);

        let mut block = inner_vm
            .get_block(id)
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        block
            .reject()
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;

        Ok(Response::new(Empty {}))
    }

    async fn get_ancestors(
        &self,
        _req: Request<vm::GetAncestorsRequest>,
    ) -> std::result::Result<Response<vm::GetAncestorsResponse>, tonic::Status> {
        log::debug!("get_ancestors called");

        Err(tonic::Status::unimplemented("get_ancestors"))
    }

    async fn batched_parse_block(
        &self,
        _req: Request<vm::BatchedParseBlockRequest>,
    ) -> std::result::Result<Response<vm::BatchedParseBlockResponse>, tonic::Status> {
        log::debug!("batched_parse_block called");

        Err(tonic::Status::unimplemented("batched_parse_block"))
    }

    async fn gather(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::GatherResponse>, tonic::Status> {
        log::debug!("gather called");

        Err(tonic::Status::unimplemented("gather"))
    }

    async fn cross_chain_app_request(
        &self,
        _req: Request<vm::CrossChainAppRequestMsg>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("cross_chain_app_request called");

        Err(tonic::Status::unimplemented("cross_chain_app_request"))
    }

    async fn cross_chain_app_request_failed(
        &self,
        _req: Request<vm::CrossChainAppRequestFailedMsg>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("cross_chain_app_request_failed called");

        Err(tonic::Status::unimplemented(
            "send_cross_chain_app_request_failed",
        ))
    }

    async fn cross_chain_app_response(
        &self,
        _req: Request<vm::CrossChainAppResponseMsg>,
    ) -> std::result::Result<Response<Empty>, tonic::Status> {
        log::debug!("cross_chain_app_response called");

        Err(tonic::Status::unimplemented("cross_chain_app_response"))
    }

    async fn state_sync_enabled(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::StateSyncEnabledResponse>, tonic::Status> {
        log::debug!("state_sync_enabled called");

        Err(tonic::Status::unimplemented("state_sync_enabled"))
    }

    async fn get_ongoing_sync_state_summary(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::GetOngoingSyncStateSummaryResponse>, tonic::Status> {
        log::debug!("get_ongoing_sync_state_summary called");

        Err(tonic::Status::unimplemented(
            "get_ongoing_sync_state_summary",
        ))
    }

    async fn parse_state_summary(
        &self,
        _req: Request<vm::ParseStateSummaryRequest>,
    ) -> std::result::Result<tonic::Response<vm::ParseStateSummaryResponse>, tonic::Status> {
        log::debug!("parse_state_summary called");

        Err(tonic::Status::unimplemented("parse_state_summary"))
    }

    async fn get_state_summary(
        &self,
        _req: Request<vm::GetStateSummaryRequest>,
    ) -> std::result::Result<Response<vm::GetStateSummaryResponse>, tonic::Status> {
        log::debug!("get_state_summary called");

        Err(tonic::Status::unimplemented("get_state_summary"))
    }

    async fn get_last_state_summary(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::GetLastStateSummaryResponse>, tonic::Status> {
        log::debug!("get_last_state_summary called");

        Err(tonic::Status::unimplemented("get_last_state_summary"))
    }

    async fn state_summary_accept(
        &self,
        _req: Request<vm::StateSummaryAcceptRequest>,
    ) -> std::result::Result<tonic::Response<vm::StateSummaryAcceptResponse>, tonic::Status> {
        log::debug!("state_summary_accept called");

        Err(tonic::Status::unimplemented("state_summary_accept"))
    }

    async fn verify_height_index(
        &self,
        _req: Request<Empty>,
    ) -> std::result::Result<Response<vm::VerifyHeightIndexResponse>, tonic::Status> {
        log::debug!("verify_height_index called");
        Err(tonic::Status::unimplemented("verify_height_index"))
    }

    async fn get_block_id_at_height(
        &self,
        _req: Request<vm::GetBlockIdAtHeightRequest>,
    ) -> std::result::Result<Response<vm::GetBlockIdAtHeightResponse>, tonic::Status> {
        log::debug!("get_block_id_at_height called");

        Err(tonic::Status::unimplemented("get_block_id_at_height"))
    }
}
