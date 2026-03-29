pub mod runner;
pub mod validation;

// If not making a type alias like this, the dependency inside the auto-generated proto code will be wrong.
// (In auto-generated proto code, class references were being resolved with super, so the positional relationship of the data class is pseudo-compatible.)
pub mod jobworkerp {
    pub mod data {
        use proto::jobworkerp::data;
        pub type ResultStatus = data::ResultStatus;
        pub type ResultOutput = data::ResultOutput;
        // pub type RetryPolicy = data::RetryPolicy;
        // pub type ResponseType = data::ResponseType;
        // pub type WorkerId = data::WorkerId;
    }
    pub mod runner {
        tonic::include_proto!("jobworkerp.runner");
        pub mod grpc {
            tonic::include_proto!("jobworkerp.runner.grpc");
        }
        pub mod llm {
            tonic::include_proto!("jobworkerp.runner.llm");
        }
        pub mod function_set_selector {
            tonic::include_proto!("jobworkerp.runner.function_set_selector");
        }
    }
}
