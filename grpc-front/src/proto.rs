// If not making a type alias like this, the dependency inside the auto-generated proto code will be wrong.
// (In auto-generated proto code, class references were being resolved with super, so the positional relationship of the data class is pseudo-compatible.)
pub mod jobworkerp {
    pub mod data {
        use proto::jobworkerp::data;
        pub type Priority = data::Priority;
        pub type RunnerId = data::RunnerId;
        pub type RunnerData = data::RunnerData;
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
        pub type JobStatus = data::JobStatus;
        pub type FunctionSpecs = data::FunctionSpecs;
        pub type FunctionInputSchema = data::FunctionInputSchema;
        pub type Empty = data::Empty;
    }
    pub mod service {
        tonic::include_proto!("jobworkerp.service");
    }
}

// for reflection
pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("jobworkerp_descriptor");
