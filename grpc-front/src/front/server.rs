use crate::proto::jobworkerp::service::job_restore_service_server::JobRestoreServiceServer;
use crate::proto::jobworkerp::service::job_result_service_server::JobResultServiceServer;
use crate::proto::jobworkerp::service::job_service_server::JobServiceServer;
use crate::proto::jobworkerp::service::job_status_service_server::JobStatusServiceServer;
use crate::proto::jobworkerp::service::runner_schema_service_server::RunnerSchemaServiceServer;
use crate::proto::jobworkerp::service::worker_service_server::WorkerServiceServer;
use crate::proto::FILE_DESCRIPTOR_SET;
use crate::service::job::JobGrpcImpl;
use crate::service::job_restore::JobRestoreGrpcImpl;
use crate::service::job_result::JobResultGrpcImpl;
use crate::service::job_status::JobStatusGrpcImpl;
use crate::service::runner_schema::RunnerSchemaGrpcImpl;
use crate::service::worker::WorkerGrpcImpl;
use anyhow::anyhow;
use anyhow::Result;
use app::module::AppModule;
use command_utils::util::result::TapErr;
use command_utils::util::shutdown::ShutdownLock;
use std::net::SocketAddr;
use std::sync::Arc;
use tonic::transport::Server;

pub async fn start_server(
    app_module: Arc<AppModule>,
    lock: ShutdownLock,
    addr: SocketAddr,
    use_web: bool,
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
                let _ = tx.send(()).tap_err(|e| {
                    tracing::error!("failed to send shutdown signal: {:?}", e);
                });
            }
            Err(e) => tracing::error!("failed to listen for ctrl_c: {:?}", e),
        }
    });

    if use_web {
        Server::builder()
            .accept_http1(true) // for gRPC-web
            // .layer(GrpcWebLayer::new()) // for grpc-web // server type is changed if this line is added
            .add_service(tonic_web::enable(RunnerSchemaServiceServer::new(
                RunnerSchemaGrpcImpl::new(app_module.clone()),
            )))
            .add_service(tonic_web::enable(WorkerServiceServer::new(
                WorkerGrpcImpl::new(app_module.clone()),
            )))
            .add_service(tonic_web::enable(JobServiceServer::new(JobGrpcImpl::new(
                app_module.clone(),
            ))))
            .add_service(tonic_web::enable(JobStatusServiceServer::new(
                JobStatusGrpcImpl::new(app_module.clone()),
            )))
            .add_service(tonic_web::enable(JobRestoreServiceServer::new(
                JobRestoreGrpcImpl::new(app_module.clone()),
            )))
            .add_service(tonic_web::enable(JobResultServiceServer::new(
                JobResultGrpcImpl::new(app_module.clone()),
            )))
    } else {
        Server::builder()
            .add_service(WorkerServiceServer::new(WorkerGrpcImpl::new(
                app_module.clone(),
            )))
            .add_service(JobServiceServer::new(JobGrpcImpl::new(app_module.clone())))
            .add_service(JobStatusServiceServer::new(JobStatusGrpcImpl::new(
                app_module.clone(),
            )))
            .add_service(tonic_web::enable(JobRestoreServiceServer::new(
                JobRestoreGrpcImpl::new(app_module.clone()),
            )))
            .add_service(JobResultServiceServer::new(JobResultGrpcImpl::new(
                app_module,
            )))
    }
    .add_service(reflection)
    .add_service(health_service)
    // serve. shutdown if tokio::signal::ctrl_c() is called
    .serve_with_shutdown(addr, async {
        rx.await.ok();
    })
    .await
    .map_err(|e| anyhow!("grpc web server error: {:?}", e))?;
    lock.unlock();
    Ok(())
}
