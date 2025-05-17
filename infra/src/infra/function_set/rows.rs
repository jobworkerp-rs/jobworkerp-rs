use proto::jobworkerp::function::data::{FunctionSet, FunctionSetData, FunctionSetId};

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
                    .map(|t| proto::jobworkerp::function::data::FunctionTarget {
                        id: t.target_id,
                        r#type: t.target_type,
                    })
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
}
