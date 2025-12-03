// This file is auto-generated from runner/schema/workflow.yaml
// Do not edit manually. Use scripts/update_workflow_schema.sh to regenerate.

// Allow clippy warnings for auto-generated code
#[allow(clippy::redundant_closure_call)]
#[allow(clippy::needless_lifetimes)]
#[allow(clippy::match_single_binding)]
#[allow(clippy::clone_on_copy)]
#[allow(irrefutable_let_patterns)]
#[allow(clippy::derivable_impls)]
#[allow(clippy::large_enum_variant)]
pub mod errors;
pub mod supplement;
#[cfg(test)]
pub mod supplement_test;
pub mod tasks;

#[doc = r" Error types."]
pub mod error {
    #[doc = r" Error from a TryFrom or FromStr implementation."]
    pub struct ConversionError(::std::borrow::Cow<'static, str>);
    impl ::std::error::Error for ConversionError {}
    impl ::std::fmt::Display for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Display::fmt(&self.0, f)
        }
    }
    impl ::std::fmt::Debug for ConversionError {
        fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
            ::std::fmt::Debug::fmt(&self.0, f)
        }
    }
    impl From<&'static str> for ConversionError {
        fn from(value: &'static str) -> Self {
            Self(value.into())
        }
    }
    impl From<String> for ConversionError {
        fn from(value: String) -> Self {
            Self(value.into())
        }
    }
}
#[doc = "Static error filter configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"CatchErrors\","]
#[doc = "  \"description\": \"Static error filter configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"with\": {"]
#[doc = "      \"$ref\": \"#/$defs/errorFilter\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct CatchErrors {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub with: ::std::option::Option<ErrorFilter>,
}
impl ::std::convert::From<&CatchErrors> for CatchErrors {
    fn from(value: &CatchErrors) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for CatchErrors {
    fn default() -> Self {
        Self {
            with: Default::default(),
        }
    }
}
impl CatchErrors {
    pub fn builder() -> builder::CatchErrors {
        Default::default()
    }
}
#[doc = "Checkpoint and restart configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Checkpoint Config\","]
#[doc = "  \"description\": \"Checkpoint and restart configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"enabled\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"enabled\": {"]
#[doc = "      \"description\": \"Enable checkpoint functionality.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"storage\": {"]
#[doc = "      \"description\": \"Storage backend for checkpoints.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"enum\": ["]
#[doc = "        \"memory\","]
#[doc = "        \"redis\""]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct CheckpointConfig {
    #[doc = "Enable checkpoint functionality."]
    pub enabled: bool,
    #[doc = "Storage backend for checkpoints."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub storage: ::std::option::Option<CheckpointConfigStorage>,
}
impl ::std::convert::From<&CheckpointConfig> for CheckpointConfig {
    fn from(value: &CheckpointConfig) -> Self {
        value.clone()
    }
}
impl CheckpointConfig {
    pub fn builder() -> builder::CheckpointConfig {
        Default::default()
    }
}
#[doc = "Storage backend for checkpoints."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"description\": \"Storage backend for checkpoints.\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"memory\","]
#[doc = "    \"redis\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum CheckpointConfigStorage {
    #[serde(rename = "memory")]
    Memory,
    #[serde(rename = "redis")]
    Redis,
}
impl ::std::convert::From<&Self> for CheckpointConfigStorage {
    fn from(value: &CheckpointConfigStorage) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for CheckpointConfigStorage {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Memory => write!(f, "memory"),
            Self::Redis => write!(f, "redis"),
        }
    }
}
impl ::std::str::FromStr for CheckpointConfigStorage {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "memory" => Ok(Self::Memory),
            "redis" => Ok(Self::Redis),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for CheckpointConfigStorage {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for CheckpointConfigStorage {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for CheckpointConfigStorage {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "DoTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"do\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"do\": {"]
#[doc = "      \"title\": \"DoTaskConfiguration\","]
#[doc = "      \"description\": \"Tasks to execute in sequence.\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"for\": false,"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct DoTask {
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[doc = "Tasks to execute in sequence."]
    #[serde(rename = "do")]
    pub do_: TaskList,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
}
impl ::std::convert::From<&DoTask> for DoTask {
    fn from(value: &DoTask) -> Self {
        value.clone()
    }
}
impl DoTask {
    pub fn builder() -> builder::DoTask {
        Default::default()
    }
}
#[doc = "Workflow metadata and identification."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Document\","]
#[doc = "  \"description\": \"Workflow metadata and identification.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"dsl\","]
#[doc = "    \"name\","]
#[doc = "    \"namespace\","]
#[doc = "    \"version\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"dsl\": {"]
#[doc = "      \"title\": \"WorkflowDSL\","]
#[doc = "      \"description\": \"DSL version used by this workflow.\","]
#[doc = "      \"default\": \"0.0.1\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"WorkflowMetadata\","]
#[doc = "      \"description\": \"Additional workflow metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"title\": \"WorkflowName\","]
#[doc = "      \"description\": \"Workflow name.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "    },"]
#[doc = "    \"namespace\": {"]
#[doc = "      \"title\": \"WorkflowNamespace\","]
#[doc = "      \"description\": \"Workflow namespace.\","]
#[doc = "      \"default\": \"default\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "    },"]
#[doc = "    \"summary\": {"]
#[doc = "      \"title\": \"WorkflowSummary\","]
#[doc = "      \"description\": \"Workflow summary in Markdown format.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"tags\": {"]
#[doc = "      \"title\": \"WorkflowTags\","]
#[doc = "      \"description\": \"Key/value tags for workflow classification.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"title\": \"WorkflowTitle\","]
#[doc = "      \"description\": \"Workflow title.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"version\": {"]
#[doc = "      \"title\": \"WorkflowVersion\","]
#[doc = "      \"description\": \"Workflow semantic version.\","]
#[doc = "      \"default\": \"0.0.1\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct Document {
    #[doc = "DSL version used by this workflow."]
    pub dsl: WorkflowDsl,
    #[doc = "Additional workflow metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Workflow name."]
    pub name: WorkflowName,
    #[doc = "Workflow namespace."]
    pub namespace: WorkflowNamespace,
    #[doc = "Workflow summary in Markdown format."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub summary: ::std::option::Option<::std::string::String>,
    #[doc = "Key/value tags for workflow classification."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub tags: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Workflow title."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "Workflow semantic version."]
    pub version: WorkflowVersion,
}
impl ::std::convert::From<&Document> for Document {
    fn from(value: &Document) -> Self {
        value.clone()
    }
}
impl Document {
    pub fn builder() -> builder::Document {
        Default::default()
    }
}
#[doc = "Duration"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"DurationInline\","]
#[doc = "      \"description\": \"Inline duration specification using individual time units.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"minProperties\": 1,"]
#[doc = "      \"properties\": {"]
#[doc = "        \"days\": {"]
#[doc = "          \"title\": \"DurationDays\","]
#[doc = "          \"description\": \"Number of days.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"hours\": {"]
#[doc = "          \"title\": \"DurationHours\","]
#[doc = "          \"description\": \"Number of hours.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"milliseconds\": {"]
#[doc = "          \"title\": \"DurationMilliseconds\","]
#[doc = "          \"description\": \"Number of milliseconds.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"minutes\": {"]
#[doc = "          \"title\": \"DurationMinutes\","]
#[doc = "          \"description\": \"Number of minutes.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"seconds\": {"]
#[doc = "          \"title\": \"DurationSeconds\","]
#[doc = "          \"description\": \"Number of seconds.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"DurationExpression\","]
#[doc = "      \"description\": \"Duration expressed in ISO 8601 format.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"pattern\": \"^P(?!$)(\\\\d+(?:\\\\.\\\\d+)?Y)?(\\\\d+(?:\\\\.\\\\d+)?M)?(\\\\d+(?:\\\\.\\\\d+)?W)?(\\\\d+(?:\\\\.\\\\d+)?D)?(T(?=\\\\d)(\\\\d+(?:\\\\.\\\\d+)?H)?(\\\\d+(?:\\\\.\\\\d+)?M)?(\\\\d+(?:\\\\.\\\\d+)?S)?)?$\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Duration {
    Inline {
        #[doc = "Number of days."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        days: ::std::option::Option<i64>,
        #[doc = "Number of hours."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        hours: ::std::option::Option<i64>,
        #[doc = "Number of milliseconds."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        milliseconds: ::std::option::Option<i64>,
        #[doc = "Number of minutes."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        minutes: ::std::option::Option<i64>,
        #[doc = "Number of seconds."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        seconds: ::std::option::Option<i64>,
    },
    Expression(DurationExpression),
}
impl ::std::convert::From<&Self> for Duration {
    fn from(value: &Duration) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<DurationExpression> for Duration {
    fn from(value: DurationExpression) -> Self {
        Self::Expression(value)
    }
}
#[doc = "Duration expressed in ISO 8601 format."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"DurationExpression\","]
#[doc = "  \"description\": \"Duration expressed in ISO 8601 format.\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"pattern\": \"^P(?!$)(\\\\d+(?:\\\\.\\\\d+)?Y)?(\\\\d+(?:\\\\.\\\\d+)?M)?(\\\\d+(?:\\\\.\\\\d+)?W)?(\\\\d+(?:\\\\.\\\\d+)?D)?(T(?=\\\\d)(\\\\d+(?:\\\\.\\\\d+)?H)?(\\\\d+(?:\\\\.\\\\d+)?M)?(\\\\d+(?:\\\\.\\\\d+)?S)?)?$\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Serialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct DurationExpression(::std::string::String);
impl ::std::ops::Deref for DurationExpression {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<DurationExpression> for ::std::string::String {
    fn from(value: DurationExpression) -> Self {
        value.0
    }
}
impl ::std::convert::From<&DurationExpression> for DurationExpression {
    fn from(value: &DurationExpression) -> Self {
        value.clone()
    }
}
impl ::std::str::FromStr for DurationExpression {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        if regress :: Regex :: new ("^P(?!$)(\\d+(?:\\.\\d+)?Y)?(\\d+(?:\\.\\d+)?M)?(\\d+(?:\\.\\d+)?W)?(\\d+(?:\\.\\d+)?D)?(T(?=\\d)(\\d+(?:\\.\\d+)?H)?(\\d+(?:\\.\\d+)?M)?(\\d+(?:\\.\\d+)?S)?)?$") . unwrap () . find (value) . is_none () { return Err ("doesn't match pattern \"^P(?!$)(\\d+(?:\\.\\d+)?Y)?(\\d+(?:\\.\\d+)?M)?(\\d+(?:\\.\\d+)?W)?(\\d+(?:\\.\\d+)?D)?(T(?=\\d)(\\d+(?:\\.\\d+)?H)?(\\d+(?:\\.\\d+)?M)?(\\d+(?:\\.\\d+)?S)?)?$\"" . into ()) ; }
        Ok(Self(value.to_string()))
    }
}
impl ::std::convert::TryFrom<&str> for DurationExpression {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for DurationExpression {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for DurationExpression {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl<'de> ::serde::Deserialize<'de> for DurationExpression {
    fn deserialize<D>(deserializer: D) -> ::std::result::Result<Self, D::Error>
    where
        D: ::serde::Deserializer<'de>,
    {
        ::std::string::String::deserialize(deserializer)?
            .parse()
            .map_err(|e: self::error::ConversionError| {
                <D::Error as ::serde::de::Error>::custom(e.to_string())
            })
    }
}
#[doc = "Endpoint configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Endpoint\","]
#[doc = "  \"description\": \"Endpoint configuration.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/uriTemplate\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"EndpointConfiguration\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"uri\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"uri\": {"]
#[doc = "          \"title\": \"EndpointUri\","]
#[doc = "          \"description\": \"Endpoint URI.\","]
#[doc = "          \"$ref\": \"#/$defs/uriTemplate\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Endpoint {
    UriTemplate(UriTemplate),
    EndpointConfiguration {
        #[doc = "Endpoint URI."]
        uri: UriTemplate,
    },
}
impl ::std::convert::From<&Self> for Endpoint {
    fn from(value: &Endpoint) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<UriTemplate> for Endpoint {
    fn from(value: UriTemplate) -> Self {
        Self::UriTemplate(value)
    }
}
#[doc = "Error definition."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Error\","]
#[doc = "  \"description\": \"Error definition.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"status\","]
#[doc = "    \"type\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"detail\": {"]
#[doc = "      \"title\": \"ErrorDetails\","]
#[doc = "      \"description\": \"Detailed error explanation.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"instance\": {"]
#[doc = "      \"title\": \"ErrorInstance\","]
#[doc = "      \"description\": \"JSON Pointer to error source component.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"format\": \"json-pointer\""]
#[doc = "    },"]
#[doc = "    \"status\": {"]
#[doc = "      \"title\": \"ErrorStatus\","]
#[doc = "      \"description\": \"HTTP status code for this error.\","]
#[doc = "      \"type\": \"integer\""]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"title\": \"ErrorTitle\","]
#[doc = "      \"description\": \"Brief error summary.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"type\": {"]
#[doc = "      \"title\": \"ErrorType\","]
#[doc = "      \"description\": \"URI identifying error type.\","]
#[doc = "      \"$ref\": \"#/$defs/uriTemplate\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct Error {
    #[doc = "Detailed error explanation."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub detail: ::std::option::Option<::std::string::String>,
    #[doc = "JSON Pointer to error source component."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub instance: ::std::option::Option<::std::string::String>,
    #[doc = "HTTP status code for this error."]
    pub status: i64,
    #[doc = "Brief error summary."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "URI identifying error type."]
    #[serde(rename = "type")]
    pub type_: UriTemplate,
}
impl ::std::convert::From<&Error> for Error {
    fn from(value: &Error) -> Self {
        value.clone()
    }
}
impl Error {
    pub fn builder() -> builder::Error {
        Default::default()
    }
}
#[doc = "Static error filtering configuration. For dynamic filtering, use catch.when property."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ErrorFilter\","]
#[doc = "  \"description\": \"Static error filtering configuration. For dynamic filtering, use catch.when property.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"minProperties\": 1,"]
#[doc = "  \"properties\": {"]
#[doc = "    \"details\": {"]
#[doc = "      \"description\": \"Filter by error details.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"instance\": {"]
#[doc = "      \"description\": \"Filter by error instance path.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"status\": {"]
#[doc = "      \"description\": \"Filter by error status code.\","]
#[doc = "      \"type\": \"integer\""]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"description\": \"Filter by error title.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"type\": {"]
#[doc = "      \"description\": \"Filter by error type value.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct ErrorFilter {
    #[doc = "Filter by error details."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub details: ::std::option::Option<::std::string::String>,
    #[doc = "Filter by error instance path."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub instance: ::std::option::Option<::std::string::String>,
    #[doc = "Filter by error status code."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub status: ::std::option::Option<i64>,
    #[doc = "Filter by error title."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "Filter by error type value."]
    #[serde(
        rename = "type",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub type_: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&ErrorFilter> for ErrorFilter {
    fn from(value: &ErrorFilter) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for ErrorFilter {
    fn default() -> Self {
        Self {
            details: Default::default(),
            instance: Default::default(),
            status: Default::default(),
            title: Default::default(),
            type_: Default::default(),
        }
    }
}
impl ErrorFilter {
    pub fn builder() -> builder::ErrorFilter {
        Default::default()
    }
}
#[doc = "Export configuration for context variables."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Export\","]
#[doc = "  \"description\": \"Export configuration for context variables.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"as\": {"]
#[doc = "      \"title\": \"ExportAs\","]
#[doc = "      \"description\": \"Runtime expression to export data to context.\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"object\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"schema\": {"]
#[doc = "      \"title\": \"ExportSchema\","]
#[doc = "      \"description\": \"Schema for context validation.\","]
#[doc = "      \"$ref\": \"#/$defs/schema\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct Export {
    #[doc = "Runtime expression to export data to context."]
    #[serde(
        rename = "as",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub as_: ::std::option::Option<ExportAs>,
    #[doc = "Schema for context validation."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub schema: ::std::option::Option<Schema>,
}
impl ::std::convert::From<&Export> for Export {
    fn from(value: &Export) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for Export {
    fn default() -> Self {
        Self {
            as_: Default::default(),
            schema: Default::default(),
        }
    }
}
impl Export {
    pub fn builder() -> builder::Export {
        Default::default()
    }
}
#[doc = "Runtime expression to export data to context."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ExportAs\","]
#[doc = "  \"description\": \"Runtime expression to export data to context.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"type\": \"object\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum ExportAs {
    Variant0(::std::string::String),
    Variant1(::serde_json::Map<::std::string::String, ::serde_json::Value>),
}
impl ::std::convert::From<&Self> for ExportAs {
    fn from(value: &ExportAs) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<::serde_json::Map<::std::string::String, ::serde_json::Value>>
    for ExportAs
{
    fn from(value: ::serde_json::Map<::std::string::String, ::serde_json::Value>) -> Self {
        Self::Variant1(value)
    }
}
#[doc = "External resource configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ExternalResource\","]
#[doc = "  \"description\": \"External resource configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"endpoint\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"endpoint\": {"]
#[doc = "      \"title\": \"ExternalResourceEndpoint\","]
#[doc = "      \"description\": \"Resource endpoint.\","]
#[doc = "      \"$ref\": \"#/$defs/endpoint\""]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"title\": \"ExternalResourceName\","]
#[doc = "      \"description\": \"Resource name.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct ExternalResource {
    #[doc = "Resource endpoint."]
    pub endpoint: Endpoint,
    #[doc = "Resource name."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub name: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&ExternalResource> for ExternalResource {
    fn from(value: &ExternalResource) -> Self {
        value.clone()
    }
}
impl ExternalResource {
    pub fn builder() -> builder::ExternalResource {
        Default::default()
    }
}
#[doc = "Control flow directive for workflow execution path."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"FlowDirective\","]
#[doc = "  \"description\": \"Control flow directive for workflow execution path.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"FlowDirectiveEnum\","]
#[doc = "      \"default\": \"continue\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"enum\": ["]
#[doc = "        \"continue\","]
#[doc = "        \"exit\","]
#[doc = "        \"end\""]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"Runtime expression evaluating to target task name.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum FlowDirective {
    Variant0(FlowDirectiveEnum),
    Variant1(::std::string::String),
}
impl ::std::convert::From<&Self> for FlowDirective {
    fn from(value: &FlowDirective) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<FlowDirectiveEnum> for FlowDirective {
    fn from(value: FlowDirectiveEnum) -> Self {
        Self::Variant0(value)
    }
}
#[doc = "FlowDirectiveEnum"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"FlowDirectiveEnum\","]
#[doc = "  \"default\": \"continue\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"continue\","]
#[doc = "    \"exit\","]
#[doc = "    \"end\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum FlowDirectiveEnum {
    #[serde(rename = "continue")]
    Continue,
    #[serde(rename = "exit")]
    Exit,
    #[serde(rename = "end")]
    End,
}
impl ::std::convert::From<&Self> for FlowDirectiveEnum {
    fn from(value: &FlowDirectiveEnum) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for FlowDirectiveEnum {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Continue => write!(f, "continue"),
            Self::Exit => write!(f, "exit"),
            Self::End => write!(f, "end"),
        }
    }
}
impl ::std::str::FromStr for FlowDirectiveEnum {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "continue" => Ok(Self::Continue),
            "exit" => Ok(Self::Exit),
            "end" => Ok(Self::End),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for FlowDirectiveEnum {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for FlowDirectiveEnum {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for FlowDirectiveEnum {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "Error handling strategy (continue to process remaining items or break the loop)."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ForOnError\","]
#[doc = "  \"description\": \"Error handling strategy (continue to process remaining items or break the loop).\","]
#[doc = "  \"default\": \"break\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"continue\","]
#[doc = "    \"break\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum ForOnError {
    #[serde(rename = "continue")]
    Continue,
    #[serde(rename = "break")]
    Break,
}
impl ::std::convert::From<&Self> for ForOnError {
    fn from(value: &ForOnError) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for ForOnError {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Continue => write!(f, "continue"),
            Self::Break => write!(f, "break"),
        }
    }
}
impl ::std::str::FromStr for ForOnError {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "continue" => Ok(Self::Continue),
            "break" => Ok(Self::Break),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for ForOnError {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ForOnError {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ForOnError {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::default::Default for ForOnError {
    fn default() -> Self {
        ForOnError::Break
    }
}
#[doc = "ForTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"do\","]
#[doc = "    \"for\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"do\": {"]
#[doc = "      \"title\": \"ForTaskDo\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"for\": {"]
#[doc = "      \"title\": \"ForTaskConfiguration\","]
#[doc = "      \"description\": \"Loop iteration configuration.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"in\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"at\": {"]
#[doc = "          \"title\": \"ForAt\","]
#[doc = "          \"description\": \"Variable name for current index (access via $variable_name for jq, {{ variable_name }} for liquid).\","]
#[doc = "          \"default\": \"index\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"each\": {"]
#[doc = "          \"title\": \"ForEach\","]
#[doc = "          \"description\": \"Variable name for current item (access via $variable_name for jq, {{ variable_name }} for liquid).\","]
#[doc = "          \"default\": \"item\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"in\": {"]
#[doc = "          \"title\": \"ForIn\","]
#[doc = "          \"description\": \"Runtime expression returning collection to iterate.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"inParallel\": {"]
#[doc = "      \"title\": \"ForInParallel\","]
#[doc = "      \"description\": \"Execute subtasks in parallel.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"onError\": {"]
#[doc = "      \"title\": \"ForOnError\","]
#[doc = "      \"description\": \"Error handling strategy (continue to process remaining items or break the loop).\","]
#[doc = "      \"default\": \"break\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"enum\": ["]
#[doc = "        \"continue\","]
#[doc = "        \"break\""]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"while\": {"]
#[doc = "      \"title\": \"While\","]
#[doc = "      \"description\": \"Runtime expression for loop continuation condition.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct ForTask {
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[serde(rename = "do")]
    pub do_: TaskList,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[serde(rename = "for")]
    pub for_: ForTaskConfiguration,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Execute subtasks in parallel."]
    #[serde(rename = "inParallel", default)]
    pub in_parallel: bool,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Error handling strategy (continue to process remaining items or break the loop)."]
    #[serde(rename = "onError", default = "defaults::for_task_on_error")]
    pub on_error: ForOnError,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
    #[doc = "Runtime expression for loop continuation condition."]
    #[serde(
        rename = "while",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub while_: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&ForTask> for ForTask {
    fn from(value: &ForTask) -> Self {
        value.clone()
    }
}
impl ForTask {
    pub fn builder() -> builder::ForTask {
        Default::default()
    }
}
#[doc = "Loop iteration configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ForTaskConfiguration\","]
#[doc = "  \"description\": \"Loop iteration configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"in\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"at\": {"]
#[doc = "      \"title\": \"ForAt\","]
#[doc = "      \"description\": \"Variable name for current index (access via $variable_name for jq, {{ variable_name }} for liquid).\","]
#[doc = "      \"default\": \"index\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"each\": {"]
#[doc = "      \"title\": \"ForEach\","]
#[doc = "      \"description\": \"Variable name for current item (access via $variable_name for jq, {{ variable_name }} for liquid).\","]
#[doc = "      \"default\": \"item\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"in\": {"]
#[doc = "      \"title\": \"ForIn\","]
#[doc = "      \"description\": \"Runtime expression returning collection to iterate.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct ForTaskConfiguration {
    #[doc = "Variable name for current index (access via $variable_name for jq, {{ variable_name }} for liquid)."]
    #[serde(default = "defaults::for_task_configuration_at")]
    pub at: ::std::string::String,
    #[doc = "Variable name for current item (access via $variable_name for jq, {{ variable_name }} for liquid)."]
    #[serde(default = "defaults::for_task_configuration_each")]
    pub each: ::std::string::String,
    #[doc = "Runtime expression returning collection to iterate."]
    #[serde(rename = "in")]
    pub in_: ::std::string::String,
}
impl ::std::convert::From<&ForTaskConfiguration> for ForTaskConfiguration {
    fn from(value: &ForTaskConfiguration) -> Self {
        value.clone()
    }
}
impl ForTaskConfiguration {
    pub fn builder() -> builder::ForTaskConfiguration {
        Default::default()
    }
}
#[doc = "ForkTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"fork\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"fork\": {"]
#[doc = "      \"title\": \"ForkTaskConfiguration\","]
#[doc = "      \"description\": \"Concurrent branch configuration.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"branches\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"branches\": {"]
#[doc = "          \"title\": \"ForkBranches\","]
#[doc = "          \"$ref\": \"#/$defs/taskList\""]
#[doc = "        },"]
#[doc = "        \"compete\": {"]
#[doc = "          \"title\": \"ForkCompete\","]
#[doc = "          \"description\": \"Enable competition mode where first completed task wins.\","]
#[doc = "          \"default\": false,"]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct ForkTask {
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    pub fork: ForkTaskConfiguration,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
}
impl ::std::convert::From<&ForkTask> for ForkTask {
    fn from(value: &ForkTask) -> Self {
        value.clone()
    }
}
impl ForkTask {
    pub fn builder() -> builder::ForkTask {
        Default::default()
    }
}
#[doc = "Concurrent branch configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ForkTaskConfiguration\","]
#[doc = "  \"description\": \"Concurrent branch configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"branches\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"branches\": {"]
#[doc = "      \"title\": \"ForkBranches\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    },"]
#[doc = "    \"compete\": {"]
#[doc = "      \"title\": \"ForkCompete\","]
#[doc = "      \"description\": \"Enable competition mode where first completed task wins.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct ForkTaskConfiguration {
    pub branches: TaskList,
    #[doc = "Enable competition mode where first completed task wins."]
    #[serde(default)]
    pub compete: bool,
}
impl ::std::convert::From<&ForkTaskConfiguration> for ForkTaskConfiguration {
    fn from(value: &ForkTaskConfiguration) -> Self {
        value.clone()
    }
}
impl ForkTaskConfiguration {
    pub fn builder() -> builder::ForkTaskConfiguration {
        Default::default()
    }
}
#[doc = "Input configuration for workflow or task."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Input\","]
#[doc = "  \"description\": \"Input configuration for workflow or task.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"from\": {"]
#[doc = "      \"title\": \"InputFrom\","]
#[doc = "      \"description\": \"Runtime expression to transform input data.\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"object\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"schema\": {"]
#[doc = "      \"title\": \"InputSchema\","]
#[doc = "      \"description\": \"Schema for input validation.\","]
#[doc = "      \"$ref\": \"#/$defs/schema\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct Input {
    #[doc = "Runtime expression to transform input data."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub from: ::std::option::Option<InputFrom>,
    #[doc = "Schema for input validation."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub schema: ::std::option::Option<Schema>,
}
impl ::std::convert::From<&Input> for Input {
    fn from(value: &Input) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for Input {
    fn default() -> Self {
        Self {
            from: Default::default(),
            schema: Default::default(),
        }
    }
}
impl Input {
    pub fn builder() -> builder::Input {
        Default::default()
    }
}
#[doc = "Runtime expression to transform input data."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"InputFrom\","]
#[doc = "  \"description\": \"Runtime expression to transform input data.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"type\": \"object\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum InputFrom {
    Variant0(::std::string::String),
    Variant1(::serde_json::Map<::std::string::String, ::serde_json::Value>),
}
impl ::std::convert::From<&Self> for InputFrom {
    fn from(value: &InputFrom) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<::serde_json::Map<::std::string::String, ::serde_json::Value>>
    for InputFrom
{
    fn from(value: ::serde_json::Map<::std::string::String, ::serde_json::Value>) -> Self {
        Self::Variant1(value)
    }
}
#[doc = "Output configuration for workflow or task."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Output\","]
#[doc = "  \"description\": \"Output configuration for workflow or task.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"as\": {"]
#[doc = "      \"title\": \"OutputAs\","]
#[doc = "      \"description\": \"Runtime expression to transform output data.\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"type\": \"object\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"schema\": {"]
#[doc = "      \"title\": \"OutputSchema\","]
#[doc = "      \"description\": \"Schema for output validation.\","]
#[doc = "      \"$ref\": \"#/$defs/schema\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct Output {
    #[doc = "Runtime expression to transform output data."]
    #[serde(
        rename = "as",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub as_: ::std::option::Option<OutputAs>,
    #[doc = "Schema for output validation."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub schema: ::std::option::Option<Schema>,
}
impl ::std::convert::From<&Output> for Output {
    fn from(value: &Output) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for Output {
    fn default() -> Self {
        Self {
            as_: Default::default(),
            schema: Default::default(),
        }
    }
}
impl Output {
    pub fn builder() -> builder::Output {
        Default::default()
    }
}
#[doc = "Runtime expression to transform output data."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"OutputAs\","]
#[doc = "  \"description\": \"Runtime expression to transform output data.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"type\": \"object\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum OutputAs {
    Variant0(::std::string::String),
    Variant1(::serde_json::Map<::std::string::String, ::serde_json::Value>),
}
impl ::std::convert::From<&Self> for OutputAs {
    fn from(value: &OutputAs) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<::serde_json::Map<::std::string::String, ::serde_json::Value>>
    for OutputAs
{
    fn from(value: ::serde_json::Map<::std::string::String, ::serde_json::Value>) -> Self {
        Self::Variant1(value)
    }
}
#[doc = "Plain string without runtime expressions."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"PlainString\","]
#[doc = "  \"description\": \"Plain string without runtime expressions.\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"pattern\": \"^\\\\s*[^\\\\$\\\\{].*[^\\\\$\\\\{]\\\\s*$\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Serialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct PlainString(::std::string::String);
impl ::std::ops::Deref for PlainString {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<PlainString> for ::std::string::String {
    fn from(value: PlainString) -> Self {
        value.0
    }
}
impl ::std::convert::From<&PlainString> for PlainString {
    fn from(value: &PlainString) -> Self {
        value.clone()
    }
}
impl ::std::str::FromStr for PlainString {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        if regress::Regex::new("^\\s*[^\\$\\{].*[^\\$\\{]\\s*$")
            .unwrap()
            .find(value)
            .is_none()
        {
            return Err("doesn't match pattern \"^\\s*[^\\$\\{].*[^\\$\\{]\\s*$\"".into());
        }
        Ok(Self(value.to_string()))
    }
}
impl ::std::convert::TryFrom<&str> for PlainString {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for PlainString {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for PlainString {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl<'de> ::serde::Deserialize<'de> for PlainString {
    fn deserialize<D>(deserializer: D) -> ::std::result::Result<Self, D::Error>
    where
        D: ::serde::Deserializer<'de>,
    {
        ::std::string::String::deserialize(deserializer)?
            .parse()
            .map_err(|e: self::error::ConversionError| {
                <D::Error as ::serde::de::Error>::custom(e.to_string())
            })
    }
}
#[doc = "Process execution result when return type is 'all'."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ProcessResult\","]
#[doc = "  \"description\": \"Process execution result when return type is 'all'.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"code\","]
#[doc = "    \"stderr\","]
#[doc = "    \"stdout\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"code\": {"]
#[doc = "      \"title\": \"ProcessExitCode\","]
#[doc = "      \"description\": \"Process exit code.\","]
#[doc = "      \"type\": \"integer\""]
#[doc = "    },"]
#[doc = "    \"stderr\": {"]
#[doc = "      \"title\": \"ProcessStandardError\","]
#[doc = "      \"description\": \"Process STDERR content.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"stdout\": {"]
#[doc = "      \"title\": \"ProcessStandardOutput\","]
#[doc = "      \"description\": \"Process STDOUT content.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct ProcessResult {
    #[doc = "Process exit code."]
    pub code: i64,
    #[doc = "Process STDERR content."]
    pub stderr: ::std::string::String,
    #[doc = "Process STDOUT content."]
    pub stdout: ::std::string::String,
}
impl ::std::convert::From<&ProcessResult> for ProcessResult {
    fn from(value: &ProcessResult) -> Self {
        value.clone()
    }
}
impl ProcessResult {
    pub fn builder() -> builder::ProcessResult {
        Default::default()
    }
}
#[doc = "Defines how jobs are queued and persisted. Values: NORMAL (default, in-memory only), WITH_BACKUP (in-memory with database backup), DB_ONLY (database only)."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"QueueType\","]
#[doc = "  \"description\": \"Defines how jobs are queued and persisted. Values: NORMAL (default, in-memory only), WITH_BACKUP (in-memory with database backup), DB_ONLY (database only).\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"NORMAL\","]
#[doc = "    \"WITH_BACKUP\","]
#[doc = "    \"DB_ONLY\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum QueueType {
    #[serde(rename = "NORMAL")]
    Normal,
    #[serde(rename = "WITH_BACKUP")]
    WithBackup,
    #[serde(rename = "DB_ONLY")]
    DbOnly,
}
impl ::std::convert::From<&Self> for QueueType {
    fn from(value: &QueueType) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for QueueType {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Normal => write!(f, "NORMAL"),
            Self::WithBackup => write!(f, "WITH_BACKUP"),
            Self::DbOnly => write!(f, "DB_ONLY"),
        }
    }
}
impl ::std::str::FromStr for QueueType {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "NORMAL" => Ok(Self::Normal),
            "WITH_BACKUP" => Ok(Self::WithBackup),
            "DB_ONLY" => Ok(Self::DbOnly),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for QueueType {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for QueueType {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for QueueType {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "RaiseTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"raise\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"raise\": {"]
#[doc = "      \"title\": \"RaiseTaskConfiguration\","]
#[doc = "      \"description\": \"Error to raise.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"error\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"error\": {"]
#[doc = "          \"title\": \"RaiseTaskError\","]
#[doc = "          \"oneOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"RaiseErrorDefinition\","]
#[doc = "              \"description\": \"Inline error definition.\","]
#[doc = "              \"$ref\": \"#/$defs/error\""]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"RaiseErrorReference\","]
#[doc = "              \"description\": \"Reference to named error definition.\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct RaiseTask {
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    pub raise: RaiseTaskConfiguration,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
}
impl ::std::convert::From<&RaiseTask> for RaiseTask {
    fn from(value: &RaiseTask) -> Self {
        value.clone()
    }
}
impl RaiseTask {
    pub fn builder() -> builder::RaiseTask {
        Default::default()
    }
}
#[doc = "Error to raise."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RaiseTaskConfiguration\","]
#[doc = "  \"description\": \"Error to raise.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"error\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"error\": {"]
#[doc = "      \"title\": \"RaiseTaskError\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"RaiseErrorDefinition\","]
#[doc = "          \"description\": \"Inline error definition.\","]
#[doc = "          \"$ref\": \"#/$defs/error\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"RaiseErrorReference\","]
#[doc = "          \"description\": \"Reference to named error definition.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct RaiseTaskConfiguration {
    pub error: RaiseTaskError,
}
impl ::std::convert::From<&RaiseTaskConfiguration> for RaiseTaskConfiguration {
    fn from(value: &RaiseTaskConfiguration) -> Self {
        value.clone()
    }
}
impl RaiseTaskConfiguration {
    pub fn builder() -> builder::RaiseTaskConfiguration {
        Default::default()
    }
}
#[doc = "RaiseTaskError"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RaiseTaskError\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"RaiseErrorDefinition\","]
#[doc = "      \"description\": \"Inline error definition.\","]
#[doc = "      \"$ref\": \"#/$defs/error\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"RaiseErrorReference\","]
#[doc = "      \"description\": \"Reference to named error definition.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum RaiseTaskError {
    Error(Error),
    RaiseErrorReference(::std::string::String),
}
impl ::std::convert::From<&Self> for RaiseTaskError {
    fn from(value: &RaiseTaskError) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<Error> for RaiseTaskError {
    fn from(value: Error) -> Self {
        Self::Error(value)
    }
}
#[doc = "Defines how job results should be returned to the client. Values: NO_RESULT (async, no wait), DIRECT (default, synchronous with result)."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ResponseType\","]
#[doc = "  \"description\": \"Defines how job results should be returned to the client. Values: NO_RESULT (async, no wait), DIRECT (default, synchronous with result).\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"NO_RESULT\","]
#[doc = "    \"DIRECT\""]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum ResponseType {
    #[serde(rename = "NO_RESULT")]
    NoResult,
    #[serde(rename = "DIRECT")]
    Direct,
}
impl ::std::convert::From<&Self> for ResponseType {
    fn from(value: &ResponseType) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for ResponseType {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::NoResult => write!(f, "NO_RESULT"),
            Self::Direct => write!(f, "DIRECT"),
        }
    }
}
impl ::std::str::FromStr for ResponseType {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "NO_RESULT" => Ok(Self::NoResult),
            "DIRECT" => Ok(Self::Direct),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for ResponseType {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ResponseType {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ResponseType {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
#[doc = "Backoff strategy for retry durations."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RetryBackoff\","]
#[doc = "  \"description\": \"Backoff strategy for retry durations.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"ConstantBackoff\","]
#[doc = "      \"required\": ["]
#[doc = "        \"constant\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"constant\": {"]
#[doc = "          \"description\": \"Constant backoff configuration (empty object).\","]
#[doc = "          \"type\": \"object\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExponentialBackOff\","]
#[doc = "      \"required\": ["]
#[doc = "        \"exponential\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"exponential\": {"]
#[doc = "          \"description\": \"Exponential backoff configuration (empty object).\","]
#[doc = "          \"type\": \"object\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LinearBackoff\","]
#[doc = "      \"required\": ["]
#[doc = "        \"linear\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"linear\": {"]
#[doc = "          \"description\": \"Linear backoff configuration (empty object).\","]
#[doc = "          \"type\": \"object\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub enum RetryBackoff {
    #[serde(rename = "constant")]
    Constant(::serde_json::Map<::std::string::String, ::serde_json::Value>),
    #[serde(rename = "exponential")]
    Exponential(::serde_json::Map<::std::string::String, ::serde_json::Value>),
    #[serde(rename = "linear")]
    Linear(::serde_json::Map<::std::string::String, ::serde_json::Value>),
}
impl ::std::convert::From<&Self> for RetryBackoff {
    fn from(value: &RetryBackoff) -> Self {
        value.clone()
    }
}
#[doc = "Retry limits configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RetryLimit\","]
#[doc = "  \"description\": \"Retry limits configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"attempt\": {"]
#[doc = "      \"title\": \"RetryLimitAttempt\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"properties\": {"]
#[doc = "        \"count\": {"]
#[doc = "          \"title\": \"RetryLimitAttemptCount\","]
#[doc = "          \"description\": \"Maximum retry attempts.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"duration\": {"]
#[doc = "          \"title\": \"RetryLimitAttemptDuration\","]
#[doc = "          \"description\": \"Maximum duration per retry attempt.\","]
#[doc = "          \"$ref\": \"#/$defs/duration\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct RetryLimit {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub attempt: ::std::option::Option<RetryLimitAttempt>,
}
impl ::std::convert::From<&RetryLimit> for RetryLimit {
    fn from(value: &RetryLimit) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for RetryLimit {
    fn default() -> Self {
        Self {
            attempt: Default::default(),
        }
    }
}
impl RetryLimit {
    pub fn builder() -> builder::RetryLimit {
        Default::default()
    }
}
#[doc = "RetryLimitAttempt"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RetryLimitAttempt\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"count\": {"]
#[doc = "      \"title\": \"RetryLimitAttemptCount\","]
#[doc = "      \"description\": \"Maximum retry attempts.\","]
#[doc = "      \"type\": \"integer\""]
#[doc = "    },"]
#[doc = "    \"duration\": {"]
#[doc = "      \"title\": \"RetryLimitAttemptDuration\","]
#[doc = "      \"description\": \"Maximum duration per retry attempt.\","]
#[doc = "      \"$ref\": \"#/$defs/duration\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct RetryLimitAttempt {
    #[doc = "Maximum retry attempts."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub count: ::std::option::Option<i64>,
    #[doc = "Maximum duration per retry attempt."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub duration: ::std::option::Option<Duration>,
}
impl ::std::convert::From<&RetryLimitAttempt> for RetryLimitAttempt {
    fn from(value: &RetryLimitAttempt) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for RetryLimitAttempt {
    fn default() -> Self {
        Self {
            count: Default::default(),
            duration: Default::default(),
        }
    }
}
impl RetryLimitAttempt {
    pub fn builder() -> builder::RetryLimitAttempt {
        Default::default()
    }
}
#[doc = "Retry policy configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RetryPolicy\","]
#[doc = "  \"description\": \"Retry policy configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"backoff\": {"]
#[doc = "      \"title\": \"RetryBackoff\","]
#[doc = "      \"description\": \"Backoff strategy for retry durations.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"ConstantBackoff\","]
#[doc = "          \"required\": ["]
#[doc = "            \"constant\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"constant\": {"]
#[doc = "              \"description\": \"Constant backoff configuration (empty object).\","]
#[doc = "              \"type\": \"object\""]
#[doc = "            }"]
#[doc = "          }"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"ExponentialBackOff\","]
#[doc = "          \"required\": ["]
#[doc = "            \"exponential\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"exponential\": {"]
#[doc = "              \"description\": \"Exponential backoff configuration (empty object).\","]
#[doc = "              \"type\": \"object\""]
#[doc = "            }"]
#[doc = "          }"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"LinearBackoff\","]
#[doc = "          \"required\": ["]
#[doc = "            \"linear\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"linear\": {"]
#[doc = "              \"description\": \"Linear backoff configuration (empty object).\","]
#[doc = "              \"type\": \"object\""]
#[doc = "            }"]
#[doc = "          }"]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"delay\": {"]
#[doc = "      \"title\": \"RetryDelay\","]
#[doc = "      \"description\": \"Delay between retry attempts.\","]
#[doc = "      \"$ref\": \"#/$defs/duration\""]
#[doc = "    },"]
#[doc = "    \"limit\": {"]
#[doc = "      \"title\": \"RetryLimit\","]
#[doc = "      \"description\": \"Retry limits configuration.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"properties\": {"]
#[doc = "        \"attempt\": {"]
#[doc = "          \"title\": \"RetryLimitAttempt\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"properties\": {"]
#[doc = "            \"count\": {"]
#[doc = "              \"title\": \"RetryLimitAttemptCount\","]
#[doc = "              \"description\": \"Maximum retry attempts.\","]
#[doc = "              \"type\": \"integer\""]
#[doc = "            },"]
#[doc = "            \"duration\": {"]
#[doc = "              \"title\": \"RetryLimitAttemptDuration\","]
#[doc = "              \"description\": \"Maximum duration per retry attempt.\","]
#[doc = "              \"$ref\": \"#/$defs/duration\""]
#[doc = "            }"]
#[doc = "          },"]
#[doc = "          \"unevaluatedProperties\": false"]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct RetryPolicy {
    #[doc = "Backoff strategy for retry durations."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub backoff: ::std::option::Option<RetryBackoff>,
    #[doc = "Delay between retry attempts."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub delay: ::std::option::Option<Duration>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub limit: ::std::option::Option<RetryLimit>,
}
impl ::std::convert::From<&RetryPolicy> for RetryPolicy {
    fn from(value: &RetryPolicy) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for RetryPolicy {
    fn default() -> Self {
        Self {
            backoff: Default::default(),
            delay: Default::default(),
            limit: Default::default(),
        }
    }
}
impl RetryPolicy {
    pub fn builder() -> builder::RetryPolicy {
        Default::default()
    }
}
#[doc = "Execute using a function configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RunFunction\","]
#[doc = "  \"description\": \"Execute using a function configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"function\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"function\": {"]
#[doc = "      \"title\": \"RunJobFunction\","]
#[doc = "      \"description\": \"Executes a job using a specified function(runner or worker).\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"RunnerFunction\","]
#[doc = "          \"description\": \"Execute using a runner with optional settings\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"required\": ["]
#[doc = "            \"arguments\","]
#[doc = "            \"runnerName\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"arguments\": {"]
#[doc = "              \"description\": \"A key/value mapping of arguments JSON (ref. jobworkerp.data.RunnerData.job_args_proto schema) to use when running the function. Runtime expressions are supported for value transformation.\","]
#[doc = "              \"type\": \"object\","]
#[doc = "              \"additionalProperties\": true"]
#[doc = "            },"]
#[doc = "            \"options\": {"]
#[doc = "              \"$ref\": \"#/$defs/workerOptions\""]
#[doc = "            },"]
#[doc = "            \"runnerName\": {"]
#[doc = "              \"description\": \"The name of the runner that executes job\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            },"]
#[doc = "            \"settings\": {"]
#[doc = "              \"description\": \"The initialization settings JSON, if any. (ref. jobworkerp.data.RunnerData.runner_settings_proto schema) Runtime expression can be used to transform each value (not keys, no mixed plain text).\","]
#[doc = "              \"type\": \"object\""]
#[doc = "            },"]
#[doc = "            \"using\": {"]
#[doc = "              \"description\": \"Selects which implementation to use for MCP/Plugin runners with multiple tools.\\n\\n- **Required**: For runners with multiple tools\\n- **Optional**: For single-tool runners (auto-selected)\\n\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            }"]
#[doc = "          },"]
#[doc = "          \"additionalProperties\": false"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"WorkerFunction\","]
#[doc = "          \"description\": \"Execute using a pre-configured worker\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"required\": ["]
#[doc = "            \"arguments\","]
#[doc = "            \"workerName\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"arguments\": {"]
#[doc = "              \"description\": \"A key/value mapping of arguments JSON to use when running the function. Runtime expressions are supported for value transformation.\","]
#[doc = "              \"type\": \"object\","]
#[doc = "              \"additionalProperties\": true"]
#[doc = "            },"]
#[doc = "            \"using\": {"]
#[doc = "              \"title\": \"Using\","]
#[doc = "              \"description\": \"Selects which method to use for multi-method runners.\\n\\nFor single-method runners, this field is optional (auto-selected if omitted).\\n\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            },"]
#[doc = "            \"workerName\": {"]
#[doc = "              \"description\": \"The name of the worker that executes job\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            }"]
#[doc = "          },"]
#[doc = "          \"additionalProperties\": false"]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"additionalProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunFunction {
    #[doc = "Executes a job using a specified function(runner or worker)."]
    pub function: RunJobFunction,
}
impl ::std::convert::From<&RunFunction> for RunFunction {
    fn from(value: &RunFunction) -> Self {
        value.clone()
    }
}
impl RunFunction {
    pub fn builder() -> builder::RunFunction {
        Default::default()
    }
}
#[doc = "Executes a job using a specified function(runner or worker)."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RunJobFunction\","]
#[doc = "  \"description\": \"Executes a job using a specified function(runner or worker).\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"RunnerFunction\","]
#[doc = "      \"description\": \"Execute using a runner with optional settings\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"arguments\","]
#[doc = "        \"runnerName\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"arguments\": {"]
#[doc = "          \"description\": \"A key/value mapping of arguments JSON (ref. jobworkerp.data.RunnerData.job_args_proto schema) to use when running the function. Runtime expressions are supported for value transformation.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": true"]
#[doc = "        },"]
#[doc = "        \"options\": {"]
#[doc = "          \"$ref\": \"#/$defs/workerOptions\""]
#[doc = "        },"]
#[doc = "        \"runnerName\": {"]
#[doc = "          \"description\": \"The name of the runner that executes job\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"settings\": {"]
#[doc = "          \"description\": \"The initialization settings JSON, if any. (ref. jobworkerp.data.RunnerData.runner_settings_proto schema) Runtime expression can be used to transform each value (not keys, no mixed plain text).\","]
#[doc = "          \"type\": \"object\""]
#[doc = "        },"]
#[doc = "        \"using\": {"]
#[doc = "          \"description\": \"Selects which implementation to use for MCP/Plugin runners with multiple tools.\\n\\n- **Required**: For runners with multiple tools\\n- **Optional**: For single-tool runners (auto-selected)\\n\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"additionalProperties\": false"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"WorkerFunction\","]
#[doc = "      \"description\": \"Execute using a pre-configured worker\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"arguments\","]
#[doc = "        \"workerName\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"arguments\": {"]
#[doc = "          \"description\": \"A key/value mapping of arguments JSON to use when running the function. Runtime expressions are supported for value transformation.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": true"]
#[doc = "        },"]
#[doc = "        \"using\": {"]
#[doc = "          \"title\": \"Using\","]
#[doc = "          \"description\": \"Selects which method to use for multi-method runners.\\n\\nFor single-method runners, this field is optional (auto-selected if omitted).\\n\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"workerName\": {"]
#[doc = "          \"description\": \"The name of the worker that executes job\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"additionalProperties\": false"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged, deny_unknown_fields)]
pub enum RunJobFunction {
    RunnerFunction {
        #[doc = "A key/value mapping of arguments JSON (ref. jobworkerp.data.RunnerData.job_args_proto schema) to use when running the function. Runtime expressions are supported for value transformation."]
        arguments: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        options: ::std::option::Option<WorkerOptions>,
        #[doc = "The name of the runner that executes job"]
        #[serde(rename = "runnerName")]
        runner_name: ::std::string::String,
        #[doc = "The initialization settings JSON, if any. (ref. jobworkerp.data.RunnerData.runner_settings_proto schema) Runtime expression can be used to transform each value (not keys, no mixed plain text)."]
        #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
        settings: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
        #[doc = "Selects which implementation to use for MCP/Plugin runners with multiple tools.\n\n- **Required**: For runners with multiple tools\n- **Optional**: For single-tool runners (auto-selected)\n"]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        using: ::std::option::Option<::std::string::String>,
    },
    WorkerFunction {
        #[doc = "A key/value mapping of arguments JSON to use when running the function. Runtime expressions are supported for value transformation."]
        arguments: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
        #[doc = "Selects which method to use for multi-method runners.\n\nFor single-method runners, this field is optional (auto-selected if omitted).\n"]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        using: ::std::option::Option<::std::string::String>,
        #[doc = "The name of the worker that executes job"]
        #[serde(rename = "workerName")]
        worker_name: ::std::string::String,
    },
}
impl ::std::convert::From<&Self> for RunJobFunction {
    fn from(value: &RunJobFunction) -> Self {
        value.clone()
    }
}
#[doc = "Executes a job using a specified runner."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RunJobRunner\","]
#[doc = "  \"description\": \"Executes a job using a specified runner.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"arguments\","]
#[doc = "    \"name\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"arguments\": {"]
#[doc = "      \"title\": \"JobArguments\","]
#[doc = "      \"description\": \"A key/value mapping of arguments to use when running the runner as job. Runtime expressions are supported for value transformation.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"title\": \"RunnerName\","]
#[doc = "      \"description\": \"The name of the runner (runtime environment) that executes the job (e.g., COMMAND, HTTP, GRPC, PYTHON_COMMAND, LLM_CHAT, MCP server names, plugin names, etc.)\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"options\": {"]
#[doc = "      \"$ref\": \"#/$defs/workerOptions\""]
#[doc = "    },"]
#[doc = "    \"settings\": {"]
#[doc = "      \"title\": \"InitializeSettings\","]
#[doc = "      \"description\": \"The initialization settings, if any. Runtime expressions can be used to transform each value (not keys, no mixed plain text).\","]
#[doc = "      \"type\": \"object\""]
#[doc = "    },"]
#[doc = "    \"using\": {"]
#[doc = "      \"title\": \"Using\","]
#[doc = "      \"description\": \"Selects which implementation to use for MCP/Plugin runners with multiple tools.\\n\\n- **Required**: For runners with multiple tools (e.g., fetch server with \\\"fetch\\\" and \\\"fetch_html\\\")\\n- **Optional**: For single-tool runners (auto-selected)\\n\\nExamples:\\n- MCP fetch server: using: \\\"fetch_html\\\"\\n- MCP time server (single tool): can be omitted\\n- COMMAND runner: not needed\\n\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct RunJobRunner {
    #[doc = "A key/value mapping of arguments to use when running the runner as job. Runtime expressions are supported for value transformation."]
    pub arguments: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "The name of the runner (runtime environment) that executes the job (e.g., COMMAND, HTTP, GRPC, PYTHON_COMMAND, LLM_CHAT, MCP server names, plugin names, etc.)"]
    pub name: ::std::string::String,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub options: ::std::option::Option<WorkerOptions>,
    #[doc = "The initialization settings, if any. Runtime expressions can be used to transform each value (not keys, no mixed plain text)."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub settings: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Selects which implementation to use for MCP/Plugin runners with multiple tools.\n\n- **Required**: For runners with multiple tools (e.g., fetch server with \"fetch\" and \"fetch_html\")\n- **Optional**: For single-tool runners (auto-selected)\n\nExamples:\n- MCP fetch server: using: \"fetch_html\"\n- MCP time server (single tool): can be omitted\n- COMMAND runner: not needed\n"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub using: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&RunJobRunner> for RunJobRunner {
    fn from(value: &RunJobRunner) -> Self {
        value.clone()
    }
}
impl RunJobRunner {
    pub fn builder() -> builder::RunJobRunner {
        Default::default()
    }
}
#[doc = "Executes a job using a specified worker (configured runner with settings and options)."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RunJobWorker\","]
#[doc = "  \"description\": \"Executes a job using a specified worker (configured runner with settings and options).\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"arguments\","]
#[doc = "    \"name\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"arguments\": {"]
#[doc = "      \"title\": \"FunctionArguments\","]
#[doc = "      \"description\": \"A key/value mapping of arguments to use when running the function. Runtime expressions are supported for value transformation.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"title\": \"WorkerName\","]
#[doc = "      \"description\": \"The name of the worker that executes this job (user-defined).\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"using\": {"]
#[doc = "      \"title\": \"Using\","]
#[doc = "      \"description\": \"Selects which method to use for multi-method runners.\\n\\nFor single-method runners, this field is optional (auto-selected if omitted).\\n\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct RunJobWorker {
    #[doc = "A key/value mapping of arguments to use when running the function. Runtime expressions are supported for value transformation."]
    pub arguments: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "The name of the worker that executes this job (user-defined)."]
    pub name: ::std::string::String,
    #[doc = "Selects which method to use for multi-method runners.\n\nFor single-method runners, this field is optional (auto-selected if omitted).\n"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub using: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&RunJobWorker> for RunJobWorker {
    fn from(value: &RunJobWorker) -> Self {
        value.clone()
    }
}
impl RunJobWorker {
    pub fn builder() -> builder::RunJobWorker {
        Default::default()
    }
}
#[doc = "Execute using a runner configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RunRunner\","]
#[doc = "  \"description\": \"Execute using a runner configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"runner\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"runner\": {"]
#[doc = "      \"title\": \"RunJobRunner\","]
#[doc = "      \"description\": \"Executes a job using a specified runner.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"arguments\","]
#[doc = "        \"name\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"arguments\": {"]
#[doc = "          \"title\": \"JobArguments\","]
#[doc = "          \"description\": \"A key/value mapping of arguments to use when running the runner as job. Runtime expressions are supported for value transformation.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": true"]
#[doc = "        },"]
#[doc = "        \"name\": {"]
#[doc = "          \"title\": \"RunnerName\","]
#[doc = "          \"description\": \"The name of the runner (runtime environment) that executes the job (e.g., COMMAND, HTTP, GRPC, PYTHON_COMMAND, LLM_CHAT, MCP server names, plugin names, etc.)\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"options\": {"]
#[doc = "          \"$ref\": \"#/$defs/workerOptions\""]
#[doc = "        },"]
#[doc = "        \"settings\": {"]
#[doc = "          \"title\": \"InitializeSettings\","]
#[doc = "          \"description\": \"The initialization settings, if any. Runtime expressions can be used to transform each value (not keys, no mixed plain text).\","]
#[doc = "          \"type\": \"object\""]
#[doc = "        },"]
#[doc = "        \"using\": {"]
#[doc = "          \"title\": \"Using\","]
#[doc = "          \"description\": \"Selects which implementation to use for MCP/Plugin runners with multiple tools.\\n\\n- **Required**: For runners with multiple tools (e.g., fetch server with \\\"fetch\\\" and \\\"fetch_html\\\")\\n- **Optional**: For single-tool runners (auto-selected)\\n\\nExamples:\\n- MCP fetch server: using: \\\"fetch_html\\\"\\n- MCP time server (single tool): can be omitted\\n- COMMAND runner: not needed\\n\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"additionalProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunRunner {
    pub runner: RunJobRunner,
}
impl ::std::convert::From<&RunRunner> for RunRunner {
    fn from(value: &RunRunner) -> Self {
        value.clone()
    }
}
impl RunRunner {
    pub fn builder() -> builder::RunRunner {
        Default::default()
    }
}
#[doc = "Execute inline or external scripts (Serverless Workflow v1.0.0 compliant)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RunScript\","]
#[doc = "  \"description\": \"Execute inline or external scripts (Serverless Workflow v1.0.0 compliant)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"script\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"script\": {"]
#[doc = "      \"title\": \"ScriptConfiguration\","]
#[doc = "      \"description\": \"Script execution configuration\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"required\": ["]
#[doc = "            \"code\""]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"required\": ["]
#[doc = "            \"source\""]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"required\": ["]
#[doc = "        \"language\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"arguments\": {"]
#[doc = "          \"title\": \"ScriptArguments\","]
#[doc = "          \"description\": \"Arguments passed to the script. Runtime expressions are supported for value transformation. Each key becomes a variable in the script.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": true"]
#[doc = "        },"]
#[doc = "        \"code\": {"]
#[doc = "          \"title\": \"InlineCode\","]
#[doc = "          \"description\": \"Inline script code to execute\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"environment\": {"]
#[doc = "          \"title\": \"EnvironmentVariables\","]
#[doc = "          \"description\": \"Environment variables for script execution\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": {"]
#[doc = "            \"type\": \"string\""]
#[doc = "          }"]
#[doc = "        },"]
#[doc = "        \"language\": {"]
#[doc = "          \"title\": \"ScriptLanguage\","]
#[doc = "          \"description\": \"Script language identifier (e.g., 'python', 'javascript'). Validated at runtime.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"source\": {"]
#[doc = "          \"title\": \"ExternalSource\","]
#[doc = "          \"description\": \"External script resource reference\","]
#[doc = "          \"$ref\": \"#/$defs/externalResource\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": true"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"additionalProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunScript {
    pub script: ScriptConfiguration,
}
impl ::std::convert::From<&RunScript> for RunScript {
    fn from(value: &RunScript) -> Self {
        value.clone()
    }
}
impl RunScript {
    pub fn builder() -> builder::RunScript {
        Default::default()
    }
}
#[doc = "RunTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"run\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"run\": {"]
#[doc = "      \"title\": \"RunTaskConfiguration\","]
#[doc = "      \"description\": \"Process execution configuration.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/runWorker\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/runRunner\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/runFunction\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"$ref\": \"#/$defs/runScript\""]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"await\": {"]
#[doc = "          \"title\": \"AwaitProcessCompletion\","]
#[doc = "          \"description\": \"Wait for process completion before continuing.\","]
#[doc = "          \"default\": true,"]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        },"]
#[doc = "        \"return\": {"]
#[doc = "          \"title\": \"ProcessReturnType\","]
#[doc = "          \"description\": \"Process output type to return.\","]
#[doc = "          \"default\": \"stdout\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"enum\": ["]
#[doc = "            \"stdout\","]
#[doc = "            \"stderr\","]
#[doc = "            \"code\","]
#[doc = "            \"all\","]
#[doc = "            \"none\""]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct RunTask {
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    pub run: RunTaskConfiguration,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
}
impl ::std::convert::From<&RunTask> for RunTask {
    fn from(value: &RunTask) -> Self {
        value.clone()
    }
}
impl RunTask {
    pub fn builder() -> builder::RunTask {
        Default::default()
    }
}
#[doc = "Process execution configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RunTaskConfiguration\","]
#[doc = "  \"description\": \"Process execution configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/runWorker\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/runRunner\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/runFunction\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/runScript\""]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"await\": {"]
#[doc = "      \"title\": \"AwaitProcessCompletion\","]
#[doc = "      \"description\": \"Wait for process completion before continuing.\","]
#[doc = "      \"default\": true,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"return\": {"]
#[doc = "      \"title\": \"ProcessReturnType\","]
#[doc = "      \"description\": \"Process output type to return.\","]
#[doc = "      \"default\": \"stdout\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"enum\": ["]
#[doc = "        \"stdout\","]
#[doc = "        \"stderr\","]
#[doc = "        \"code\","]
#[doc = "        \"all\","]
#[doc = "        \"none\""]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum RunTaskConfiguration {
    Worker(RunWorker),
    Runner(RunRunner),
    Function(RunFunction),
    Script(RunScript),
}
impl ::std::convert::From<&Self> for RunTaskConfiguration {
    fn from(value: &RunTaskConfiguration) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<RunWorker> for RunTaskConfiguration {
    fn from(value: RunWorker) -> Self {
        Self::Worker(value)
    }
}
impl ::std::convert::From<RunRunner> for RunTaskConfiguration {
    fn from(value: RunRunner) -> Self {
        Self::Runner(value)
    }
}
impl ::std::convert::From<RunFunction> for RunTaskConfiguration {
    fn from(value: RunFunction) -> Self {
        Self::Function(value)
    }
}
impl ::std::convert::From<RunScript> for RunTaskConfiguration {
    fn from(value: RunScript) -> Self {
        Self::Script(value)
    }
}
#[doc = "Execute using a worker configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RunWorker\","]
#[doc = "  \"description\": \"Execute using a worker configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"worker\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"worker\": {"]
#[doc = "      \"title\": \"RunJobWorker\","]
#[doc = "      \"description\": \"Executes a job using a specified worker (configured runner with settings and options).\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"arguments\","]
#[doc = "        \"name\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"arguments\": {"]
#[doc = "          \"title\": \"FunctionArguments\","]
#[doc = "          \"description\": \"A key/value mapping of arguments to use when running the function. Runtime expressions are supported for value transformation.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": true"]
#[doc = "        },"]
#[doc = "        \"name\": {"]
#[doc = "          \"title\": \"WorkerName\","]
#[doc = "          \"description\": \"The name of the worker that executes this job (user-defined).\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"using\": {"]
#[doc = "          \"title\": \"Using\","]
#[doc = "          \"description\": \"Selects which method to use for multi-method runners.\\n\\nFor single-method runners, this field is optional (auto-selected if omitted).\\n\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"additionalProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunWorker {
    pub worker: RunJobWorker,
}
impl ::std::convert::From<&RunWorker> for RunWorker {
    fn from(value: &RunWorker) -> Self {
        value.clone()
    }
}
impl RunWorker {
    pub fn builder() -> builder::RunWorker {
        Default::default()
    }
}
#[doc = "Schema definition configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Schema\","]
#[doc = "  \"description\": \"Schema definition configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"SchemaInline\","]
#[doc = "      \"required\": ["]
#[doc = "        \"document\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"document\": {"]
#[doc = "          \"description\": \"Inline schema definition.\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"SchemaExternal\","]
#[doc = "      \"required\": ["]
#[doc = "        \"resource\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"resource\": {"]
#[doc = "          \"title\": \"SchemaExternalResource\","]
#[doc = "          \"description\": \"External schema resource.\","]
#[doc = "          \"$ref\": \"#/$defs/externalResource\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"format\": {"]
#[doc = "      \"title\": \"SchemaFormat\","]
#[doc = "      \"description\": \"Schema format (defaults to 'json'). Use `{format}:{version}` for versioning.\","]
#[doc = "      \"default\": \"json\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Schema {
    Variant0 {
        #[doc = "Inline schema definition."]
        document: ::serde_json::Value,
        #[doc = "Schema format (defaults to 'json'). Use `{format}:{version}` for versioning."]
        #[serde(default = "defaults::schema_variant0_format")]
        format: ::std::string::String,
    },
    Variant1 {
        #[doc = "Schema format (defaults to 'json'). Use `{format}:{version}` for versioning."]
        #[serde(default = "defaults::schema_variant1_format")]
        format: ::std::string::String,
        #[doc = "External schema resource."]
        resource: ExternalResource,
    },
}
impl ::std::convert::From<&Self> for Schema {
    fn from(value: &Schema) -> Self {
        value.clone()
    }
}
#[doc = "Script execution configuration"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ScriptConfiguration\","]
#[doc = "  \"description\": \"Script execution configuration\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"required\": ["]
#[doc = "        \"code\""]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"required\": ["]
#[doc = "        \"source\""]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"required\": ["]
#[doc = "    \"language\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"arguments\": {"]
#[doc = "      \"title\": \"ScriptArguments\","]
#[doc = "      \"description\": \"Arguments passed to the script. Runtime expressions are supported for value transformation. Each key becomes a variable in the script.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"code\": {"]
#[doc = "      \"title\": \"InlineCode\","]
#[doc = "      \"description\": \"Inline script code to execute\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"environment\": {"]
#[doc = "      \"title\": \"EnvironmentVariables\","]
#[doc = "      \"description\": \"Environment variables for script execution\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": {"]
#[doc = "        \"type\": \"string\""]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"language\": {"]
#[doc = "      \"title\": \"ScriptLanguage\","]
#[doc = "      \"description\": \"Script language identifier (e.g., 'python', 'javascript'). Validated at runtime.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"source\": {"]
#[doc = "      \"title\": \"ExternalSource\","]
#[doc = "      \"description\": \"External script resource reference\","]
#[doc = "      \"$ref\": \"#/$defs/externalResource\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": true"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum ScriptConfiguration {
    Variant0 {
        #[doc = "Arguments passed to the script. Runtime expressions are supported for value transformation. Each key becomes a variable in the script."]
        #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
        arguments: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
        #[doc = "Inline script code to execute"]
        code: ::std::string::String,
        #[doc = "Environment variables for script execution"]
        #[serde(
            default,
            skip_serializing_if = ":: std :: collections :: HashMap::is_empty"
        )]
        environment: ::std::collections::HashMap<::std::string::String, ::std::string::String>,
        #[doc = "Script language identifier (e.g., 'python', 'javascript'). Validated at runtime."]
        language: ::std::string::String,
    },
    Variant1 {
        #[doc = "Arguments passed to the script. Runtime expressions are supported for value transformation. Each key becomes a variable in the script."]
        #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
        arguments: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
        #[doc = "Environment variables for script execution"]
        #[serde(
            default,
            skip_serializing_if = ":: std :: collections :: HashMap::is_empty"
        )]
        environment: ::std::collections::HashMap<::std::string::String, ::std::string::String>,
        #[doc = "Script language identifier (e.g., 'python', 'javascript'). Validated at runtime."]
        language: ::std::string::String,
        #[doc = "External script resource reference"]
        source: ExternalResource,
    },
}
impl ::std::convert::From<&Self> for ScriptConfiguration {
    fn from(value: &ScriptConfiguration) -> Self {
        value.clone()
    }
}
#[doc = "SetTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"set\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"set\": {"]
#[doc = "      \"title\": \"SetTaskConfiguration\","]
#[doc = "      \"description\": \"Data to set as context variables.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"minProperties\": 1,"]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct SetTask {
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "Data to set as context variables."]
    pub set: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
}
impl ::std::convert::From<&SetTask> for SetTask {
    fn from(value: &SetTask) -> Self {
        value.clone()
    }
}
impl SetTask {
    pub fn builder() -> builder::SetTask {
        Default::default()
    }
}
#[doc = "Case condition and action definition."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"SwitchCase\","]
#[doc = "  \"description\": \"Case condition and action definition.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"then\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"SwitchCaseOutcome\","]
#[doc = "      \"description\": \"Flow directive for matching case.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"when\": {"]
#[doc = "      \"title\": \"SwitchCaseCondition\","]
#[doc = "      \"description\": \"Runtime expression for case matching.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct SwitchCase {
    #[doc = "Flow directive for matching case."]
    pub then: FlowDirective,
    #[doc = "Runtime expression for case matching."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub when: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&SwitchCase> for SwitchCase {
    fn from(value: &SwitchCase) -> Self {
        value.clone()
    }
}
impl SwitchCase {
    pub fn builder() -> builder::SwitchCase {
        Default::default()
    }
}
#[doc = "SwitchTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"switch\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"switch\": {"]
#[doc = "      \"title\": \"SwitchTaskConfiguration\","]
#[doc = "      \"description\": \"Switch case definitions.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"title\": \"SwitchItem\","]
#[doc = "        \"type\": \"object\","]
#[doc = "        \"maxProperties\": 1,"]
#[doc = "        \"minProperties\": 1,"]
#[doc = "        \"additionalProperties\": {"]
#[doc = "          \"title\": \"SwitchCase\","]
#[doc = "          \"description\": \"Case condition and action definition.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"required\": ["]
#[doc = "            \"then\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"then\": {"]
#[doc = "              \"title\": \"SwitchCaseOutcome\","]
#[doc = "              \"description\": \"Flow directive for matching case.\","]
#[doc = "              \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "            },"]
#[doc = "            \"when\": {"]
#[doc = "              \"title\": \"SwitchCaseCondition\","]
#[doc = "              \"description\": \"Runtime expression for case matching.\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            }"]
#[doc = "          },"]
#[doc = "          \"unevaluatedProperties\": false"]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"minItems\": 1"]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct SwitchTask {
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "Switch case definitions."]
    pub switch: ::std::vec::Vec<::std::collections::HashMap<::std::string::String, SwitchCase>>,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
}
impl ::std::convert::From<&SwitchTask> for SwitchTask {
    fn from(value: &SwitchTask) -> Self {
        value.clone()
    }
}
impl SwitchTask {
    pub fn builder() -> builder::SwitchTask {
        Default::default()
    }
}
#[doc = "Single unit of work within a workflow."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Task\","]
#[doc = "  \"description\": \"Single unit of work within a workflow.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/forkTask\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/forTask\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/raiseTask\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/runTask\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/setTask\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/switchTask\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/tryTask\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/doTask\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/waitTask\""]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Task {
    ForkTask(ForkTask),
    ForTask(ForTask),
    RaiseTask(RaiseTask),
    RunTask(RunTask),
    SetTask(SetTask),
    SwitchTask(SwitchTask),
    TryTask(TryTask),
    DoTask(DoTask),
    WaitTask(WaitTask),
}
impl ::std::convert::From<&Self> for Task {
    fn from(value: &Task) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<ForkTask> for Task {
    fn from(value: ForkTask) -> Self {
        Self::ForkTask(value)
    }
}
impl ::std::convert::From<ForTask> for Task {
    fn from(value: ForTask) -> Self {
        Self::ForTask(value)
    }
}
impl ::std::convert::From<RaiseTask> for Task {
    fn from(value: RaiseTask) -> Self {
        Self::RaiseTask(value)
    }
}
impl ::std::convert::From<RunTask> for Task {
    fn from(value: RunTask) -> Self {
        Self::RunTask(value)
    }
}
impl ::std::convert::From<SetTask> for Task {
    fn from(value: SetTask) -> Self {
        Self::SetTask(value)
    }
}
impl ::std::convert::From<SwitchTask> for Task {
    fn from(value: SwitchTask) -> Self {
        Self::SwitchTask(value)
    }
}
impl ::std::convert::From<TryTask> for Task {
    fn from(value: TryTask) -> Self {
        Self::TryTask(value)
    }
}
impl ::std::convert::From<DoTask> for Task {
    fn from(value: DoTask) -> Self {
        Self::DoTask(value)
    }
}
impl ::std::convert::From<WaitTask> for Task {
    fn from(value: WaitTask) -> Self {
        Self::WaitTask(value)
    }
}
#[doc = "Base properties inherited by all task types."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"TaskBase\","]
#[doc = "  \"description\": \"Base properties inherited by all task types.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct TaskBase {
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
}
impl ::std::convert::From<&TaskBase> for TaskBase {
    fn from(value: &TaskBase) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for TaskBase {
    fn default() -> Self {
        Self {
            checkpoint: Default::default(),
            export: Default::default(),
            if_: Default::default(),
            input: Default::default(),
            metadata: Default::default(),
            output: Default::default(),
            then: Default::default(),
            timeout: Default::default(),
        }
    }
}
impl TaskBase {
    pub fn builder() -> builder::TaskBase {
        Default::default()
    }
}
#[doc = "Ordered list of named tasks."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"TaskList\","]
#[doc = "  \"description\": \"Ordered list of named tasks.\","]
#[doc = "  \"type\": \"array\","]
#[doc = "  \"items\": {"]
#[doc = "    \"title\": \"TaskItem\","]
#[doc = "    \"type\": \"object\","]
#[doc = "    \"maxProperties\": 1,"]
#[doc = "    \"minProperties\": 1,"]
#[doc = "    \"additionalProperties\": {"]
#[doc = "      \"$ref\": \"#/$defs/task\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(transparent)]
pub struct TaskList(pub ::std::vec::Vec<::std::collections::HashMap<::std::string::String, Task>>);
impl ::std::ops::Deref for TaskList {
    type Target = ::std::vec::Vec<::std::collections::HashMap<::std::string::String, Task>>;
    fn deref(&self) -> &::std::vec::Vec<::std::collections::HashMap<::std::string::String, Task>> {
        &self.0
    }
}
impl ::std::convert::From<TaskList>
    for ::std::vec::Vec<::std::collections::HashMap<::std::string::String, Task>>
{
    fn from(value: TaskList) -> Self {
        value.0
    }
}
impl ::std::convert::From<&TaskList> for TaskList {
    fn from(value: &TaskList) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<::std::vec::Vec<::std::collections::HashMap<::std::string::String, Task>>>
    for TaskList
{
    fn from(
        value: ::std::vec::Vec<::std::collections::HashMap<::std::string::String, Task>>,
    ) -> Self {
        Self(value)
    }
}
#[doc = "TaskTimeout"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"TaskTimeout\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"TaskTimeoutDefinition\","]
#[doc = "      \"description\": \"Task timeout configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/timeout\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"TaskTimeoutReference\","]
#[doc = "      \"description\": \"Reference to named timeout configuration.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum TaskTimeout {
    Timeout(Timeout),
    TaskTimeoutReference(::std::string::String),
}
impl ::std::convert::From<&Self> for TaskTimeout {
    fn from(value: &TaskTimeout) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<Timeout> for TaskTimeout {
    fn from(value: Timeout) -> Self {
        Self::Timeout(value)
    }
}
#[doc = "Timeout configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Timeout\","]
#[doc = "  \"description\": \"Timeout configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"after\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"after\": {"]
#[doc = "      \"title\": \"TimeoutAfter\","]
#[doc = "      \"description\": \"Timeout duration.\","]
#[doc = "      \"$ref\": \"#/$defs/duration\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct Timeout {
    #[doc = "Timeout duration."]
    pub after: Duration,
}
impl ::std::convert::From<&Timeout> for Timeout {
    fn from(value: &Timeout) -> Self {
        value.clone()
    }
}
impl Timeout {
    pub fn builder() -> builder::Timeout {
        Default::default()
    }
}
#[doc = "TryTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"catch\","]
#[doc = "    \"try\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"catch\": {"]
#[doc = "      \"title\": \"TryTaskCatch\","]
#[doc = "      \"description\": \"Error handling configuration.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"properties\": {"]
#[doc = "        \"as\": {"]
#[doc = "          \"title\": \"CatchAs\","]
#[doc = "          \"description\": \"Variable name to store caught error (defaults to 'error').\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"do\": {"]
#[doc = "          \"title\": \"TryTaskCatchDo\","]
#[doc = "          \"description\": \"Tasks to execute when error is caught.\","]
#[doc = "          \"$ref\": \"#/$defs/taskList\""]
#[doc = "        },"]
#[doc = "        \"errors\": {"]
#[doc = "          \"title\": \"CatchErrors\","]
#[doc = "          \"description\": \"Static error filter configuration.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"properties\": {"]
#[doc = "            \"with\": {"]
#[doc = "              \"$ref\": \"#/$defs/errorFilter\""]
#[doc = "            }"]
#[doc = "          }"]
#[doc = "        },"]
#[doc = "        \"exceptWhen\": {"]
#[doc = "          \"title\": \"CatchExceptWhen\","]
#[doc = "          \"description\": \"Runtime expression to disable error catching.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"retry\": {"]
#[doc = "          \"oneOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"RetryPolicyDefinition\","]
#[doc = "              \"description\": \"Retry policy configuration.\","]
#[doc = "              \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"RetryPolicyReference\","]
#[doc = "              \"description\": \"Reference to named retry policy.\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        \"when\": {"]
#[doc = "          \"title\": \"CatchWhen\","]
#[doc = "          \"description\": \"Runtime expression to enable error catching.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"try\": {"]
#[doc = "      \"title\": \"TryTaskConfiguration\","]
#[doc = "      \"description\": \"Tasks to attempt execution.\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct TryTask {
    pub catch: TryTaskCatch,
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
    #[doc = "Tasks to attempt execution."]
    #[serde(rename = "try")]
    pub try_: TaskList,
}
impl ::std::convert::From<&TryTask> for TryTask {
    fn from(value: &TryTask) -> Self {
        value.clone()
    }
}
impl TryTask {
    pub fn builder() -> builder::TryTask {
        Default::default()
    }
}
#[doc = "Error handling configuration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"TryTaskCatch\","]
#[doc = "  \"description\": \"Error handling configuration.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"as\": {"]
#[doc = "      \"title\": \"CatchAs\","]
#[doc = "      \"description\": \"Variable name to store caught error (defaults to 'error').\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"do\": {"]
#[doc = "      \"title\": \"TryTaskCatchDo\","]
#[doc = "      \"description\": \"Tasks to execute when error is caught.\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    },"]
#[doc = "    \"errors\": {"]
#[doc = "      \"title\": \"CatchErrors\","]
#[doc = "      \"description\": \"Static error filter configuration.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"properties\": {"]
#[doc = "        \"with\": {"]
#[doc = "          \"$ref\": \"#/$defs/errorFilter\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"exceptWhen\": {"]
#[doc = "      \"title\": \"CatchExceptWhen\","]
#[doc = "      \"description\": \"Runtime expression to disable error catching.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"retry\": {"]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"RetryPolicyDefinition\","]
#[doc = "          \"description\": \"Retry policy configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"RetryPolicyReference\","]
#[doc = "          \"description\": \"Reference to named retry policy.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"when\": {"]
#[doc = "      \"title\": \"CatchWhen\","]
#[doc = "      \"description\": \"Runtime expression to enable error catching.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct TryTaskCatch {
    #[doc = "Variable name to store caught error (defaults to 'error')."]
    #[serde(
        rename = "as",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub as_: ::std::option::Option<::std::string::String>,
    #[doc = "Tasks to execute when error is caught."]
    #[serde(
        rename = "do",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub do_: ::std::option::Option<TaskList>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub errors: ::std::option::Option<CatchErrors>,
    #[doc = "Runtime expression to disable error catching."]
    #[serde(
        rename = "exceptWhen",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub except_when: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub retry: ::std::option::Option<TryTaskCatchRetry>,
    #[doc = "Runtime expression to enable error catching."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub when: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&TryTaskCatch> for TryTaskCatch {
    fn from(value: &TryTaskCatch) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for TryTaskCatch {
    fn default() -> Self {
        Self {
            as_: Default::default(),
            do_: Default::default(),
            errors: Default::default(),
            except_when: Default::default(),
            retry: Default::default(),
            when: Default::default(),
        }
    }
}
impl TryTaskCatch {
    pub fn builder() -> builder::TryTaskCatch {
        Default::default()
    }
}
#[doc = "TryTaskCatchRetry"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"RetryPolicyDefinition\","]
#[doc = "      \"description\": \"Retry policy configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"RetryPolicyReference\","]
#[doc = "      \"description\": \"Reference to named retry policy.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum TryTaskCatchRetry {
    Variant0(RetryPolicy),
    Variant1(::std::string::String),
}
impl ::std::convert::From<&Self> for TryTaskCatchRetry {
    fn from(value: &TryTaskCatchRetry) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<RetryPolicy> for TryTaskCatchRetry {
    fn from(value: RetryPolicy) -> Self {
        Self::Variant0(value)
    }
}
#[doc = "UriTemplate"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"UriTemplate\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"LiteralUriTemplate\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"format\": \"uri-template\","]
#[doc = "      \"pattern\": \"^[A-Za-z][A-Za-z0-9+\\\\-.]*://.*\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(
    :: serde :: Deserialize,
    :: serde :: Serialize,
    Clone,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
#[serde(transparent)]
pub struct UriTemplate(pub ::std::string::String);
impl ::std::ops::Deref for UriTemplate {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<UriTemplate> for ::std::string::String {
    fn from(value: UriTemplate) -> Self {
        value.0
    }
}
impl ::std::convert::From<&UriTemplate> for UriTemplate {
    fn from(value: &UriTemplate) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<::std::string::String> for UriTemplate {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for UriTemplate {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for UriTemplate {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "WaitTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"wait\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpoint\": {"]
#[doc = "      \"title\": \"Checkpoint\","]
#[doc = "      \"description\": \"Save workflow state after this task for checkpoint/restart.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"Runtime expression to conditionally execute this task.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Task input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Additional task metadata.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Task output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"Flow control directive executed after task completion.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"Task timeout configuration.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"Reference to named timeout configuration.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"wait\": {"]
#[doc = "      \"title\": \"WaitTaskConfiguration\","]
#[doc = "      \"description\": \"Duration to wait.\","]
#[doc = "      \"$ref\": \"#/$defs/duration\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct WaitTask {
    #[doc = "Save workflow state after this task for checkpoint/restart."]
    #[serde(default)]
    pub checkpoint: bool,
    #[doc = "Export task output to workflow context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "Runtime expression to conditionally execute this task."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Task input configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Additional task metadata."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Task output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "Flow control directive executed after task completion."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
    #[doc = "Duration to wait."]
    pub wait: Duration,
}
impl ::std::convert::From<&WaitTask> for WaitTask {
    fn from(value: &WaitTask) -> Self {
        value.clone()
    }
}
impl WaitTask {
    pub fn builder() -> builder::WaitTask {
        Default::default()
    }
}
#[doc = "The options to use when running the worker."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"WorkerOptions\","]
#[doc = "  \"description\": \"The options to use when running the worker.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"broadcastResults\": {"]
#[doc = "      \"title\": \"BroadcastResultsToListener\","]
#[doc = "      \"description\": \"Whether to broadcast results to listeners.\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"channel\": {"]
#[doc = "      \"title\": \"Channel\","]
#[doc = "      \"description\": \"The channel to use when running the worker (controls execution concurrency).\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"queueType\": {"]
#[doc = "      \"title\": \"QueueType\","]
#[doc = "      \"description\": \"Defines how jobs are queued and persisted. Values: NORMAL (default, in-memory only), WITH_BACKUP (in-memory with database backup), DB_ONLY (database only).\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"enum\": ["]
#[doc = "        \"NORMAL\","]
#[doc = "        \"WITH_BACKUP\","]
#[doc = "        \"DB_ONLY\""]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"responseType\": {"]
#[doc = "      \"title\": \"ResponseType\","]
#[doc = "      \"description\": \"Defines how job results should be returned to the client. Values: NO_RESULT (async, no wait), DIRECT (default, synchronous with result).\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"enum\": ["]
#[doc = "        \"NO_RESULT\","]
#[doc = "        \"DIRECT\""]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"retry\": {"]
#[doc = "      \"title\": \"RetryPolicyDefinition\","]
#[doc = "      \"description\": \"The retry policy to use, if any, when catching errors.\","]
#[doc = "      \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "    },"]
#[doc = "    \"storeFailure\": {"]
#[doc = "      \"title\": \"StoreFailureResult\","]
#[doc = "      \"description\": \"Whether to store failure results in the database.\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"storeSuccess\": {"]
#[doc = "      \"title\": \"StoreSuccessResult\","]
#[doc = "      \"description\": \"Whether to store successful results in the database.\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"useStatic\": {"]
#[doc = "      \"title\": \"UseStaticWorker\","]
#[doc = "      \"description\": \"Whether to use a static worker (persisted in database with pooled initialization).\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct WorkerOptions {
    #[doc = "Whether to broadcast results to listeners."]
    #[serde(
        rename = "broadcastResults",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub broadcast_results: ::std::option::Option<bool>,
    #[doc = "The channel to use when running the worker (controls execution concurrency)."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub channel: ::std::option::Option<::std::string::String>,
    #[doc = "Defines how jobs are queued and persisted. Values: NORMAL (default, in-memory only), WITH_BACKUP (in-memory with database backup), DB_ONLY (database only)."]
    #[serde(
        rename = "queueType",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub queue_type: ::std::option::Option<QueueType>,
    #[doc = "Defines how job results should be returned to the client. Values: NO_RESULT (async, no wait), DIRECT (default, synchronous with result)."]
    #[serde(
        rename = "responseType",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub response_type: ::std::option::Option<ResponseType>,
    #[doc = "The retry policy to use, if any, when catching errors."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub retry: ::std::option::Option<RetryPolicy>,
    #[doc = "Whether to store failure results in the database."]
    #[serde(
        rename = "storeFailure",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub store_failure: ::std::option::Option<bool>,
    #[doc = "Whether to store successful results in the database."]
    #[serde(
        rename = "storeSuccess",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub store_success: ::std::option::Option<bool>,
    #[doc = "Whether to use a static worker (persisted in database with pooled initialization)."]
    #[serde(
        rename = "useStatic",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub use_static: ::std::option::Option<bool>,
}
impl ::std::convert::From<&WorkerOptions> for WorkerOptions {
    fn from(value: &WorkerOptions) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for WorkerOptions {
    fn default() -> Self {
        Self {
            broadcast_results: Default::default(),
            channel: Default::default(),
            queue_type: Default::default(),
            response_type: Default::default(),
            retry: Default::default(),
            store_failure: Default::default(),
            store_success: Default::default(),
            use_static: Default::default(),
        }
    }
}
impl WorkerOptions {
    pub fn builder() -> builder::WorkerOptions {
        Default::default()
    }
}
#[doc = "DSL version used by this workflow."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"WorkflowDSL\","]
#[doc = "  \"description\": \"DSL version used by this workflow.\","]
#[doc = "  \"default\": \"0.0.1\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Serialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct WorkflowDsl(::std::string::String);
impl ::std::ops::Deref for WorkflowDsl {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<WorkflowDsl> for ::std::string::String {
    fn from(value: WorkflowDsl) -> Self {
        value.0
    }
}
impl ::std::convert::From<&WorkflowDsl> for WorkflowDsl {
    fn from(value: &WorkflowDsl) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for WorkflowDsl {
    fn default() -> Self {
        WorkflowDsl("0.0.1".to_string())
    }
}
impl ::std::str::FromStr for WorkflowDsl {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        if regress :: Regex :: new ("^(0|[1-9]\\d*)\\.(0|[1-9]\\d*)\\.(0|[1-9]\\d*)(?:-((?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\.(?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\+([0-9a-zA-Z-]+(?:\\.[0-9a-zA-Z-]+)*))?$") . unwrap () . find (value) . is_none () { return Err ("doesn't match pattern \"^(0|[1-9]\\d*)\\.(0|[1-9]\\d*)\\.(0|[1-9]\\d*)(?:-((?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\.(?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\+([0-9a-zA-Z-]+(?:\\.[0-9a-zA-Z-]+)*))?$\"" . into ()) ; }
        Ok(Self(value.to_string()))
    }
}
impl ::std::convert::TryFrom<&str> for WorkflowDsl {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for WorkflowDsl {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for WorkflowDsl {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl<'de> ::serde::Deserialize<'de> for WorkflowDsl {
    fn deserialize<D>(deserializer: D) -> ::std::result::Result<Self, D::Error>
    where
        D: ::serde::Deserializer<'de>,
    {
        ::std::string::String::deserialize(deserializer)?
            .parse()
            .map_err(|e: self::error::ConversionError| {
                <D::Error as ::serde::de::Error>::custom(e.to_string())
            })
    }
}
#[doc = "Workflow name."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"WorkflowName\","]
#[doc = "  \"description\": \"Workflow name.\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Serialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct WorkflowName(::std::string::String);
impl ::std::ops::Deref for WorkflowName {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<WorkflowName> for ::std::string::String {
    fn from(value: WorkflowName) -> Self {
        value.0
    }
}
impl ::std::convert::From<&WorkflowName> for WorkflowName {
    fn from(value: &WorkflowName) -> Self {
        value.clone()
    }
}
impl ::std::str::FromStr for WorkflowName {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        if regress::Regex::new("^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$")
            .unwrap()
            .find(value)
            .is_none()
        {
            return Err(
                "doesn't match pattern \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\"".into(),
            );
        }
        Ok(Self(value.to_string()))
    }
}
impl ::std::convert::TryFrom<&str> for WorkflowName {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for WorkflowName {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for WorkflowName {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl<'de> ::serde::Deserialize<'de> for WorkflowName {
    fn deserialize<D>(deserializer: D) -> ::std::result::Result<Self, D::Error>
    where
        D: ::serde::Deserializer<'de>,
    {
        ::std::string::String::deserialize(deserializer)?
            .parse()
            .map_err(|e: self::error::ConversionError| {
                <D::Error as ::serde::de::Error>::custom(e.to_string())
            })
    }
}
#[doc = "Workflow namespace."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"WorkflowNamespace\","]
#[doc = "  \"description\": \"Workflow namespace.\","]
#[doc = "  \"default\": \"default\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Serialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct WorkflowNamespace(::std::string::String);
impl ::std::ops::Deref for WorkflowNamespace {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<WorkflowNamespace> for ::std::string::String {
    fn from(value: WorkflowNamespace) -> Self {
        value.0
    }
}
impl ::std::convert::From<&WorkflowNamespace> for WorkflowNamespace {
    fn from(value: &WorkflowNamespace) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for WorkflowNamespace {
    fn default() -> Self {
        WorkflowNamespace("default".to_string())
    }
}
impl ::std::str::FromStr for WorkflowNamespace {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        if regress::Regex::new("^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$")
            .unwrap()
            .find(value)
            .is_none()
        {
            return Err(
                "doesn't match pattern \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\"".into(),
            );
        }
        Ok(Self(value.to_string()))
    }
}
impl ::std::convert::TryFrom<&str> for WorkflowNamespace {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for WorkflowNamespace {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for WorkflowNamespace {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl<'de> ::serde::Deserialize<'de> for WorkflowNamespace {
    fn deserialize<D>(deserializer: D) -> ::std::result::Result<Self, D::Error>
    where
        D: ::serde::Deserializer<'de>,
    {
        ::std::string::String::deserialize(deserializer)?
            .parse()
            .map_err(|e: self::error::ConversionError| {
                <D::Error as ::serde::de::Error>::custom(e.to_string())
            })
    }
}
#[doc = "Workflow schema supporting job execution with functions and tools.\nRuntime expressions are supported in fields marked in descriptions: - jq syntax: ${.key.subkey} for data access, ${$task.input} for context - liquid syntax: $${..} for templates\nAvailable context variables: - Task input data: direct key access via ${.key} (only within current task context) - Task output data: direct key access via ${.key} (only within current task context) - Context vars: set by task.export, setTask (access via $variable_name for jq, {{ variable_name }} for liquid) - Workflow: access via $workflow (e.g., $workflow.input.key, $workflow.id, $workflow.definition, $workflow.context_variables) - Task: access via $task (e.g., $task.definition, $task.input, $task.raw_output, $task.output, $task.flow_directive)"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"$id\": \"https://serverlessworkflow.io/schemas/1.0.0/workflow.yaml\","]
#[doc = "  \"title\": \"WorkflowSchema\","]
#[doc = "  \"description\": \"Workflow schema supporting job execution with functions and tools.\\nRuntime expressions are supported in fields marked in descriptions: - jq syntax: ${.key.subkey} for data access, ${$task.input} for context - liquid syntax: $${..} for templates\\nAvailable context variables: - Task input data: direct key access via ${.key} (only within current task context) - Task output data: direct key access via ${.key} (only within current task context) - Context vars: set by task.export, setTask (access via $variable_name for jq, {{ variable_name }} for liquid) - Workflow: access via $workflow (e.g., $workflow.input.key, $workflow.id, $workflow.definition, $workflow.context_variables) - Task: access via $task (e.g., $task.definition, $task.input, $task.raw_output, $task.output, $task.flow_directive)\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"do\","]
#[doc = "    \"document\","]
#[doc = "    \"input\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"checkpointing\": {"]
#[doc = "      \"title\": \"Checkpoint Config\","]
#[doc = "      \"description\": \"Checkpoint and restart configuration.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"enabled\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"enabled\": {"]
#[doc = "          \"description\": \"Enable checkpoint functionality.\","]
#[doc = "          \"default\": false,"]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        },"]
#[doc = "        \"storage\": {"]
#[doc = "          \"description\": \"Storage backend for checkpoints.\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"enum\": ["]
#[doc = "            \"memory\","]
#[doc = "            \"redis\""]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"do\": {"]
#[doc = "      \"title\": \"Do\","]
#[doc = "      \"description\": \"Tasks to execute in this workflow.\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    },"]
#[doc = "    \"document\": {"]
#[doc = "      \"title\": \"Document\","]
#[doc = "      \"description\": \"Workflow metadata and identification.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"dsl\","]
#[doc = "        \"name\","]
#[doc = "        \"namespace\","]
#[doc = "        \"version\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"dsl\": {"]
#[doc = "          \"title\": \"WorkflowDSL\","]
#[doc = "          \"description\": \"DSL version used by this workflow.\","]
#[doc = "          \"default\": \"0.0.1\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "        },"]
#[doc = "        \"metadata\": {"]
#[doc = "          \"title\": \"WorkflowMetadata\","]
#[doc = "          \"description\": \"Additional workflow metadata.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": true"]
#[doc = "        },"]
#[doc = "        \"name\": {"]
#[doc = "          \"title\": \"WorkflowName\","]
#[doc = "          \"description\": \"Workflow name.\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "        },"]
#[doc = "        \"namespace\": {"]
#[doc = "          \"title\": \"WorkflowNamespace\","]
#[doc = "          \"description\": \"Workflow namespace.\","]
#[doc = "          \"default\": \"default\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "        },"]
#[doc = "        \"summary\": {"]
#[doc = "          \"title\": \"WorkflowSummary\","]
#[doc = "          \"description\": \"Workflow summary in Markdown format.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"tags\": {"]
#[doc = "          \"title\": \"WorkflowTags\","]
#[doc = "          \"description\": \"Key/value tags for workflow classification.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": true"]
#[doc = "        },"]
#[doc = "        \"title\": {"]
#[doc = "          \"title\": \"WorkflowTitle\","]
#[doc = "          \"description\": \"Workflow title.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"version\": {"]
#[doc = "          \"title\": \"WorkflowVersion\","]
#[doc = "          \"description\": \"Workflow semantic version.\","]
#[doc = "          \"default\": \"0.0.1\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"Input\","]
#[doc = "      \"description\": \"Workflow input configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"Output\","]
#[doc = "      \"description\": \"Workflow output configuration.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct WorkflowSchema {
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub checkpointing: ::std::option::Option<CheckpointConfig>,
    #[doc = "Tasks to execute in this workflow."]
    #[serde(rename = "do")]
    pub do_: TaskList,
    pub document: Document,
    #[doc = "Workflow input configuration."]
    pub input: Input,
    #[doc = "Workflow output configuration."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
}
impl ::std::convert::From<&WorkflowSchema> for WorkflowSchema {
    fn from(value: &WorkflowSchema) -> Self {
        value.clone()
    }
}
impl WorkflowSchema {
    pub fn builder() -> builder::WorkflowSchema {
        Default::default()
    }
}
#[doc = "Workflow semantic version."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"WorkflowVersion\","]
#[doc = "  \"description\": \"Workflow semantic version.\","]
#[doc = "  \"default\": \"0.0.1\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Serialize, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[serde(transparent)]
pub struct WorkflowVersion(::std::string::String);
impl ::std::ops::Deref for WorkflowVersion {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<WorkflowVersion> for ::std::string::String {
    fn from(value: WorkflowVersion) -> Self {
        value.0
    }
}
impl ::std::convert::From<&WorkflowVersion> for WorkflowVersion {
    fn from(value: &WorkflowVersion) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for WorkflowVersion {
    fn default() -> Self {
        WorkflowVersion("0.0.1".to_string())
    }
}
impl ::std::str::FromStr for WorkflowVersion {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        if regress :: Regex :: new ("^(0|[1-9]\\d*)\\.(0|[1-9]\\d*)\\.(0|[1-9]\\d*)(?:-((?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\.(?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\+([0-9a-zA-Z-]+(?:\\.[0-9a-zA-Z-]+)*))?$") . unwrap () . find (value) . is_none () { return Err ("doesn't match pattern \"^(0|[1-9]\\d*)\\.(0|[1-9]\\d*)\\.(0|[1-9]\\d*)(?:-((?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\.(?:0|[1-9]\\d*|\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\+([0-9a-zA-Z-]+(?:\\.[0-9a-zA-Z-]+)*))?$\"" . into ()) ; }
        Ok(Self(value.to_string()))
    }
}
impl ::std::convert::TryFrom<&str> for WorkflowVersion {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for WorkflowVersion {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for WorkflowVersion {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl<'de> ::serde::Deserialize<'de> for WorkflowVersion {
    fn deserialize<D>(deserializer: D) -> ::std::result::Result<Self, D::Error>
    where
        D: ::serde::Deserializer<'de>,
    {
        ::std::string::String::deserialize(deserializer)?
            .parse()
            .map_err(|e: self::error::ConversionError| {
                <D::Error as ::serde::de::Error>::custom(e.to_string())
            })
    }
}
#[doc = r" Types for composing complex structures."]
pub mod builder {
    #[derive(Clone, Debug)]
    pub struct CatchErrors {
        with:
            ::std::result::Result<::std::option::Option<super::ErrorFilter>, ::std::string::String>,
    }
    impl ::std::default::Default for CatchErrors {
        fn default() -> Self {
            Self {
                with: Ok(Default::default()),
            }
        }
    }
    impl CatchErrors {
        pub fn with<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ErrorFilter>>,
            T::Error: ::std::fmt::Display,
        {
            self.with = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for with: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<CatchErrors> for super::CatchErrors {
        type Error = super::error::ConversionError;
        fn try_from(
            value: CatchErrors,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self { with: value.with? })
        }
    }
    impl ::std::convert::From<super::CatchErrors> for CatchErrors {
        fn from(value: super::CatchErrors) -> Self {
            Self {
                with: Ok(value.with),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct CheckpointConfig {
        enabled: ::std::result::Result<bool, ::std::string::String>,
        storage: ::std::result::Result<
            ::std::option::Option<super::CheckpointConfigStorage>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for CheckpointConfig {
        fn default() -> Self {
            Self {
                enabled: Err("no value supplied for enabled".to_string()),
                storage: Ok(Default::default()),
            }
        }
    }
    impl CheckpointConfig {
        pub fn enabled<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.enabled = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for enabled: {}", e));
            self
        }
        pub fn storage<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CheckpointConfigStorage>>,
            T::Error: ::std::fmt::Display,
        {
            self.storage = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for storage: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<CheckpointConfig> for super::CheckpointConfig {
        type Error = super::error::ConversionError;
        fn try_from(
            value: CheckpointConfig,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                enabled: value.enabled?,
                storage: value.storage?,
            })
        }
    }
    impl ::std::convert::From<super::CheckpointConfig> for CheckpointConfig {
        fn from(value: super::CheckpointConfig) -> Self {
            Self {
                enabled: Ok(value.enabled),
                storage: Ok(value.storage),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct DoTask {
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        do_: ::std::result::Result<super::TaskList, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
    }
    impl ::std::default::Default for DoTask {
        fn default() -> Self {
            Self {
                checkpoint: Ok(Default::default()),
                do_: Err("no value supplied for do_".to_string()),
                export: Ok(Default::default()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
            }
        }
    }
    impl DoTask {
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn do_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TaskList>,
            T::Error: ::std::fmt::Display,
        {
            self.do_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for do_: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<DoTask> for super::DoTask {
        type Error = super::error::ConversionError;
        fn try_from(value: DoTask) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpoint: value.checkpoint?,
                do_: value.do_?,
                export: value.export?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                then: value.then?,
                timeout: value.timeout?,
            })
        }
    }
    impl ::std::convert::From<super::DoTask> for DoTask {
        fn from(value: super::DoTask) -> Self {
            Self {
                checkpoint: Ok(value.checkpoint),
                do_: Ok(value.do_),
                export: Ok(value.export),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Document {
        dsl: ::std::result::Result<super::WorkflowDsl, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        name: ::std::result::Result<super::WorkflowName, ::std::string::String>,
        namespace: ::std::result::Result<super::WorkflowNamespace, ::std::string::String>,
        summary: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        tags: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        title: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        version: ::std::result::Result<super::WorkflowVersion, ::std::string::String>,
    }
    impl ::std::default::Default for Document {
        fn default() -> Self {
            Self {
                dsl: Err("no value supplied for dsl".to_string()),
                metadata: Ok(Default::default()),
                name: Err("no value supplied for name".to_string()),
                namespace: Err("no value supplied for namespace".to_string()),
                summary: Ok(Default::default()),
                tags: Ok(Default::default()),
                title: Ok(Default::default()),
                version: Err("no value supplied for version".to_string()),
            }
        }
    }
    impl Document {
        pub fn dsl<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::WorkflowDsl>,
            T::Error: ::std::fmt::Display,
        {
            self.dsl = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for dsl: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::WorkflowName>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {}", e));
            self
        }
        pub fn namespace<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::WorkflowNamespace>,
            T::Error: ::std::fmt::Display,
        {
            self.namespace = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for namespace: {}", e));
            self
        }
        pub fn summary<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.summary = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for summary: {}", e));
            self
        }
        pub fn tags<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.tags = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for tags: {}", e));
            self
        }
        pub fn title<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {}", e));
            self
        }
        pub fn version<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::WorkflowVersion>,
            T::Error: ::std::fmt::Display,
        {
            self.version = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for version: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<Document> for super::Document {
        type Error = super::error::ConversionError;
        fn try_from(value: Document) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                dsl: value.dsl?,
                metadata: value.metadata?,
                name: value.name?,
                namespace: value.namespace?,
                summary: value.summary?,
                tags: value.tags?,
                title: value.title?,
                version: value.version?,
            })
        }
    }
    impl ::std::convert::From<super::Document> for Document {
        fn from(value: super::Document) -> Self {
            Self {
                dsl: Ok(value.dsl),
                metadata: Ok(value.metadata),
                name: Ok(value.name),
                namespace: Ok(value.namespace),
                summary: Ok(value.summary),
                tags: Ok(value.tags),
                title: Ok(value.title),
                version: Ok(value.version),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Error {
        detail: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        instance: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        status: ::std::result::Result<i64, ::std::string::String>,
        title: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        type_: ::std::result::Result<super::UriTemplate, ::std::string::String>,
    }
    impl ::std::default::Default for Error {
        fn default() -> Self {
            Self {
                detail: Ok(Default::default()),
                instance: Ok(Default::default()),
                status: Err("no value supplied for status".to_string()),
                title: Ok(Default::default()),
                type_: Err("no value supplied for type_".to_string()),
            }
        }
    }
    impl Error {
        pub fn detail<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.detail = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for detail: {}", e));
            self
        }
        pub fn instance<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.instance = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for instance: {}", e));
            self
        }
        pub fn status<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<i64>,
            T::Error: ::std::fmt::Display,
        {
            self.status = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for status: {}", e));
            self
        }
        pub fn title<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {}", e));
            self
        }
        pub fn type_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::UriTemplate>,
            T::Error: ::std::fmt::Display,
        {
            self.type_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for type_: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<Error> for super::Error {
        type Error = super::error::ConversionError;
        fn try_from(value: Error) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                detail: value.detail?,
                instance: value.instance?,
                status: value.status?,
                title: value.title?,
                type_: value.type_?,
            })
        }
    }
    impl ::std::convert::From<super::Error> for Error {
        fn from(value: super::Error) -> Self {
            Self {
                detail: Ok(value.detail),
                instance: Ok(value.instance),
                status: Ok(value.status),
                title: Ok(value.title),
                type_: Ok(value.type_),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ErrorFilter {
        details: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        instance: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        status: ::std::result::Result<::std::option::Option<i64>, ::std::string::String>,
        title: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        type_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ErrorFilter {
        fn default() -> Self {
            Self {
                details: Ok(Default::default()),
                instance: Ok(Default::default()),
                status: Ok(Default::default()),
                title: Ok(Default::default()),
                type_: Ok(Default::default()),
            }
        }
    }
    impl ErrorFilter {
        pub fn details<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.details = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for details: {}", e));
            self
        }
        pub fn instance<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.instance = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for instance: {}", e));
            self
        }
        pub fn status<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i64>>,
            T::Error: ::std::fmt::Display,
        {
            self.status = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for status: {}", e));
            self
        }
        pub fn title<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {}", e));
            self
        }
        pub fn type_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.type_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for type_: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<ErrorFilter> for super::ErrorFilter {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ErrorFilter,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                details: value.details?,
                instance: value.instance?,
                status: value.status?,
                title: value.title?,
                type_: value.type_?,
            })
        }
    }
    impl ::std::convert::From<super::ErrorFilter> for ErrorFilter {
        fn from(value: super::ErrorFilter) -> Self {
            Self {
                details: Ok(value.details),
                instance: Ok(value.instance),
                status: Ok(value.status),
                title: Ok(value.title),
                type_: Ok(value.type_),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Export {
        as_: ::std::result::Result<::std::option::Option<super::ExportAs>, ::std::string::String>,
        schema: ::std::result::Result<::std::option::Option<super::Schema>, ::std::string::String>,
    }
    impl ::std::default::Default for Export {
        fn default() -> Self {
            Self {
                as_: Ok(Default::default()),
                schema: Ok(Default::default()),
            }
        }
    }
    impl Export {
        pub fn as_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ExportAs>>,
            T::Error: ::std::fmt::Display,
        {
            self.as_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for as_: {}", e));
            self
        }
        pub fn schema<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Schema>>,
            T::Error: ::std::fmt::Display,
        {
            self.schema = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for schema: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<Export> for super::Export {
        type Error = super::error::ConversionError;
        fn try_from(value: Export) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                as_: value.as_?,
                schema: value.schema?,
            })
        }
    }
    impl ::std::convert::From<super::Export> for Export {
        fn from(value: super::Export) -> Self {
            Self {
                as_: Ok(value.as_),
                schema: Ok(value.schema),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ExternalResource {
        endpoint: ::std::result::Result<super::Endpoint, ::std::string::String>,
        name: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ExternalResource {
        fn default() -> Self {
            Self {
                endpoint: Err("no value supplied for endpoint".to_string()),
                name: Ok(Default::default()),
            }
        }
    }
    impl ExternalResource {
        pub fn endpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::Endpoint>,
            T::Error: ::std::fmt::Display,
        {
            self.endpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for endpoint: {}", e));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<ExternalResource> for super::ExternalResource {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ExternalResource,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                endpoint: value.endpoint?,
                name: value.name?,
            })
        }
    }
    impl ::std::convert::From<super::ExternalResource> for ExternalResource {
        fn from(value: super::ExternalResource) -> Self {
            Self {
                endpoint: Ok(value.endpoint),
                name: Ok(value.name),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ForTask {
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        do_: ::std::result::Result<super::TaskList, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        for_: ::std::result::Result<super::ForTaskConfiguration, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        in_parallel: ::std::result::Result<bool, ::std::string::String>,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        on_error: ::std::result::Result<super::ForOnError, ::std::string::String>,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
        while_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ForTask {
        fn default() -> Self {
            Self {
                checkpoint: Ok(Default::default()),
                do_: Err("no value supplied for do_".to_string()),
                export: Ok(Default::default()),
                for_: Err("no value supplied for for_".to_string()),
                if_: Ok(Default::default()),
                in_parallel: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                on_error: Ok(super::defaults::for_task_on_error()),
                output: Ok(Default::default()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
                while_: Ok(Default::default()),
            }
        }
    }
    impl ForTask {
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn do_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TaskList>,
            T::Error: ::std::fmt::Display,
        {
            self.do_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for do_: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn for_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ForTaskConfiguration>,
            T::Error: ::std::fmt::Display,
        {
            self.for_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for for_: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn in_parallel<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.in_parallel = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for in_parallel: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn on_error<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ForOnError>,
            T::Error: ::std::fmt::Display,
        {
            self.on_error = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for on_error: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
        pub fn while_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.while_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for while_: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<ForTask> for super::ForTask {
        type Error = super::error::ConversionError;
        fn try_from(value: ForTask) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpoint: value.checkpoint?,
                do_: value.do_?,
                export: value.export?,
                for_: value.for_?,
                if_: value.if_?,
                in_parallel: value.in_parallel?,
                input: value.input?,
                metadata: value.metadata?,
                on_error: value.on_error?,
                output: value.output?,
                then: value.then?,
                timeout: value.timeout?,
                while_: value.while_?,
            })
        }
    }
    impl ::std::convert::From<super::ForTask> for ForTask {
        fn from(value: super::ForTask) -> Self {
            Self {
                checkpoint: Ok(value.checkpoint),
                do_: Ok(value.do_),
                export: Ok(value.export),
                for_: Ok(value.for_),
                if_: Ok(value.if_),
                in_parallel: Ok(value.in_parallel),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                on_error: Ok(value.on_error),
                output: Ok(value.output),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
                while_: Ok(value.while_),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ForTaskConfiguration {
        at: ::std::result::Result<::std::string::String, ::std::string::String>,
        each: ::std::result::Result<::std::string::String, ::std::string::String>,
        in_: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for ForTaskConfiguration {
        fn default() -> Self {
            Self {
                at: Ok(super::defaults::for_task_configuration_at()),
                each: Ok(super::defaults::for_task_configuration_each()),
                in_: Err("no value supplied for in_".to_string()),
            }
        }
    }
    impl ForTaskConfiguration {
        pub fn at<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.at = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for at: {}", e));
            self
        }
        pub fn each<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.each = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for each: {}", e));
            self
        }
        pub fn in_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.in_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for in_: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<ForTaskConfiguration> for super::ForTaskConfiguration {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ForTaskConfiguration,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                at: value.at?,
                each: value.each?,
                in_: value.in_?,
            })
        }
    }
    impl ::std::convert::From<super::ForTaskConfiguration> for ForTaskConfiguration {
        fn from(value: super::ForTaskConfiguration) -> Self {
            Self {
                at: Ok(value.at),
                each: Ok(value.each),
                in_: Ok(value.in_),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ForkTask {
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        fork: ::std::result::Result<super::ForkTaskConfiguration, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
    }
    impl ::std::default::Default for ForkTask {
        fn default() -> Self {
            Self {
                checkpoint: Ok(Default::default()),
                export: Ok(Default::default()),
                fork: Err("no value supplied for fork".to_string()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
            }
        }
    }
    impl ForkTask {
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn fork<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ForkTaskConfiguration>,
            T::Error: ::std::fmt::Display,
        {
            self.fork = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for fork: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<ForkTask> for super::ForkTask {
        type Error = super::error::ConversionError;
        fn try_from(value: ForkTask) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpoint: value.checkpoint?,
                export: value.export?,
                fork: value.fork?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                then: value.then?,
                timeout: value.timeout?,
            })
        }
    }
    impl ::std::convert::From<super::ForkTask> for ForkTask {
        fn from(value: super::ForkTask) -> Self {
            Self {
                checkpoint: Ok(value.checkpoint),
                export: Ok(value.export),
                fork: Ok(value.fork),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ForkTaskConfiguration {
        branches: ::std::result::Result<super::TaskList, ::std::string::String>,
        compete: ::std::result::Result<bool, ::std::string::String>,
    }
    impl ::std::default::Default for ForkTaskConfiguration {
        fn default() -> Self {
            Self {
                branches: Err("no value supplied for branches".to_string()),
                compete: Ok(Default::default()),
            }
        }
    }
    impl ForkTaskConfiguration {
        pub fn branches<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TaskList>,
            T::Error: ::std::fmt::Display,
        {
            self.branches = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for branches: {}", e));
            self
        }
        pub fn compete<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.compete = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for compete: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<ForkTaskConfiguration> for super::ForkTaskConfiguration {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ForkTaskConfiguration,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                branches: value.branches?,
                compete: value.compete?,
            })
        }
    }
    impl ::std::convert::From<super::ForkTaskConfiguration> for ForkTaskConfiguration {
        fn from(value: super::ForkTaskConfiguration) -> Self {
            Self {
                branches: Ok(value.branches),
                compete: Ok(value.compete),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Input {
        from: ::std::result::Result<::std::option::Option<super::InputFrom>, ::std::string::String>,
        schema: ::std::result::Result<::std::option::Option<super::Schema>, ::std::string::String>,
    }
    impl ::std::default::Default for Input {
        fn default() -> Self {
            Self {
                from: Ok(Default::default()),
                schema: Ok(Default::default()),
            }
        }
    }
    impl Input {
        pub fn from<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::InputFrom>>,
            T::Error: ::std::fmt::Display,
        {
            self.from = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for from: {}", e));
            self
        }
        pub fn schema<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Schema>>,
            T::Error: ::std::fmt::Display,
        {
            self.schema = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for schema: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<Input> for super::Input {
        type Error = super::error::ConversionError;
        fn try_from(value: Input) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                from: value.from?,
                schema: value.schema?,
            })
        }
    }
    impl ::std::convert::From<super::Input> for Input {
        fn from(value: super::Input) -> Self {
            Self {
                from: Ok(value.from),
                schema: Ok(value.schema),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Output {
        as_: ::std::result::Result<::std::option::Option<super::OutputAs>, ::std::string::String>,
        schema: ::std::result::Result<::std::option::Option<super::Schema>, ::std::string::String>,
    }
    impl ::std::default::Default for Output {
        fn default() -> Self {
            Self {
                as_: Ok(Default::default()),
                schema: Ok(Default::default()),
            }
        }
    }
    impl Output {
        pub fn as_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::OutputAs>>,
            T::Error: ::std::fmt::Display,
        {
            self.as_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for as_: {}", e));
            self
        }
        pub fn schema<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Schema>>,
            T::Error: ::std::fmt::Display,
        {
            self.schema = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for schema: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<Output> for super::Output {
        type Error = super::error::ConversionError;
        fn try_from(value: Output) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                as_: value.as_?,
                schema: value.schema?,
            })
        }
    }
    impl ::std::convert::From<super::Output> for Output {
        fn from(value: super::Output) -> Self {
            Self {
                as_: Ok(value.as_),
                schema: Ok(value.schema),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ProcessResult {
        code: ::std::result::Result<i64, ::std::string::String>,
        stderr: ::std::result::Result<::std::string::String, ::std::string::String>,
        stdout: ::std::result::Result<::std::string::String, ::std::string::String>,
    }
    impl ::std::default::Default for ProcessResult {
        fn default() -> Self {
            Self {
                code: Err("no value supplied for code".to_string()),
                stderr: Err("no value supplied for stderr".to_string()),
                stdout: Err("no value supplied for stdout".to_string()),
            }
        }
    }
    impl ProcessResult {
        pub fn code<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<i64>,
            T::Error: ::std::fmt::Display,
        {
            self.code = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for code: {}", e));
            self
        }
        pub fn stderr<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.stderr = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for stderr: {}", e));
            self
        }
        pub fn stdout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.stdout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for stdout: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<ProcessResult> for super::ProcessResult {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ProcessResult,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                code: value.code?,
                stderr: value.stderr?,
                stdout: value.stdout?,
            })
        }
    }
    impl ::std::convert::From<super::ProcessResult> for ProcessResult {
        fn from(value: super::ProcessResult) -> Self {
            Self {
                code: Ok(value.code),
                stderr: Ok(value.stderr),
                stdout: Ok(value.stdout),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RaiseTask {
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        raise: ::std::result::Result<super::RaiseTaskConfiguration, ::std::string::String>,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
    }
    impl ::std::default::Default for RaiseTask {
        fn default() -> Self {
            Self {
                checkpoint: Ok(Default::default()),
                export: Ok(Default::default()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                raise: Err("no value supplied for raise".to_string()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
            }
        }
    }
    impl RaiseTask {
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn raise<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RaiseTaskConfiguration>,
            T::Error: ::std::fmt::Display,
        {
            self.raise = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for raise: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RaiseTask> for super::RaiseTask {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RaiseTask,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpoint: value.checkpoint?,
                export: value.export?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                raise: value.raise?,
                then: value.then?,
                timeout: value.timeout?,
            })
        }
    }
    impl ::std::convert::From<super::RaiseTask> for RaiseTask {
        fn from(value: super::RaiseTask) -> Self {
            Self {
                checkpoint: Ok(value.checkpoint),
                export: Ok(value.export),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                raise: Ok(value.raise),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RaiseTaskConfiguration {
        error: ::std::result::Result<super::RaiseTaskError, ::std::string::String>,
    }
    impl ::std::default::Default for RaiseTaskConfiguration {
        fn default() -> Self {
            Self {
                error: Err("no value supplied for error".to_string()),
            }
        }
    }
    impl RaiseTaskConfiguration {
        pub fn error<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RaiseTaskError>,
            T::Error: ::std::fmt::Display,
        {
            self.error = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for error: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RaiseTaskConfiguration> for super::RaiseTaskConfiguration {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RaiseTaskConfiguration,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                error: value.error?,
            })
        }
    }
    impl ::std::convert::From<super::RaiseTaskConfiguration> for RaiseTaskConfiguration {
        fn from(value: super::RaiseTaskConfiguration) -> Self {
            Self {
                error: Ok(value.error),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RetryLimit {
        attempt: ::std::result::Result<
            ::std::option::Option<super::RetryLimitAttempt>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for RetryLimit {
        fn default() -> Self {
            Self {
                attempt: Ok(Default::default()),
            }
        }
    }
    impl RetryLimit {
        pub fn attempt<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::RetryLimitAttempt>>,
            T::Error: ::std::fmt::Display,
        {
            self.attempt = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for attempt: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RetryLimit> for super::RetryLimit {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RetryLimit,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                attempt: value.attempt?,
            })
        }
    }
    impl ::std::convert::From<super::RetryLimit> for RetryLimit {
        fn from(value: super::RetryLimit) -> Self {
            Self {
                attempt: Ok(value.attempt),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RetryLimitAttempt {
        count: ::std::result::Result<::std::option::Option<i64>, ::std::string::String>,
        duration:
            ::std::result::Result<::std::option::Option<super::Duration>, ::std::string::String>,
    }
    impl ::std::default::Default for RetryLimitAttempt {
        fn default() -> Self {
            Self {
                count: Ok(Default::default()),
                duration: Ok(Default::default()),
            }
        }
    }
    impl RetryLimitAttempt {
        pub fn count<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<i64>>,
            T::Error: ::std::fmt::Display,
        {
            self.count = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for count: {}", e));
            self
        }
        pub fn duration<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Duration>>,
            T::Error: ::std::fmt::Display,
        {
            self.duration = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for duration: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RetryLimitAttempt> for super::RetryLimitAttempt {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RetryLimitAttempt,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                count: value.count?,
                duration: value.duration?,
            })
        }
    }
    impl ::std::convert::From<super::RetryLimitAttempt> for RetryLimitAttempt {
        fn from(value: super::RetryLimitAttempt) -> Self {
            Self {
                count: Ok(value.count),
                duration: Ok(value.duration),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RetryPolicy {
        backoff: ::std::result::Result<
            ::std::option::Option<super::RetryBackoff>,
            ::std::string::String,
        >,
        delay: ::std::result::Result<::std::option::Option<super::Duration>, ::std::string::String>,
        limit:
            ::std::result::Result<::std::option::Option<super::RetryLimit>, ::std::string::String>,
    }
    impl ::std::default::Default for RetryPolicy {
        fn default() -> Self {
            Self {
                backoff: Ok(Default::default()),
                delay: Ok(Default::default()),
                limit: Ok(Default::default()),
            }
        }
    }
    impl RetryPolicy {
        pub fn backoff<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::RetryBackoff>>,
            T::Error: ::std::fmt::Display,
        {
            self.backoff = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for backoff: {}", e));
            self
        }
        pub fn delay<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Duration>>,
            T::Error: ::std::fmt::Display,
        {
            self.delay = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for delay: {}", e));
            self
        }
        pub fn limit<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::RetryLimit>>,
            T::Error: ::std::fmt::Display,
        {
            self.limit = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for limit: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RetryPolicy> for super::RetryPolicy {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RetryPolicy,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                backoff: value.backoff?,
                delay: value.delay?,
                limit: value.limit?,
            })
        }
    }
    impl ::std::convert::From<super::RetryPolicy> for RetryPolicy {
        fn from(value: super::RetryPolicy) -> Self {
            Self {
                backoff: Ok(value.backoff),
                delay: Ok(value.delay),
                limit: Ok(value.limit),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RunFunction {
        function: ::std::result::Result<super::RunJobFunction, ::std::string::String>,
    }
    impl ::std::default::Default for RunFunction {
        fn default() -> Self {
            Self {
                function: Err("no value supplied for function".to_string()),
            }
        }
    }
    impl RunFunction {
        pub fn function<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RunJobFunction>,
            T::Error: ::std::fmt::Display,
        {
            self.function = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for function: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RunFunction> for super::RunFunction {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RunFunction,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                function: value.function?,
            })
        }
    }
    impl ::std::convert::From<super::RunFunction> for RunFunction {
        fn from(value: super::RunFunction) -> Self {
            Self {
                function: Ok(value.function),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RunJobRunner {
        arguments: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        options: ::std::result::Result<
            ::std::option::Option<super::WorkerOptions>,
            ::std::string::String,
        >,
        settings: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        using: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for RunJobRunner {
        fn default() -> Self {
            Self {
                arguments: Err("no value supplied for arguments".to_string()),
                name: Err("no value supplied for name".to_string()),
                options: Ok(Default::default()),
                settings: Ok(Default::default()),
                using: Ok(Default::default()),
            }
        }
    }
    impl RunJobRunner {
        pub fn arguments<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.arguments = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for arguments: {}", e));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {}", e));
            self
        }
        pub fn options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::WorkerOptions>>,
            T::Error: ::std::fmt::Display,
        {
            self.options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for options: {}", e));
            self
        }
        pub fn settings<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.settings = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for settings: {}", e));
            self
        }
        pub fn using<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.using = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for using: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RunJobRunner> for super::RunJobRunner {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RunJobRunner,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                arguments: value.arguments?,
                name: value.name?,
                options: value.options?,
                settings: value.settings?,
                using: value.using?,
            })
        }
    }
    impl ::std::convert::From<super::RunJobRunner> for RunJobRunner {
        fn from(value: super::RunJobRunner) -> Self {
            Self {
                arguments: Ok(value.arguments),
                name: Ok(value.name),
                options: Ok(value.options),
                settings: Ok(value.settings),
                using: Ok(value.using),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RunJobWorker {
        arguments: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        name: ::std::result::Result<::std::string::String, ::std::string::String>,
        using: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for RunJobWorker {
        fn default() -> Self {
            Self {
                arguments: Err("no value supplied for arguments".to_string()),
                name: Err("no value supplied for name".to_string()),
                using: Ok(Default::default()),
            }
        }
    }
    impl RunJobWorker {
        pub fn arguments<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.arguments = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for arguments: {}", e));
            self
        }
        pub fn name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for name: {}", e));
            self
        }
        pub fn using<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.using = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for using: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RunJobWorker> for super::RunJobWorker {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RunJobWorker,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                arguments: value.arguments?,
                name: value.name?,
                using: value.using?,
            })
        }
    }
    impl ::std::convert::From<super::RunJobWorker> for RunJobWorker {
        fn from(value: super::RunJobWorker) -> Self {
            Self {
                arguments: Ok(value.arguments),
                name: Ok(value.name),
                using: Ok(value.using),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RunRunner {
        runner: ::std::result::Result<super::RunJobRunner, ::std::string::String>,
    }
    impl ::std::default::Default for RunRunner {
        fn default() -> Self {
            Self {
                runner: Err("no value supplied for runner".to_string()),
            }
        }
    }
    impl RunRunner {
        pub fn runner<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RunJobRunner>,
            T::Error: ::std::fmt::Display,
        {
            self.runner = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for runner: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RunRunner> for super::RunRunner {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RunRunner,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                runner: value.runner?,
            })
        }
    }
    impl ::std::convert::From<super::RunRunner> for RunRunner {
        fn from(value: super::RunRunner) -> Self {
            Self {
                runner: Ok(value.runner),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RunScript {
        script: ::std::result::Result<super::ScriptConfiguration, ::std::string::String>,
    }
    impl ::std::default::Default for RunScript {
        fn default() -> Self {
            Self {
                script: Err("no value supplied for script".to_string()),
            }
        }
    }
    impl RunScript {
        pub fn script<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ScriptConfiguration>,
            T::Error: ::std::fmt::Display,
        {
            self.script = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for script: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RunScript> for super::RunScript {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RunScript,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                script: value.script?,
            })
        }
    }
    impl ::std::convert::From<super::RunScript> for RunScript {
        fn from(value: super::RunScript) -> Self {
            Self {
                script: Ok(value.script),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RunTask {
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        run: ::std::result::Result<super::RunTaskConfiguration, ::std::string::String>,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
    }
    impl ::std::default::Default for RunTask {
        fn default() -> Self {
            Self {
                checkpoint: Ok(Default::default()),
                export: Ok(Default::default()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                run: Err("no value supplied for run".to_string()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
            }
        }
    }
    impl RunTask {
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn run<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RunTaskConfiguration>,
            T::Error: ::std::fmt::Display,
        {
            self.run = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for run: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RunTask> for super::RunTask {
        type Error = super::error::ConversionError;
        fn try_from(value: RunTask) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpoint: value.checkpoint?,
                export: value.export?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                run: value.run?,
                then: value.then?,
                timeout: value.timeout?,
            })
        }
    }
    impl ::std::convert::From<super::RunTask> for RunTask {
        fn from(value: super::RunTask) -> Self {
            Self {
                checkpoint: Ok(value.checkpoint),
                export: Ok(value.export),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                run: Ok(value.run),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct RunWorker {
        worker: ::std::result::Result<super::RunJobWorker, ::std::string::String>,
    }
    impl ::std::default::Default for RunWorker {
        fn default() -> Self {
            Self {
                worker: Err("no value supplied for worker".to_string()),
            }
        }
    }
    impl RunWorker {
        pub fn worker<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::RunJobWorker>,
            T::Error: ::std::fmt::Display,
        {
            self.worker = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for worker: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RunWorker> for super::RunWorker {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RunWorker,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                worker: value.worker?,
            })
        }
    }
    impl ::std::convert::From<super::RunWorker> for RunWorker {
        fn from(value: super::RunWorker) -> Self {
            Self {
                worker: Ok(value.worker),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SetTask {
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        set: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
    }
    impl ::std::default::Default for SetTask {
        fn default() -> Self {
            Self {
                checkpoint: Ok(Default::default()),
                export: Ok(Default::default()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                set: Err("no value supplied for set".to_string()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
            }
        }
    }
    impl SetTask {
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn set<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.set = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for set: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<SetTask> for super::SetTask {
        type Error = super::error::ConversionError;
        fn try_from(value: SetTask) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpoint: value.checkpoint?,
                export: value.export?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                set: value.set?,
                then: value.then?,
                timeout: value.timeout?,
            })
        }
    }
    impl ::std::convert::From<super::SetTask> for SetTask {
        fn from(value: super::SetTask) -> Self {
            Self {
                checkpoint: Ok(value.checkpoint),
                export: Ok(value.export),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                set: Ok(value.set),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SwitchCase {
        then: ::std::result::Result<super::FlowDirective, ::std::string::String>,
        when: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for SwitchCase {
        fn default() -> Self {
            Self {
                then: Err("no value supplied for then".to_string()),
                when: Ok(Default::default()),
            }
        }
    }
    impl SwitchCase {
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::FlowDirective>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn when<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.when = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for when: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<SwitchCase> for super::SwitchCase {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SwitchCase,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                then: value.then?,
                when: value.when?,
            })
        }
    }
    impl ::std::convert::From<super::SwitchCase> for SwitchCase {
        fn from(value: super::SwitchCase) -> Self {
            Self {
                then: Ok(value.then),
                when: Ok(value.when),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SwitchTask {
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        switch: ::std::result::Result<
            ::std::vec::Vec<::std::collections::HashMap<::std::string::String, super::SwitchCase>>,
            ::std::string::String,
        >,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
    }
    impl ::std::default::Default for SwitchTask {
        fn default() -> Self {
            Self {
                checkpoint: Ok(Default::default()),
                export: Ok(Default::default()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                switch: Err("no value supplied for switch".to_string()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
            }
        }
    }
    impl SwitchTask {
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn switch<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::std::vec::Vec<
                    ::std::collections::HashMap<::std::string::String, super::SwitchCase>,
                >,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.switch = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for switch: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<SwitchTask> for super::SwitchTask {
        type Error = super::error::ConversionError;
        fn try_from(
            value: SwitchTask,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpoint: value.checkpoint?,
                export: value.export?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                switch: value.switch?,
                then: value.then?,
                timeout: value.timeout?,
            })
        }
    }
    impl ::std::convert::From<super::SwitchTask> for SwitchTask {
        fn from(value: super::SwitchTask) -> Self {
            Self {
                checkpoint: Ok(value.checkpoint),
                export: Ok(value.export),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                switch: Ok(value.switch),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct TaskBase {
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
    }
    impl ::std::default::Default for TaskBase {
        fn default() -> Self {
            Self {
                checkpoint: Ok(Default::default()),
                export: Ok(Default::default()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
            }
        }
    }
    impl TaskBase {
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<TaskBase> for super::TaskBase {
        type Error = super::error::ConversionError;
        fn try_from(value: TaskBase) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpoint: value.checkpoint?,
                export: value.export?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                then: value.then?,
                timeout: value.timeout?,
            })
        }
    }
    impl ::std::convert::From<super::TaskBase> for TaskBase {
        fn from(value: super::TaskBase) -> Self {
            Self {
                checkpoint: Ok(value.checkpoint),
                export: Ok(value.export),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct Timeout {
        after: ::std::result::Result<super::Duration, ::std::string::String>,
    }
    impl ::std::default::Default for Timeout {
        fn default() -> Self {
            Self {
                after: Err("no value supplied for after".to_string()),
            }
        }
    }
    impl Timeout {
        pub fn after<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::Duration>,
            T::Error: ::std::fmt::Display,
        {
            self.after = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for after: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<Timeout> for super::Timeout {
        type Error = super::error::ConversionError;
        fn try_from(value: Timeout) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                after: value.after?,
            })
        }
    }
    impl ::std::convert::From<super::Timeout> for Timeout {
        fn from(value: super::Timeout) -> Self {
            Self {
                after: Ok(value.after),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct TryTask {
        catch: ::std::result::Result<super::TryTaskCatch, ::std::string::String>,
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
        try_: ::std::result::Result<super::TaskList, ::std::string::String>,
    }
    impl ::std::default::Default for TryTask {
        fn default() -> Self {
            Self {
                catch: Err("no value supplied for catch".to_string()),
                checkpoint: Ok(Default::default()),
                export: Ok(Default::default()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
                try_: Err("no value supplied for try_".to_string()),
            }
        }
    }
    impl TryTask {
        pub fn catch<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TryTaskCatch>,
            T::Error: ::std::fmt::Display,
        {
            self.catch = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for catch: {}", e));
            self
        }
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
        pub fn try_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TaskList>,
            T::Error: ::std::fmt::Display,
        {
            self.try_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for try_: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<TryTask> for super::TryTask {
        type Error = super::error::ConversionError;
        fn try_from(value: TryTask) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                catch: value.catch?,
                checkpoint: value.checkpoint?,
                export: value.export?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                then: value.then?,
                timeout: value.timeout?,
                try_: value.try_?,
            })
        }
    }
    impl ::std::convert::From<super::TryTask> for TryTask {
        fn from(value: super::TryTask) -> Self {
            Self {
                catch: Ok(value.catch),
                checkpoint: Ok(value.checkpoint),
                export: Ok(value.export),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
                try_: Ok(value.try_),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct TryTaskCatch {
        as_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        do_: ::std::result::Result<::std::option::Option<super::TaskList>, ::std::string::String>,
        errors:
            ::std::result::Result<::std::option::Option<super::CatchErrors>, ::std::string::String>,
        except_when: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        retry: ::std::result::Result<
            ::std::option::Option<super::TryTaskCatchRetry>,
            ::std::string::String,
        >,
        when: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for TryTaskCatch {
        fn default() -> Self {
            Self {
                as_: Ok(Default::default()),
                do_: Ok(Default::default()),
                errors: Ok(Default::default()),
                except_when: Ok(Default::default()),
                retry: Ok(Default::default()),
                when: Ok(Default::default()),
            }
        }
    }
    impl TryTaskCatch {
        pub fn as_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.as_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for as_: {}", e));
            self
        }
        pub fn do_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskList>>,
            T::Error: ::std::fmt::Display,
        {
            self.do_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for do_: {}", e));
            self
        }
        pub fn errors<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CatchErrors>>,
            T::Error: ::std::fmt::Display,
        {
            self.errors = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for errors: {}", e));
            self
        }
        pub fn except_when<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.except_when = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for except_when: {}", e));
            self
        }
        pub fn retry<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TryTaskCatchRetry>>,
            T::Error: ::std::fmt::Display,
        {
            self.retry = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for retry: {}", e));
            self
        }
        pub fn when<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.when = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for when: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<TryTaskCatch> for super::TryTaskCatch {
        type Error = super::error::ConversionError;
        fn try_from(
            value: TryTaskCatch,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                as_: value.as_?,
                do_: value.do_?,
                errors: value.errors?,
                except_when: value.except_when?,
                retry: value.retry?,
                when: value.when?,
            })
        }
    }
    impl ::std::convert::From<super::TryTaskCatch> for TryTaskCatch {
        fn from(value: super::TryTaskCatch) -> Self {
            Self {
                as_: Ok(value.as_),
                do_: Ok(value.do_),
                errors: Ok(value.errors),
                except_when: Ok(value.except_when),
                retry: Ok(value.retry),
                when: Ok(value.when),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct WaitTask {
        checkpoint: ::std::result::Result<bool, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        if_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        input: ::std::result::Result<::std::option::Option<super::Input>, ::std::string::String>,
        metadata: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
        then: ::std::result::Result<
            ::std::option::Option<super::FlowDirective>,
            ::std::string::String,
        >,
        timeout:
            ::std::result::Result<::std::option::Option<super::TaskTimeout>, ::std::string::String>,
        wait: ::std::result::Result<super::Duration, ::std::string::String>,
    }
    impl ::std::default::Default for WaitTask {
        fn default() -> Self {
            Self {
                checkpoint: Ok(Default::default()),
                export: Ok(Default::default()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
                wait: Err("no value supplied for wait".to_string()),
            }
        }
    }
    impl WaitTask {
        pub fn checkpoint<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpoint = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpoint: {}", e));
            self
        }
        pub fn export<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Export>>,
            T::Error: ::std::fmt::Display,
        {
            self.export = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for export: {}", e));
            self
        }
        pub fn if_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.if_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for if_: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Input>>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn metadata<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.metadata = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for metadata: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
        pub fn then<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirective>>,
            T::Error: ::std::fmt::Display,
        {
            self.then = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for then: {}", e));
            self
        }
        pub fn timeout<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::TaskTimeout>>,
            T::Error: ::std::fmt::Display,
        {
            self.timeout = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for timeout: {}", e));
            self
        }
        pub fn wait<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::Duration>,
            T::Error: ::std::fmt::Display,
        {
            self.wait = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for wait: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<WaitTask> for super::WaitTask {
        type Error = super::error::ConversionError;
        fn try_from(value: WaitTask) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpoint: value.checkpoint?,
                export: value.export?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                then: value.then?,
                timeout: value.timeout?,
                wait: value.wait?,
            })
        }
    }
    impl ::std::convert::From<super::WaitTask> for WaitTask {
        fn from(value: super::WaitTask) -> Self {
            Self {
                checkpoint: Ok(value.checkpoint),
                export: Ok(value.export),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
                wait: Ok(value.wait),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct WorkerOptions {
        broadcast_results:
            ::std::result::Result<::std::option::Option<bool>, ::std::string::String>,
        channel: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        queue_type:
            ::std::result::Result<::std::option::Option<super::QueueType>, ::std::string::String>,
        response_type: ::std::result::Result<
            ::std::option::Option<super::ResponseType>,
            ::std::string::String,
        >,
        retry:
            ::std::result::Result<::std::option::Option<super::RetryPolicy>, ::std::string::String>,
        store_failure: ::std::result::Result<::std::option::Option<bool>, ::std::string::String>,
        store_success: ::std::result::Result<::std::option::Option<bool>, ::std::string::String>,
        use_static: ::std::result::Result<::std::option::Option<bool>, ::std::string::String>,
    }
    impl ::std::default::Default for WorkerOptions {
        fn default() -> Self {
            Self {
                broadcast_results: Ok(Default::default()),
                channel: Ok(Default::default()),
                queue_type: Ok(Default::default()),
                response_type: Ok(Default::default()),
                retry: Ok(Default::default()),
                store_failure: Ok(Default::default()),
                store_success: Ok(Default::default()),
                use_static: Ok(Default::default()),
            }
        }
    }
    impl WorkerOptions {
        pub fn broadcast_results<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<bool>>,
            T::Error: ::std::fmt::Display,
        {
            self.broadcast_results = value.try_into().map_err(|e| {
                format!(
                    "error converting supplied value for broadcast_results: {}",
                    e
                )
            });
            self
        }
        pub fn channel<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.channel = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for channel: {}", e));
            self
        }
        pub fn queue_type<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::QueueType>>,
            T::Error: ::std::fmt::Display,
        {
            self.queue_type = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for queue_type: {}", e));
            self
        }
        pub fn response_type<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ResponseType>>,
            T::Error: ::std::fmt::Display,
        {
            self.response_type = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for response_type: {}", e));
            self
        }
        pub fn retry<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::RetryPolicy>>,
            T::Error: ::std::fmt::Display,
        {
            self.retry = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for retry: {}", e));
            self
        }
        pub fn store_failure<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<bool>>,
            T::Error: ::std::fmt::Display,
        {
            self.store_failure = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for store_failure: {}", e));
            self
        }
        pub fn store_success<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<bool>>,
            T::Error: ::std::fmt::Display,
        {
            self.store_success = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for store_success: {}", e));
            self
        }
        pub fn use_static<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<bool>>,
            T::Error: ::std::fmt::Display,
        {
            self.use_static = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for use_static: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<WorkerOptions> for super::WorkerOptions {
        type Error = super::error::ConversionError;
        fn try_from(
            value: WorkerOptions,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                broadcast_results: value.broadcast_results?,
                channel: value.channel?,
                queue_type: value.queue_type?,
                response_type: value.response_type?,
                retry: value.retry?,
                store_failure: value.store_failure?,
                store_success: value.store_success?,
                use_static: value.use_static?,
            })
        }
    }
    impl ::std::convert::From<super::WorkerOptions> for WorkerOptions {
        fn from(value: super::WorkerOptions) -> Self {
            Self {
                broadcast_results: Ok(value.broadcast_results),
                channel: Ok(value.channel),
                queue_type: Ok(value.queue_type),
                response_type: Ok(value.response_type),
                retry: Ok(value.retry),
                store_failure: Ok(value.store_failure),
                store_success: Ok(value.store_success),
                use_static: Ok(value.use_static),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct WorkflowSchema {
        checkpointing: ::std::result::Result<
            ::std::option::Option<super::CheckpointConfig>,
            ::std::string::String,
        >,
        do_: ::std::result::Result<super::TaskList, ::std::string::String>,
        document: ::std::result::Result<super::Document, ::std::string::String>,
        input: ::std::result::Result<super::Input, ::std::string::String>,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
    }
    impl ::std::default::Default for WorkflowSchema {
        fn default() -> Self {
            Self {
                checkpointing: Ok(Default::default()),
                do_: Err("no value supplied for do_".to_string()),
                document: Err("no value supplied for document".to_string()),
                input: Err("no value supplied for input".to_string()),
                output: Ok(Default::default()),
            }
        }
    }
    impl WorkflowSchema {
        pub fn checkpointing<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::CheckpointConfig>>,
            T::Error: ::std::fmt::Display,
        {
            self.checkpointing = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for checkpointing: {}", e));
            self
        }
        pub fn do_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::TaskList>,
            T::Error: ::std::fmt::Display,
        {
            self.do_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for do_: {}", e));
            self
        }
        pub fn document<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::Document>,
            T::Error: ::std::fmt::Display,
        {
            self.document = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for document: {}", e));
            self
        }
        pub fn input<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::Input>,
            T::Error: ::std::fmt::Display,
        {
            self.input = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for input: {}", e));
            self
        }
        pub fn output<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::Output>>,
            T::Error: ::std::fmt::Display,
        {
            self.output = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for output: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<WorkflowSchema> for super::WorkflowSchema {
        type Error = super::error::ConversionError;
        fn try_from(
            value: WorkflowSchema,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                checkpointing: value.checkpointing?,
                do_: value.do_?,
                document: value.document?,
                input: value.input?,
                output: value.output?,
            })
        }
    }
    impl ::std::convert::From<super::WorkflowSchema> for WorkflowSchema {
        fn from(value: super::WorkflowSchema) -> Self {
            Self {
                checkpointing: Ok(value.checkpointing),
                do_: Ok(value.do_),
                document: Ok(value.document),
                input: Ok(value.input),
                output: Ok(value.output),
            }
        }
    }
}
#[doc = r" Generation of default values for serde."]
pub mod defaults {
    pub(super) fn for_task_on_error() -> super::ForOnError {
        super::ForOnError::Break
    }
    pub(super) fn for_task_configuration_at() -> ::std::string::String {
        "index".to_string()
    }
    pub(super) fn for_task_configuration_each() -> ::std::string::String {
        "item".to_string()
    }
    pub(super) fn schema_variant0_format() -> ::std::string::String {
        "json".to_string()
    }
    pub(super) fn schema_variant1_format() -> ::std::string::String {
        "json".to_string()
    }
}
