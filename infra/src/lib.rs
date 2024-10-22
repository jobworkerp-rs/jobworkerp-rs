pub mod error;
pub mod infra;

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
    }
}
