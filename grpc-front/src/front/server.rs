use crate::proto::jobworkerp::function::service::function_service_server::FunctionServiceServer;
use crate::proto::jobworkerp::function::service::function_set_service_server::FunctionSetServiceServer;
use crate::proto::jobworkerp::service::job_processing_status_service_server::JobProcessingStatusServiceServer;
use crate::proto::jobworkerp::service::job_restore_service_server::JobRestoreServiceServer;
use crate::proto::jobworkerp::service::job_result_service_server::JobResultServiceServer;
use crate::proto::jobworkerp::service::job_service_server::JobServiceServer;
use crate::proto::jobworkerp::service::runner_service_server::RunnerServiceServer;
use crate::proto::jobworkerp::service::worker_service_server::WorkerServiceServer;
use crate::proto::FILE_DESCRIPTOR_SET;
use crate::service::function::FunctionGrpcImpl;
use crate::service::function_set::FunctionSetGrpcImpl;
use crate::service::job::JobGrpcImpl;
use crate::service::job_restore::JobRestoreGrpcImpl;
use crate::service::job_result::JobResultGrpcImpl;
use crate::service::job_status::JobProcessingStatusGrpcImpl;
use crate::service::runner::RunnerGrpcImpl;
use crate::service::worker::WorkerGrpcImpl;
use anyhow::anyhow;
use anyhow::Result;
use app::module::AppModule;
use command_utils::util::shutdown::ShutdownLock;
use infra_utils::infra::net::grpc::enable_grpc_web;
use std::net::SocketAddr;
use std::sync::Arc;
use tonic::transport::Server;
use tonic_tracing_opentelemetry::middleware::filters;
use tonic_tracing_opentelemetry::middleware::server;
use tower::ServiceBuilder;

pub async fn start_server(
    app_module: Arc<AppModule>,
    lock: ShutdownLock,
    addr: SocketAddr,
    use_web: bool,
    max_frame_size: Option<u32>,
) -> Result<()> {
    let (mut _health_reporter, health_service) = tonic_health::server::health_reporter();
    // reflection
    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
        .build_v1()
        .unwrap();

    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                tracing::info!("received ctrl_c");
                let _ = tx.send(()).inspect_err(|e| {
                    tracing::error!("failed to send shutdown signal: {:?}", e);
                });
            }
            Err(e) => tracing::error!("failed to listen for ctrl_c: {:?}", e),
        }
    });

    if use_web {
        Server::builder()
            .accept_http1(true) // for gRPC-web
            // .layer(server::OtelGrpcLayer::default().filter(filters::reject_healthcheck))
            .max_frame_size(max_frame_size) // 16MB
            // .layer(GrpcWebLayer::new()) // for grpc-web // server type is changed if this line is added
            .layer(
                ServiceBuilder::new()
                    .layer(server::OtelGrpcLayer::default().filter(filters::reject_healthcheck))
                    .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>),
            )
            .add_service(enable_grpc_web(RunnerServiceServer::new(
                RunnerGrpcImpl::new(app_module.clone()),
            )))
            .add_service(enable_grpc_web(WorkerServiceServer::new(
                WorkerGrpcImpl::new(app_module.clone()),
            )))
            .add_service(enable_grpc_web(JobServiceServer::new(JobGrpcImpl::new(
                app_module.clone(),
            ))))
            .add_service(enable_grpc_web(JobProcessingStatusServiceServer::new(
                JobProcessingStatusGrpcImpl::new(app_module.clone()),
            )))
            .add_service(enable_grpc_web(JobRestoreServiceServer::new(
                JobRestoreGrpcImpl::new(app_module.clone()),
            )))
            .add_service(enable_grpc_web(JobResultServiceServer::new(
                JobResultGrpcImpl::new(app_module.clone()),
            )))
            .add_service(enable_grpc_web(FunctionSetServiceServer::new(
                FunctionSetGrpcImpl::new(app_module.clone()),
            )))
            .add_service(enable_grpc_web(FunctionServiceServer::new(
                FunctionGrpcImpl::new(app_module),
            )))
            .add_service(reflection)
            .add_service(health_service)
            // serve. shutdown if tokio::signal::ctrl_c() is called
            .serve_with_shutdown(addr, async {
                rx.await.ok();
            })
            .await
            .map_err(|e| anyhow!("grpc web server error: {:?}", e))?;
    } else {
        Server::builder()
            .max_frame_size(max_frame_size) // 16MB
            .layer(
                ServiceBuilder::new()
                    .layer(server::OtelGrpcLayer::default().filter(filters::reject_healthcheck))
                    // .map_request(|req| req)
                    .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>),
            )
            // .layer(server::OtelGrpcLayer::default().filter(filters::reject_healthcheck))
            .add_service(RunnerServiceServer::new(RunnerGrpcImpl::new(
                app_module.clone(),
            )))
            .add_service(WorkerServiceServer::new(WorkerGrpcImpl::new(
                app_module.clone(),
            )))
            .add_service(JobServiceServer::new(JobGrpcImpl::new(app_module.clone())))
            .add_service(JobProcessingStatusServiceServer::new(
                JobProcessingStatusGrpcImpl::new(app_module.clone()),
            ))
            .add_service(JobRestoreServiceServer::new(JobRestoreGrpcImpl::new(
                app_module.clone(),
            )))
            .add_service(JobResultServiceServer::new(JobResultGrpcImpl::new(
                app_module.clone(),
            )))
            .add_service(FunctionSetServiceServer::new(FunctionSetGrpcImpl::new(
                app_module.clone(),
            )))
            .add_service(FunctionServiceServer::new(FunctionGrpcImpl::new(
                app_module,
            )))
            .add_service(reflection)
            .add_service(health_service)
            // serve. shutdown if tokio::signal::ctrl_c() is called
            .serve_with_shutdown(addr, async {
                rx.await.ok();
            })
            .await
            .map_err(|e| anyhow!("grpc web server error: {:?}", e))?;
    }
    lock.unlock();
    Ok(())
}
