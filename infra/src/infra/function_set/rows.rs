use proto::jobworkerp::data::{RunnerId, WorkerId};
use proto::jobworkerp::function::data::{
    function_id, FunctionId, FunctionSet, FunctionSetData, FunctionSetId, FunctionUsing,
};

// Constants for target_type values
const RUNNER_TYPE: i32 = 0;
const WORKER_TYPE: i32 = 1;

// db row definitions
#[derive(sqlx::FromRow)]
pub struct FunctionSetRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub category: i32,
}

impl FunctionSetRow {
    pub fn to_proto(&self, targets: Vec<FunctionSetTargetRow>) -> FunctionSet {
        FunctionSet {
            id: Some(FunctionSetId { value: self.id }),
            data: Some(FunctionSetData {
                name: self.name.clone(),
                description: self.description.clone(),
                category: self.category,
                targets: targets
                    .into_iter()
                    .filter(|t| t.set_id == self.id)
                    .map(|t| t.to_function_using())
                    .collect(),
            }),
        }
    }
}

// db row definitions
#[derive(sqlx::FromRow)]
pub struct FunctionSetTargetRow {
    pub id: i64,
    pub set_id: i64,
    pub target_id: i64,
    pub target_type: i32,
    pub using: Option<String>,
}

impl FunctionSetTargetRow {
    pub fn to_function_using(&self) -> FunctionUsing {
        let function_id = match self.target_type {
            RUNNER_TYPE => Some(FunctionId {
                id: Some(function_id::Id::RunnerId(RunnerId {
                    value: self.target_id,
                })),
            }),
            WORKER_TYPE => Some(FunctionId {
                id: Some(function_id::Id::WorkerId(WorkerId {
                    value: self.target_id,
                })),
            }),
            _ => {
                tracing::warn!(
                    "Unknown target_type: {} for target_id: {}. Treating as None.",
                    self.target_type,
                    self.target_id
                );
                None
            }
        };

        FunctionUsing {
            function_id,
            using: self.using.clone(),
        }
    }

    pub fn from_function_using(
        set_id: i64,
        function_using: &FunctionUsing,
    ) -> Option<(i64, i32, Option<String>)> {
        let function_id = function_using.function_id.as_ref()?;

        match &function_id.id {
            Some(function_id::Id::RunnerId(runner_id)) => {
                Some((runner_id.value, RUNNER_TYPE, function_using.using.clone()))
            }
            Some(function_id::Id::WorkerId(worker_id)) => {
                Some((worker_id.value, WORKER_TYPE, function_using.using.clone()))
            }
            None => {
                tracing::warn!(
                    "FunctionId has no id set for set_id: {}. Skipping target.",
                    set_id
                );
                None
            }
        }
    }
}
