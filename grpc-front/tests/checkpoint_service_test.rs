use grpc_front::proto::jobworkerp::data::{
    CheckPoint,
    TaskCheckPointContext as ProtoTaskCheckPointContext,
    WorkflowCheckPointContext as ProtoWorkflowCheckPointContext,
};
use grpc_front::proto::jobworkerp::service::checkpoint_service_server::CheckpointService;
use grpc_front::proto::jobworkerp::service::{GetCheckpointRequest, UpdateCheckpointRequest};
use grpc_front::service::checkpoint::CheckpointGrpcImpl;
use std::sync::Arc;
use tonic::Request;

#[tokio::test]
async fn test_checkpoint_service() {
    // 1. Setup AppWrapperModule with memory repository
    // We need an AppModule to create AppWrapperModule using the test helper
    // But create_test_app_wrapper_module requires AppModule which might be heavy to create.
    // Alternatively, we can construct AppWrapperModule manually if possible, or use mocks.
    
    // Let's try to look at how AppWrapperModule is constructed in `app_wrapper::modules::test`
    // It seems it needs AppModule.
    
    // However, CheckpointGrpcImpl only needs AppWrapperModule, and specifically the repositories inside it.
    // The repositories are just Arc<dyn CheckPointRepositoryWithId>.
    
    // Let's try to construct AppWrapperModule manually.
    
    let workflow_config = Arc::new(app_wrapper::workflow::WorkflowConfig::new_by_envy());
    let config_module = Arc::new(app_wrapper::modules::AppWrapperConfigModule::new(workflow_config));
    
    // AppWrapperRepositoryModule::new requires RedisPool (optional)
    let repositories = Arc::new(app_wrapper::modules::AppWrapperRepositoryModule::new(
        config_module.clone(),
        None, // No Redis
    ));
    
    let app_wrapper_module = Arc::new(app_wrapper::modules::AppWrapperModule {
        config_module,
        repositories,
    });

    // 2. Create Service
    let service = CheckpointGrpcImpl::new(app_wrapper_module);

    // 3. Create Checkpoint Data
    let execution_id = "exec-123";
    let workflow_name = "test-workflow";
    let position_str = "/steps/0";
    
    let workflow_ctx = ProtoWorkflowCheckPointContext {
        name: "test-workflow".to_string(),
        input: "{\"foo\":\"bar\"}".to_string(),
        context_variables: "{\"var1\":1}".to_string(),
    };
    
    let task_ctx = ProtoTaskCheckPointContext {
        input: "{\"task_in\":\"val\"}".to_string(),
        output: "{\"task_out\":\"res\"}".to_string(),
        context_variables: "{\"var2\":2}".to_string(),
        flow_directive: "Continue".to_string(),
    };

    let checkpoint_proto = CheckPoint {
        workflow: Some(workflow_ctx),
        task: Some(task_ctx),
        position: position_str.to_string(),
    };

    // 4. Test Update (Save)
    let update_req = Request::new(UpdateCheckpointRequest {
        execution_id: execution_id.to_string(),
        workflow_name: workflow_name.to_string(),
        checkpoint: Some(checkpoint_proto.clone()),
    });

    let update_res = service.update(update_req).await;
    assert!(update_res.is_ok());

    // 5. Test Get
    let get_req = Request::new(GetCheckpointRequest {
        execution_id: execution_id.to_string(),
        workflow_name: workflow_name.to_string(),
        position: position_str.to_string(),
    });

    let get_res = service.get(get_req).await;
    assert!(get_res.is_ok());
    let response = get_res.unwrap().into_inner();
    assert!(response.checkpoint.is_some());
    
    let retrieved_checkpoint = response.checkpoint.unwrap();
    assert_eq!(retrieved_checkpoint.position, position_str);
    assert_eq!(retrieved_checkpoint.workflow.as_ref().unwrap().name, "test-workflow");
    
    // 6. Test Get Non-existent
    let get_req_missing = Request::new(GetCheckpointRequest {
        execution_id: "missing-id".to_string(),
        workflow_name: workflow_name.to_string(),
        position: position_str.to_string(),
    });
    
    let get_res_missing = service.get(get_req_missing).await;
    assert!(get_res_missing.is_ok());
    let response_missing = get_res_missing.unwrap().into_inner();
    assert!(response_missing.checkpoint.is_none());
}
