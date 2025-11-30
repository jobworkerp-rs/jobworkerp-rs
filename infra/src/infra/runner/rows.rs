use jobworkerp_runner::runner::RunnerSpec;
use proto::jobworkerp::data::{Runner, RunnerData, RunnerId};

// db row definitions
#[derive(sqlx::FromRow, Debug, Clone, PartialEq)]
pub struct RunnerRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub definition: String,
    pub r#type: i32,
    pub created_at: i64,
}

impl RunnerRow {
    /// Convert RunnerRow to RunnerWithSchema with cached JSON Schema generation
    ///
    /// Phase 6.7: This method generates both Protobuf and JSON Schema maps from the runner,
    /// and caches them in RunnerWithSchema for efficient reuse.
    ///
    /// **Performance**: JSON Schema conversion happens only once during this call.
    /// All subsequent usage (FunctionSpecs conversion, etc.) reuses the cached data.
    ///
    /// Phase 6.7 Final: Unified schema loading for all runners (MCP/Plugin/Normal).
    /// - MCP Server: Instant (from in-memory available_tools)
    /// - Plugin: Synchronous FFI call (timeout handled at plugin initialization)
    /// - Normal Runners: Instant (hardcoded schemas)
    pub async fn to_runner_with_schema(
        &self,
        runner: Box<dyn RunnerSpec + Send + Sync>,
    ) -> RunnerWithSchema {
        let runner_settings_proto = runner.runner_settings_proto();

        let proto_map = runner.method_proto_map();
        let method_proto_map = Some(proto::jobworkerp::data::MethodProtoMap {
            schemas: proto_map.clone(),
        });

        // Phase 6.7: Use RunnerSpec::method_json_schema_map() to respect custom schemas
        // CRITICAL: Call runner.method_json_schema_map() instead of auto-converting
        // Reason: Runners like InlineWorkflowRunnerSpec provide hand-crafted JSON Schema
        //         with oneOf constraints that would be lost in auto-conversion
        use jobworkerp_runner::runner::MethodJsonSchema;
        let json_schema_map = MethodJsonSchema::map_to_proto(runner.method_json_schema_map());
        let method_json_schema_map = Some(proto::jobworkerp::data::MethodJsonSchemaMap {
            schemas: json_schema_map,
        });

        let settings_schema = runner.settings_schema();

        RunnerWithSchema {
            id: Some(RunnerId { value: self.id }),
            data: Some(RunnerData {
                name: self.name.clone(),
                description: self.description.clone(),
                runner_type: self.r#type,
                runner_settings_proto,
                definition: self.definition.clone(),
                method_proto_map,
            }),
            settings_schema,
            method_json_schema_map, // ✅ Cached JSON Schema
        }
    }
}

/// Runner with cached schema information (Phase 6.7)
///
/// This structure caches both Protobuf and JSON Schema definitions
/// to avoid repeated conversions. The conversion happens once during
/// RunnerRow::to_runner_with_schema() and is reused everywhere.
///
/// # Phase 6.7 Changes
/// - Removed: arguments_schema (tag 4), output_schema (tag 5), tools (tag 6)
/// - Added: method_json_schema_map (tag 7)
/// - All method-level schemas (including MCP tools) are unified in method_json_schema_map
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, ::prost::Message)]
pub struct RunnerWithSchema {
    #[prost(message, tag = "1")]
    pub id: Option<RunnerId>,
    #[prost(message, tag = "2")]
    pub data: Option<RunnerData>,
    #[prost(string, tag = "3")]
    pub settings_schema: String,
    // Phase 6.7: Reserved tags for deleted fields (prevent reuse)
    // tag 4: arguments_schema (deprecated, use method_json_schema_map)
    // tag 5: output_schema (deprecated, use method_json_schema_map)
    // tag 6: tools (deprecated, use method_json_schema_map)
    /// Phase 6.7: Unified JSON Schema map (replaces arguments_schema, output_schema, tools)
    /// Generated once from method_proto_map during to_runner_with_schema()
    #[prost(message, tag = "7")]
    pub method_json_schema_map: Option<proto::jobworkerp::data::MethodJsonSchemaMap>,
}

impl RunnerWithSchema {
    pub fn to_proto(&self) -> Runner {
        Runner {
            id: self.id,
            data: self.data.clone(),
        }
    }
    pub fn into_proto(self) -> Runner {
        Runner {
            id: self.id,
            data: self.data,
        }
    }
}
