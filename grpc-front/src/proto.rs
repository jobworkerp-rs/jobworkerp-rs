// If not making a type alias like this, the dependency inside the auto-generated proto code will be wrong.
// (In auto-generated proto code, class references were being resolved with super, so the positional relationship of the data class is pseudo-compatible.)
pub mod jobworkerp {
    pub mod function {
        pub mod data {
            pub use data::function_id;
            use proto::jobworkerp::function::data;
            pub type FunctionId = data::FunctionId;
            pub type FunctionSetData = data::FunctionSetData;
            pub type FunctionSet = data::FunctionSet;
            pub type FunctionSetId = data::FunctionSetId;
            pub type FunctionSetDetail = data::FunctionSetDetail;
            pub type FunctionSetDetailData = data::FunctionSetDetailData;
            pub type FunctionSpecs = data::FunctionSpecs;
            pub type FunctionSchema = data::FunctionSchema;
            pub type McpToolList = data::McpToolList;
            pub type McpTool = data::McpTool;
            pub type WorkerOptions = data::WorkerOptions;
            pub type FunctionCallOptions = data::FunctionCallOptions;
            pub type FunctionExecutionInfo = data::FunctionExecutionInfo;
            pub type FunctionResult = data::FunctionResult;
        }
        pub mod service {
            tonic::include_proto!("jobworkerp.function.service");
        }
    }
    pub mod data {
        use proto::jobworkerp::data;
        pub type Priority = data::Priority;
        pub type RunnerId = data::RunnerId;
        pub type RunnerData = data::RunnerData;
        pub type RunnerType = data::RunnerType;
        pub type Runner = data::Runner;
        pub type WorkerId = data::WorkerId;
        pub type WorkerData = data::WorkerData;
        pub type Worker = data::Worker;
        pub type QueueType = data::QueueType;
        pub type ResponseType = data::ResponseType;
        pub type RetryPolicy = data::RetryPolicy;
        pub type JobId = data::JobId;
        pub type JobData = data::JobData;
        pub type Job = data::Job;
        pub type JobResultId = data::JobResultId;
        pub type JobResultData = data::JobResultData;
        pub type JobResult = data::JobResult;
        pub type ResultOutput = data::ResultOutput;
        pub type ResultOutputItem = data::ResultOutputItem;
        pub type ResultStatus = data::ResultStatus;
        pub type JobProcessingStatus = data::JobProcessingStatus;
        pub type Empty = data::Empty;
    }
    pub mod service {
        tonic::include_proto!("jobworkerp.service");
    }
}

// for reflection
pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("jobworkerp_descriptor");
