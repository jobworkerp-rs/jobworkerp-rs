pub mod runner;

// If not making a type alias like this, the dependency inside the auto-generated proto code will be wrong.
// (In auto-generated proto code, class references were being resolved with super, so the positional relationship of the data class is pseudo-compatible.)
pub mod jobworkerp {
    pub mod data {
        use proto::jobworkerp::data;
        pub type ResultStatus = data::ResultStatus;
        pub type ResultOutput = data::ResultOutput;
    }
    pub mod runner {
        tonic::include_proto!("jobworkerp.runner");
        impl ReusableWorkflowRunnerSettings {
            pub fn input_schema(&self) -> Option<serde_json::Value> {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                    self.json_data.as_str(),
                )
                .ok()
                .and_then(|json_data| json_data.get("input").cloned())
            }
        }
        pub mod llm {
            tonic::include_proto!("jobworkerp.runner.llm");
        }
    }
}
