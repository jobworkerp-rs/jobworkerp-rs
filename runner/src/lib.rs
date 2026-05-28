pub mod runner;
pub mod validation;

// Re-export crates used inside `register_plugin_v2!` so plugin authors only
// need to depend on `jobworkerp-runner` itself (the macro references these
// paths via `::jobworkerp_runner::*`).
pub use async_ffi;
pub use async_trait;
pub use futures;
pub use prost;
// Proc macro for FFI plugin registration.
pub use jobworkerp_runner_macros::register_plugin_v2;

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
