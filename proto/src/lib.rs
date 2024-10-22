pub mod jobworkerp {
    pub mod data {
        tonic::include_proto!("jobworkerp.data");
    }
}

// for test runner
tonic::include_proto!("_");
