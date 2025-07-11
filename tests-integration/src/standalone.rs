// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::Arc;

use cache::{
    build_datanode_cache_registry, build_fundamental_cache_registry,
    with_default_composite_cache_registry,
};
use catalog::information_schema::NoopInformationExtension;
use catalog::kvbackend::KvBackendCatalogManagerBuilder;
use catalog::process_manager::ProcessManager;
use cmd::error::StartFlownodeSnafu;
use cmd::standalone::StandaloneOptions;
use common_base::Plugins;
use common_catalog::consts::{MIN_USER_FLOW_ID, MIN_USER_TABLE_ID};
use common_config::KvBackendConfig;
use common_meta::cache::LayeredCacheRegistryBuilder;
use common_meta::ddl::flow_meta::FlowMetadataAllocator;
use common_meta::ddl::table_meta::TableMetadataAllocator;
use common_meta::ddl::{DdlContext, NoopRegionFailureDetectorControl};
use common_meta::ddl_manager::DdlManager;
use common_meta::key::flow::FlowMetadataManager;
use common_meta::key::TableMetadataManager;
use common_meta::kv_backend::KvBackendRef;
use common_meta::region_keeper::MemoryRegionKeeper;
use common_meta::region_registry::LeaderRegionRegistry;
use common_meta::sequence::SequenceBuilder;
use common_meta::wal_options_allocator::build_wal_options_allocator;
use common_procedure::options::ProcedureConfig;
use common_procedure::ProcedureManagerRef;
use common_wal::config::{DatanodeWalConfig, MetasrvWalConfig};
use datanode::datanode::DatanodeBuilder;
use flow::{FlownodeBuilder, FrontendClient, GrpcQueryHandlerWithBoxedError};
use frontend::frontend::Frontend;
use frontend::instance::builder::FrontendBuilder;
use frontend::instance::{Instance, StandaloneDatanodeManager};
use meta_srv::metasrv::{FLOW_ID_SEQ, TABLE_ID_SEQ};
use servers::grpc::GrpcOptions;
use servers::server::ServerHandlers;
use snafu::ResultExt;

use crate::test_util::{self, create_tmp_dir_and_datanode_opts, StorageType, TestGuard};

pub struct GreptimeDbStandalone {
    pub frontend: Arc<Frontend>,
    pub opts: StandaloneOptions,
    pub guard: TestGuard,
    // Used in rebuild.
    pub kv_backend: KvBackendRef,
    pub procedure_manager: ProcedureManagerRef,
}

impl GreptimeDbStandalone {
    pub fn fe_instance(&self) -> &Arc<Instance> {
        &self.frontend.instance
    }
}

pub struct GreptimeDbStandaloneBuilder {
    instance_name: String,
    datanode_wal_config: DatanodeWalConfig,
    metasrv_wal_config: MetasrvWalConfig,
    store_providers: Option<Vec<StorageType>>,
    default_store: Option<StorageType>,
    plugin: Option<Plugins>,
}

impl GreptimeDbStandaloneBuilder {
    pub fn new(instance_name: &str) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            store_providers: None,
            plugin: None,
            default_store: None,
            datanode_wal_config: DatanodeWalConfig::default(),
            metasrv_wal_config: MetasrvWalConfig::default(),
        }
    }

    #[must_use]
    pub fn with_default_store_type(self, store_type: StorageType) -> Self {
        Self {
            default_store: Some(store_type),
            ..self
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_store_providers(self, store_providers: Vec<StorageType>) -> Self {
        Self {
            store_providers: Some(store_providers),
            ..self
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_plugin(self, plugin: Plugins) -> Self {
        Self {
            plugin: Some(plugin),
            ..self
        }
    }

    #[must_use]
    pub fn with_datanode_wal_config(mut self, datanode_wal_config: DatanodeWalConfig) -> Self {
        self.datanode_wal_config = datanode_wal_config;
        self
    }

    #[must_use]
    pub fn with_metasrv_wal_config(mut self, metasrv_wal_config: MetasrvWalConfig) -> Self {
        self.metasrv_wal_config = metasrv_wal_config;
        self
    }

    pub async fn build_with(
        &self,
        kv_backend: KvBackendRef,
        guard: TestGuard,
        opts: StandaloneOptions,
        procedure_manager: ProcedureManagerRef,
        register_procedure_loaders: bool,
    ) -> GreptimeDbStandalone {
        let plugins = self.plugin.clone().unwrap_or_default();

        let layered_cache_registry = Arc::new(
            LayeredCacheRegistryBuilder::default()
                .add_cache_registry(build_datanode_cache_registry(kv_backend.clone()))
                .build(),
        );

        let mut builder =
            DatanodeBuilder::new(opts.datanode_options(), plugins.clone(), kv_backend.clone());
        builder.with_cache_registry(layered_cache_registry);
        let datanode = builder.build().await.unwrap();

        let table_metadata_manager = Arc::new(TableMetadataManager::new(kv_backend.clone()));
        table_metadata_manager.init().await.unwrap();

        let flow_metadata_manager = Arc::new(FlowMetadataManager::new(kv_backend.clone()));

        let layered_cache_builder = LayeredCacheRegistryBuilder::default();
        let fundamental_cache_registry = build_fundamental_cache_registry(kv_backend.clone());
        let cache_registry = Arc::new(
            with_default_composite_cache_registry(
                layered_cache_builder.add_cache_registry(fundamental_cache_registry),
            )
            .unwrap()
            .build(),
        );

        let catalog_manager = KvBackendCatalogManagerBuilder::new(
            Arc::new(NoopInformationExtension),
            kv_backend.clone(),
            cache_registry.clone(),
        )
        .with_procedure_manager(procedure_manager.clone())
        .build();

        let (frontend_client, frontend_instance_handler) =
            FrontendClient::from_empty_grpc_handler(opts.query.clone());
        let flow_builder = FlownodeBuilder::new(
            Default::default(),
            plugins.clone(),
            table_metadata_manager.clone(),
            catalog_manager.clone(),
            flow_metadata_manager.clone(),
            Arc::new(frontend_client),
        );
        let flownode = Arc::new(flow_builder.build().await.unwrap());

        let node_manager = Arc::new(StandaloneDatanodeManager {
            region_server: datanode.region_server(),
            flow_server: flownode.flow_engine(),
        });

        let table_id_sequence = Arc::new(
            SequenceBuilder::new(TABLE_ID_SEQ, kv_backend.clone())
                .initial(MIN_USER_TABLE_ID as u64)
                .step(10)
                .build(),
        );
        let flow_id_sequence = Arc::new(
            SequenceBuilder::new(FLOW_ID_SEQ, kv_backend.clone())
                .initial(MIN_USER_FLOW_ID as u64)
                .step(10)
                .build(),
        );
        let kafka_options = opts.wal.clone().into();
        let wal_options_allocator = build_wal_options_allocator(&kafka_options, kv_backend.clone())
            .await
            .unwrap();
        let wal_options_allocator = Arc::new(wal_options_allocator);
        let table_metadata_allocator = Arc::new(TableMetadataAllocator::new(
            table_id_sequence,
            wal_options_allocator.clone(),
        ));
        let flow_metadata_allocator = Arc::new(FlowMetadataAllocator::with_noop_peer_allocator(
            flow_id_sequence,
        ));

        let ddl_task_executor = Arc::new(
            DdlManager::try_new(
                DdlContext {
                    node_manager: node_manager.clone(),
                    cache_invalidator: cache_registry.clone(),
                    memory_region_keeper: Arc::new(MemoryRegionKeeper::default()),
                    leader_region_registry: Arc::new(LeaderRegionRegistry::default()),
                    table_metadata_manager,
                    table_metadata_allocator,
                    flow_metadata_manager,
                    flow_metadata_allocator,
                    region_failure_detector_controller: Arc::new(NoopRegionFailureDetectorControl),
                },
                procedure_manager.clone(),
                register_procedure_loaders,
            )
            .unwrap(),
        );

        let server_addr = opts.frontend_options().grpc.server_addr.clone();

        let instance = FrontendBuilder::new(
            opts.frontend_options(),
            kv_backend.clone(),
            cache_registry.clone(),
            catalog_manager.clone(),
            node_manager.clone(),
            ddl_task_executor.clone(),
            Arc::new(ProcessManager::new(server_addr, None)),
        )
        .with_plugin(plugins)
        .try_build()
        .await
        .unwrap();
        let instance = Arc::new(instance);

        // set the frontend client for flownode
        let grpc_handler = instance.clone() as Arc<dyn GrpcQueryHandlerWithBoxedError>;
        let weak_grpc_handler = Arc::downgrade(&grpc_handler);
        frontend_instance_handler
            .lock()
            .unwrap()
            .replace(weak_grpc_handler);

        let flow_streaming_engine = flownode.flow_engine().streaming_engine();
        let invoker = flow::FrontendInvoker::build_from(
            flow_streaming_engine.clone(),
            catalog_manager.clone(),
            kv_backend.clone(),
            cache_registry.clone(),
            ddl_task_executor.clone(),
            node_manager.clone(),
        )
        .await
        .context(StartFlownodeSnafu)
        .unwrap();

        flow_streaming_engine.set_frontend_invoker(invoker).await;

        procedure_manager.start().await.unwrap();
        wal_options_allocator.start().await.unwrap();

        test_util::prepare_another_catalog_and_schema(&instance).await;

        let mut frontend = Frontend {
            instance,
            servers: ServerHandlers::default(),
            heartbeat_task: None,
            export_metrics_task: None,
        };

        frontend.start().await.unwrap();

        GreptimeDbStandalone {
            frontend: Arc::new(frontend),
            opts,
            guard,
            kv_backend,
            procedure_manager,
        }
    }

    pub async fn build(&self) -> GreptimeDbStandalone {
        let default_store_type = self.default_store.unwrap_or(StorageType::File);
        let store_types = self.store_providers.clone().unwrap_or_default();

        let (opts, guard) = create_tmp_dir_and_datanode_opts(
            default_store_type,
            store_types,
            &self.instance_name,
            self.datanode_wal_config.clone(),
        );

        let kv_backend_config = KvBackendConfig::default();
        let procedure_config = ProcedureConfig::default();
        let (kv_backend, procedure_manager) = Instance::try_build_standalone_components(
            format!("{}/kv", &opts.storage.data_home),
            kv_backend_config,
            procedure_config,
        )
        .await
        .unwrap();

        let standalone_opts = StandaloneOptions {
            storage: opts.storage,
            procedure: procedure_config,
            metadata_store: kv_backend_config,
            wal: self.metasrv_wal_config.clone().into(),
            grpc: GrpcOptions::default().with_server_addr("127.0.0.1:4001"),
            ..StandaloneOptions::default()
        };

        self.build_with(kv_backend, guard, standalone_opts, procedure_manager, true)
            .await
    }
}
