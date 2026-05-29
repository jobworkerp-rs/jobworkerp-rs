pub mod runner;
pub mod validation;

// Re-export the shared ABI crate so plugin authors and host downstream
// code can both reach FFI types / PluginV2 trait / proc macro through a
// single `jobworkerp_runner::*` path.
pub use jobworkerp_plugin_abi as plugin_abi;
pub use jobworkerp_plugin_abi_macros::register_plugin_v2;
// Also keep top-level re-exports of the third-party crates the macro
// expansion references; this lets older plugin code that resolves them
// via `::jobworkerp_runner::*` keep compiling while migration is in
// progress. New plugin code should depend on `jobworkerp-plugin-abi`
// directly and use `::jobworkerp_plugin_abi::*` instead.
pub use jobworkerp_plugin_abi::{async_ffi, async_trait, futures, prost};

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
