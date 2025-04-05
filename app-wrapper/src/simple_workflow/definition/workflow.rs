#![allow(clippy::redundant_closure_call)]
#![allow(clippy::needless_lifetimes)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::clone_on_copy)]
#![allow(irrefutable_let_patterns)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::large_enum_variant)]

pub mod supplement;
#[cfg(test)]
pub mod supplement_test;

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
#[doc = "CallTask"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"call\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"call\": {"]
#[doc = "      \"description\": \"The name of the function to call.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"with\": {"]
#[doc = "      \"title\": \"FunctionArguments\","]
#[doc = "      \"description\": \"A name/value mapping of the parameters, if any, to call the function with.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct CallTask {
    #[doc = "The name of the function to call."]
    pub call: ::std::string::String,
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "The flow directive to be performed upon completion of the task."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
    #[doc = "A name/value mapping of the parameters, if any, to call the function with."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub with: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
}
impl ::std::convert::From<&CallTask> for CallTask {
    fn from(value: &CallTask) -> Self {
        value.clone()
    }
}
impl CallTask {
    pub fn builder() -> builder::CallTask {
        Default::default()
    }
}
#[doc = "static error filter"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"CatchErrors\","]
#[doc = "  \"description\": \"static error filter\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"with\": {"]
#[doc = "      \"$ref\": \"#/$defs/errorFilter\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
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
#[doc = "    \"do\": {"]
#[doc = "      \"title\": \"DoTaskConfiguration\","]
#[doc = "      \"description\": \"The configuration of the tasks to perform sequentially.\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"for\": false,"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct DoTask {
    #[doc = "The configuration of the tasks to perform sequentially."]
    #[serde(rename = "do")]
    pub do_: TaskList,
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "The flow directive to be performed upon completion of the task."]
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
#[doc = "Documents the workflow."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Document\","]
#[doc = "  \"description\": \"Documents the workflow.\","]
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
#[doc = "      \"description\": \"The version of the DSL used by the workflow.\","]
#[doc = "      \"default\": \"0.0.1\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"WorkflowMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the workflow.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"title\": \"WorkflowName\","]
#[doc = "      \"description\": \"The workflow's name.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "    },"]
#[doc = "    \"namespace\": {"]
#[doc = "      \"title\": \"WorkflowNamespace\","]
#[doc = "      \"description\": \"The workflow's namespace.\","]
#[doc = "      \"default\": \"default\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "    },"]
#[doc = "    \"summary\": {"]
#[doc = "      \"title\": \"WorkflowSummary\","]
#[doc = "      \"description\": \"The workflow's Markdown summary.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"tags\": {"]
#[doc = "      \"title\": \"WorkflowTags\","]
#[doc = "      \"description\": \"A key/value mapping of the workflow's tags, if any.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"title\": \"WorkflowTitle\","]
#[doc = "      \"description\": \"The workflow's title.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"version\": {"]
#[doc = "      \"title\": \"WorkflowVersion\","]
#[doc = "      \"description\": \"The workflow's semantic version.\","]
#[doc = "      \"default\": \"0.0.1\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Document {
    #[doc = "The version of the DSL used by the workflow."]
    pub dsl: WorkflowDsl,
    #[doc = "Holds additional information about the workflow."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "The workflow's name."]
    pub name: WorkflowName,
    #[doc = "The workflow's namespace."]
    pub namespace: WorkflowNamespace,
    #[doc = "The workflow's Markdown summary."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub summary: ::std::option::Option<::std::string::String>,
    #[doc = "A key/value mapping of the workflow's tags, if any."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub tags: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "The workflow's title."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "The workflow's semantic version."]
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
#[doc = "      \"description\": \"The inline definition of a duration.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"minProperties\": 1,"]
#[doc = "      \"properties\": {"]
#[doc = "        \"days\": {"]
#[doc = "          \"title\": \"DurationDays\","]
#[doc = "          \"description\": \"Number of days, if any.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"hours\": {"]
#[doc = "          \"title\": \"DurationHours\","]
#[doc = "          \"description\": \"Number of days, if any.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"milliseconds\": {"]
#[doc = "          \"title\": \"DurationMilliseconds\","]
#[doc = "          \"description\": \"Number of milliseconds, if any.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"minutes\": {"]
#[doc = "          \"title\": \"DurationMinutes\","]
#[doc = "          \"description\": \"Number of minutes, if any.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"seconds\": {"]
#[doc = "          \"title\": \"DurationSeconds\","]
#[doc = "          \"description\": \"Number of seconds, if any.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"DurationExpression\","]
#[doc = "      \"description\": \"The ISO 8601 expression of a duration.\","]
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
        #[doc = "Number of days, if any."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        days: ::std::option::Option<i64>,
        #[doc = "Number of days, if any."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        hours: ::std::option::Option<i64>,
        #[doc = "Number of milliseconds, if any."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        milliseconds: ::std::option::Option<i64>,
        #[doc = "Number of minutes, if any."]
        #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
        minutes: ::std::option::Option<i64>,
        #[doc = "Number of seconds, if any."]
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
#[doc = "The ISO 8601 expression of a duration."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"DurationExpression\","]
#[doc = "  \"description\": \"The ISO 8601 expression of a duration.\","]
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
#[doc = "Represents an endpoint."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Endpoint\","]
#[doc = "  \"description\": \"Represents an endpoint.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "    },"]
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
#[doc = "          \"description\": \"The endpoint's URI.\","]
#[doc = "          \"oneOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"LiteralEndpointURI\","]
#[doc = "              \"description\": \"The literal endpoint's URI.\","]
#[doc = "              \"$ref\": \"#/$defs/uriTemplate\""]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"ExpressionEndpointURI\","]
#[doc = "              \"description\": \"An expression based endpoint's URI.\","]
#[doc = "              \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Endpoint {
    RuntimeExpression(RuntimeExpression),
    UriTemplate(UriTemplate),
    EndpointConfiguration {
        #[doc = "The endpoint's URI."]
        uri: EndpointUri,
    },
}
impl ::std::convert::From<&Self> for Endpoint {
    fn from(value: &Endpoint) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<RuntimeExpression> for Endpoint {
    fn from(value: RuntimeExpression) -> Self {
        Self::RuntimeExpression(value)
    }
}
impl ::std::convert::From<UriTemplate> for Endpoint {
    fn from(value: UriTemplate) -> Self {
        Self::UriTemplate(value)
    }
}
#[doc = "The endpoint's URI."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"EndpointUri\","]
#[doc = "  \"description\": \"The endpoint's URI.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"LiteralEndpointURI\","]
#[doc = "      \"description\": \"The literal endpoint's URI.\","]
#[doc = "      \"$ref\": \"#/$defs/uriTemplate\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExpressionEndpointURI\","]
#[doc = "      \"description\": \"An expression based endpoint's URI.\","]
#[doc = "      \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum EndpointUri {
    UriTemplate(UriTemplate),
    RuntimeExpression(RuntimeExpression),
}
impl ::std::convert::From<&Self> for EndpointUri {
    fn from(value: &EndpointUri) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<UriTemplate> for EndpointUri {
    fn from(value: UriTemplate) -> Self {
        Self::UriTemplate(value)
    }
}
impl ::std::convert::From<RuntimeExpression> for EndpointUri {
    fn from(value: RuntimeExpression) -> Self {
        Self::RuntimeExpression(value)
    }
}
#[doc = "Represents an error."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Error\","]
#[doc = "  \"description\": \"Represents an error.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"status\","]
#[doc = "    \"type\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"detail\": {"]
#[doc = "      \"title\": \"ErrorDetails\","]
#[doc = "      \"description\": \"A human-readable explanation specific to this occurrence of the error.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"ExpressionErrorDetails\","]
#[doc = "          \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"LiteralErrorDetails\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"instance\": {"]
#[doc = "      \"title\": \"ErrorInstance\","]
#[doc = "      \"description\": \"A JSON Pointer used to reference the component the error originates from.\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"LiteralErrorInstance\","]
#[doc = "          \"description\": \"The literal error instance.\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"format\": \"json-pointer\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"ExpressionErrorInstance\","]
#[doc = "          \"description\": \"An expression based error instance.\","]
#[doc = "          \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"status\": {"]
#[doc = "      \"title\": \"ErrorStatus\","]
#[doc = "      \"description\": \"The status code generated by the origin for this occurrence of the error.\","]
#[doc = "      \"type\": \"integer\""]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"title\": \"ErrorTitle\","]
#[doc = "      \"description\": \"A short, human-readable summary of the error.\","]
#[doc = "      \"anyOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"ExpressionErrorTitle\","]
#[doc = "          \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"LiteralErrorTitle\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"type\": {"]
#[doc = "      \"title\": \"ErrorType\","]
#[doc = "      \"description\": \"A URI reference that identifies the error type.\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"LiteralErrorType\","]
#[doc = "          \"description\": \"The literal error type.\","]
#[doc = "          \"$ref\": \"#/$defs/uriTemplate\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"ExpressionErrorType\","]
#[doc = "          \"description\": \"An expression based error type.\","]
#[doc = "          \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Error {
    #[doc = "A human-readable explanation specific to this occurrence of the error."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub detail: ::std::option::Option<ErrorDetails>,
    #[doc = "A JSON Pointer used to reference the component the error originates from."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub instance: ::std::option::Option<ErrorInstance>,
    #[doc = "The status code generated by the origin for this occurrence of the error."]
    pub status: i64,
    #[doc = "A short, human-readable summary of the error."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<ErrorTitle>,
    #[doc = "A URI reference that identifies the error type."]
    #[serde(rename = "type")]
    pub type_: ErrorType,
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
#[doc = "A human-readable explanation specific to this occurrence of the error."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ErrorDetails\","]
#[doc = "  \"description\": \"A human-readable explanation specific to this occurrence of the error.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"ExpressionErrorDetails\","]
#[doc = "      \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LiteralErrorDetails\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ErrorDetails {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<RuntimeExpression>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&ErrorDetails> for ErrorDetails {
    fn from(value: &ErrorDetails) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for ErrorDetails {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl ErrorDetails {
    pub fn builder() -> builder::ErrorDetails {
        Default::default()
    }
}
#[doc = "Error filtering base on static values. For error filtering on dynamic values, use catch.when property"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ErrorFilter\","]
#[doc = "  \"description\": \"Error filtering base on static values. For error filtering on dynamic values, use catch.when property\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"minProperties\": 1,"]
#[doc = "  \"properties\": {"]
#[doc = "    \"details\": {"]
#[doc = "      \"description\": \"if present, means this value should be used for filtering\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"instance\": {"]
#[doc = "      \"description\": \"if present, means this value should be used for filtering\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"status\": {"]
#[doc = "      \"description\": \"if present, means this value should be used for filtering\","]
#[doc = "      \"type\": \"integer\""]
#[doc = "    },"]
#[doc = "    \"title\": {"]
#[doc = "      \"description\": \"if present, means this value should be used for filtering\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"type\": {"]
#[doc = "      \"description\": \"if present, means this value should be used for filtering\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ErrorFilter {
    #[doc = "if present, means this value should be used for filtering"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub details: ::std::option::Option<::std::string::String>,
    #[doc = "if present, means this value should be used for filtering"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub instance: ::std::option::Option<::std::string::String>,
    #[doc = "if present, means this value should be used for filtering"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub status: ::std::option::Option<i64>,
    #[doc = "if present, means this value should be used for filtering"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub title: ::std::option::Option<::std::string::String>,
    #[doc = "if present, means this value should be used for filtering"]
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
#[doc = "A JSON Pointer used to reference the component the error originates from."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ErrorInstance\","]
#[doc = "  \"description\": \"A JSON Pointer used to reference the component the error originates from.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"LiteralErrorInstance\","]
#[doc = "      \"description\": \"The literal error instance.\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"format\": \"json-pointer\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExpressionErrorInstance\","]
#[doc = "      \"description\": \"An expression based error instance.\","]
#[doc = "      \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum ErrorInstance {
    LiteralErrorInstance(::std::string::String),
    RuntimeExpression(RuntimeExpression),
}
impl ::std::convert::From<&Self> for ErrorInstance {
    fn from(value: &ErrorInstance) -> Self {
        value.clone()
    }
}
impl ::std::str::FromStr for ErrorInstance {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        if let Ok(v) = value.parse() {
            Ok(Self::LiteralErrorInstance(v))
        } else if let Ok(v) = value.parse() {
            Ok(Self::RuntimeExpression(v))
        } else {
            Err("string conversion failed for all variants".into())
        }
    }
}
impl ::std::convert::TryFrom<&str> for ErrorInstance {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ErrorInstance {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ErrorInstance {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::fmt::Display for ErrorInstance {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match self {
            Self::LiteralErrorInstance(x) => x.fmt(f),
            Self::RuntimeExpression(x) => x.fmt(f),
        }
    }
}
impl ::std::convert::From<RuntimeExpression> for ErrorInstance {
    fn from(value: RuntimeExpression) -> Self {
        Self::RuntimeExpression(value)
    }
}
#[doc = "A short, human-readable summary of the error."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ErrorTitle\","]
#[doc = "  \"description\": \"A short, human-readable summary of the error.\","]
#[doc = "  \"anyOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"ExpressionErrorTitle\","]
#[doc = "      \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LiteralErrorTitle\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ErrorTitle {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<RuntimeExpression>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&ErrorTitle> for ErrorTitle {
    fn from(value: &ErrorTitle) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for ErrorTitle {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl ErrorTitle {
    pub fn builder() -> builder::ErrorTitle {
        Default::default()
    }
}
#[doc = "A URI reference that identifies the error type."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ErrorType\","]
#[doc = "  \"description\": \"A URI reference that identifies the error type.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"LiteralErrorType\","]
#[doc = "      \"description\": \"The literal error type.\","]
#[doc = "      \"$ref\": \"#/$defs/uriTemplate\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"ExpressionErrorType\","]
#[doc = "      \"description\": \"An expression based error type.\","]
#[doc = "      \"$ref\": \"#/$defs/runtimeExpression\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum ErrorType {
    UriTemplate(UriTemplate),
    RuntimeExpression(RuntimeExpression),
}
impl ::std::convert::From<&Self> for ErrorType {
    fn from(value: &ErrorType) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<UriTemplate> for ErrorType {
    fn from(value: UriTemplate) -> Self {
        Self::UriTemplate(value)
    }
}
impl ::std::convert::From<RuntimeExpression> for ErrorType {
    fn from(value: RuntimeExpression) -> Self {
        Self::RuntimeExpression(value)
    }
}
#[doc = "Set the content of the context. ."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Export\","]
#[doc = "  \"description\": \"Set the content of the context. .\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"as\": {"]
#[doc = "      \"title\": \"ExportAs\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to export the output data to the context.\","]
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
#[doc = "      \"description\": \"The schema used to describe and validate the workflow context.\","]
#[doc = "      \"$ref\": \"#/$defs/schema\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Export {
    #[doc = "A runtime expression, if any, used to export the output data to the context."]
    #[serde(
        rename = "as",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub as_: ::std::option::Option<ExportAs>,
    #[doc = "The schema used to describe and validate the workflow context."]
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
#[doc = "A runtime expression, if any, used to export the output data to the context."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ExportAs\","]
#[doc = "  \"description\": \"A runtime expression, if any, used to export the output data to the context.\","]
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
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
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
#[doc = "Represents an external resource."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ExternalResource\","]
#[doc = "  \"description\": \"Represents an external resource.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"endpoint\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"endpoint\": {"]
#[doc = "      \"title\": \"ExternalResourceEndpoint\","]
#[doc = "      \"description\": \"The endpoint of the external resource.\","]
#[doc = "      \"$ref\": \"#/$defs/endpoint\""]
#[doc = "    },"]
#[doc = "    \"name\": {"]
#[doc = "      \"title\": \"ExternalResourceName\","]
#[doc = "      \"description\": \"The name of the external resource, if any.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ExternalResource {
    #[doc = "The endpoint of the external resource."]
    pub endpoint: Endpoint,
    #[doc = "The name of the external resource, if any."]
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
#[doc = "Represents different transition options for a workflow."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"FlowDirective\","]
#[doc = "  \"description\": \"Represents different transition options for a workflow.\","]
#[doc = "  \"anyOf\": ["]
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
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct FlowDirective {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<FlowDirectiveEnum>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&FlowDirective> for FlowDirective {
    fn from(value: &FlowDirective) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for FlowDirective {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl FlowDirective {
    pub fn builder() -> builder::FlowDirective {
        Default::default()
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
impl ::std::default::Default for FlowDirectiveEnum {
    fn default() -> Self {
        FlowDirectiveEnum::Continue
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
#[doc = "    \"do\": {"]
#[doc = "      \"title\": \"ForTaskDo\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"for\": {"]
#[doc = "      \"title\": \"ForTaskConfiguration\","]
#[doc = "      \"description\": \"The definition of the loop that iterates over a range of values.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"required\": ["]
#[doc = "        \"in\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"at\": {"]
#[doc = "          \"title\": \"ForAt\","]
#[doc = "          \"description\": \"The name of the variable used to store the index of the current item being enumerated.\","]
#[doc = "          \"default\": \"index\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"each\": {"]
#[doc = "          \"title\": \"ForEach\","]
#[doc = "          \"description\": \"The name of the variable used to store the current item being enumerated.\","]
#[doc = "          \"default\": \"item\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"in\": {"]
#[doc = "          \"title\": \"ForIn\","]
#[doc = "          \"description\": \"A runtime expression used to get the collection to enumerate.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"while\": {"]
#[doc = "      \"title\": \"While\","]
#[doc = "      \"description\": \"A runtime expression that represents the condition, if any, that must be met for the iteration to continue.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ForTask {
    #[serde(rename = "do")]
    pub do_: TaskList,
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[serde(rename = "for")]
    pub for_: ForTaskConfiguration,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "The flow directive to be performed upon completion of the task."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
    #[doc = "A runtime expression that represents the condition, if any, that must be met for the iteration to continue."]
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
#[doc = "The definition of the loop that iterates over a range of values."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ForTaskConfiguration\","]
#[doc = "  \"description\": \"The definition of the loop that iterates over a range of values.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"in\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"at\": {"]
#[doc = "      \"title\": \"ForAt\","]
#[doc = "      \"description\": \"The name of the variable used to store the index of the current item being enumerated.\","]
#[doc = "      \"default\": \"index\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"each\": {"]
#[doc = "      \"title\": \"ForEach\","]
#[doc = "      \"description\": \"The name of the variable used to store the current item being enumerated.\","]
#[doc = "      \"default\": \"item\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"in\": {"]
#[doc = "      \"title\": \"ForIn\","]
#[doc = "      \"description\": \"A runtime expression used to get the collection to enumerate.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ForTaskConfiguration {
    #[doc = "The name of the variable used to store the index of the current item being enumerated."]
    #[serde(default = "defaults::for_task_configuration_at")]
    pub at: ::std::string::String,
    #[doc = "The name of the variable used to store the current item being enumerated."]
    #[serde(default = "defaults::for_task_configuration_each")]
    pub each: ::std::string::String,
    #[doc = "A runtime expression used to get the collection to enumerate."]
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
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"fork\": {"]
#[doc = "      \"title\": \"ForkTaskConfiguration\","]
#[doc = "      \"description\": \"The configuration of the branches to perform concurrently.\","]
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
#[doc = "          \"description\": \"Indicates whether or not the concurrent tasks are racing against each other, with a single possible winner, which sets the composite task's output.\","]
#[doc = "          \"default\": false,"]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ForkTask {
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    pub fork: ForkTaskConfiguration,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "The flow directive to be performed upon completion of the task."]
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
#[doc = "The configuration of the branches to perform concurrently."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ForkTaskConfiguration\","]
#[doc = "  \"description\": \"The configuration of the branches to perform concurrently.\","]
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
#[doc = "      \"description\": \"Indicates whether or not the concurrent tasks are racing against each other, with a single possible winner, which sets the composite task's output.\","]
#[doc = "      \"default\": false,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ForkTaskConfiguration {
    pub branches: TaskList,
    #[doc = "Indicates whether or not the concurrent tasks are racing against each other, with a single possible winner, which sets the composite task's output."]
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
#[doc = "The configuration of the function to run by runner(runtime)."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Function\","]
#[doc = "  \"description\": \"The configuration of the function to run by runner(runtime).\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"arguments\","]
#[doc = "    \"runnerName\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"arguments\": {"]
#[doc = "      \"title\": \"FunctionArguments\","]
#[doc = "      \"description\": \"A key/value mapping of arguments to use when running the function. Runtime expressions are supported for value transformation.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"options\": {"]
#[doc = "      \"title\": \"FunctionOptions\","]
#[doc = "      \"description\": \"The options to use when running the configured function.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"properties\": {"]
#[doc = "        \"broadcastResultsToListener\": {"]
#[doc = "          \"title\": \"BroadcastResultsToListener\","]
#[doc = "          \"description\": \"Whether to broadcast results to listeners.\","]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        },"]
#[doc = "        \"channel\": {"]
#[doc = "          \"title\": \"FunctionChannel\","]
#[doc = "          \"description\": \"The channel to use when running the function. (Channel controls execution concurrency)\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"retry\": {"]
#[doc = "          \"title\": \"RetryPolicyDefinition\","]
#[doc = "          \"description\": \"The retry policy to use, if any, when catching errors.\","]
#[doc = "          \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "        },"]
#[doc = "        \"storeFailure\": {"]
#[doc = "          \"title\": \"StoreFailureResult\","]
#[doc = "          \"description\": \"Whether to store failure results to database.\","]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        },"]
#[doc = "        \"storeSuccess\": {"]
#[doc = "          \"title\": \"StoreSuccessResult\","]
#[doc = "          \"description\": \"Whether to store successful results to database.\","]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        },"]
#[doc = "        \"useStatic\": {"]
#[doc = "          \"title\": \"UseStaticFunction\","]
#[doc = "          \"description\": \"Whether to use a static function (persist in database, pool initialized function).\","]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        },"]
#[doc = "        \"withBackup\": {"]
#[doc = "          \"title\": \"FunctionWithBackup\","]
#[doc = "          \"description\": \"Whether to backup the function call (queue) to database when queueing and running the function.\","]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"runnerName\": {"]
#[doc = "      \"title\": \"RunnerName\","]
#[doc = "      \"description\": \"The name of the runtime environment that executes this function (e.g., COMMAND, HTTP, GRPC, PYTHON_COMMAND)\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"settings\": {"]
#[doc = "      \"title\": \"InitializeSettings\","]
#[doc = "      \"description\": \"The initialization settings, if any. Runtime expression can be used to transform each value (not keys, no mixed plain text).\","]
#[doc = "      \"type\": \"object\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct Function {
    #[doc = "A key/value mapping of arguments to use when running the function. Runtime expressions are supported for value transformation."]
    pub arguments: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub options: ::std::option::Option<FunctionOptions>,
    #[doc = "The name of the runtime environment that executes this function (e.g., COMMAND, HTTP, GRPC, PYTHON_COMMAND)"]
    #[serde(rename = "runnerName")]
    pub runner_name: ::std::string::String,
    #[doc = "The initialization settings, if any. Runtime expression can be used to transform each value (not keys, no mixed plain text)."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub settings: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
}
impl ::std::convert::From<&Function> for Function {
    fn from(value: &Function) -> Self {
        value.clone()
    }
}
impl Function {
    pub fn builder() -> builder::Function {
        Default::default()
    }
}
#[doc = "The options to use when running the configured function."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"FunctionOptions\","]
#[doc = "  \"description\": \"The options to use when running the configured function.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"broadcastResultsToListener\": {"]
#[doc = "      \"title\": \"BroadcastResultsToListener\","]
#[doc = "      \"description\": \"Whether to broadcast results to listeners.\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"channel\": {"]
#[doc = "      \"title\": \"FunctionChannel\","]
#[doc = "      \"description\": \"The channel to use when running the function. (Channel controls execution concurrency)\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"retry\": {"]
#[doc = "      \"title\": \"RetryPolicyDefinition\","]
#[doc = "      \"description\": \"The retry policy to use, if any, when catching errors.\","]
#[doc = "      \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "    },"]
#[doc = "    \"storeFailure\": {"]
#[doc = "      \"title\": \"StoreFailureResult\","]
#[doc = "      \"description\": \"Whether to store failure results to database.\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"storeSuccess\": {"]
#[doc = "      \"title\": \"StoreSuccessResult\","]
#[doc = "      \"description\": \"Whether to store successful results to database.\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"useStatic\": {"]
#[doc = "      \"title\": \"UseStaticFunction\","]
#[doc = "      \"description\": \"Whether to use a static function (persist in database, pool initialized function).\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"withBackup\": {"]
#[doc = "      \"title\": \"FunctionWithBackup\","]
#[doc = "      \"description\": \"Whether to backup the function call (queue) to database when queueing and running the function.\","]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct FunctionOptions {
    #[doc = "Whether to broadcast results to listeners."]
    #[serde(
        rename = "broadcastResultsToListener",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub broadcast_results_to_listener: ::std::option::Option<bool>,
    #[doc = "The channel to use when running the function. (Channel controls execution concurrency)"]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub channel: ::std::option::Option<::std::string::String>,
    #[doc = "The retry policy to use, if any, when catching errors."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub retry: ::std::option::Option<RetryPolicy>,
    #[doc = "Whether to store failure results to database."]
    #[serde(
        rename = "storeFailure",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub store_failure: ::std::option::Option<bool>,
    #[doc = "Whether to store successful results to database."]
    #[serde(
        rename = "storeSuccess",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub store_success: ::std::option::Option<bool>,
    #[doc = "Whether to use a static function (persist in database, pool initialized function)."]
    #[serde(
        rename = "useStatic",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub use_static: ::std::option::Option<bool>,
    #[doc = "Whether to backup the function call (queue) to database when queueing and running the function."]
    #[serde(
        rename = "withBackup",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub with_backup: ::std::option::Option<bool>,
}
impl ::std::convert::From<&FunctionOptions> for FunctionOptions {
    fn from(value: &FunctionOptions) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for FunctionOptions {
    fn default() -> Self {
        Self {
            broadcast_results_to_listener: Default::default(),
            channel: Default::default(),
            retry: Default::default(),
            store_failure: Default::default(),
            store_success: Default::default(),
            use_static: Default::default(),
            with_backup: Default::default(),
        }
    }
}
impl FunctionOptions {
    pub fn builder() -> builder::FunctionOptions {
        Default::default()
    }
}
#[doc = "Configures the input of a workflow or task."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Input\","]
#[doc = "  \"description\": \"Configures the input of a workflow or task.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"from\": {"]
#[doc = "      \"title\": \"InputFrom\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to mutate and/or filter the input of the workflow or task.\","]
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
#[doc = "      \"description\": \"The schema used to describe and validate the input of the workflow or task.\","]
#[doc = "      \"$ref\": \"#/$defs/schema\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Input {
    #[doc = "A runtime expression, if any, used to mutate and/or filter the input of the workflow or task."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub from: ::std::option::Option<InputFrom>,
    #[doc = "The schema used to describe and validate the input of the workflow or task."]
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
#[doc = "A runtime expression, if any, used to mutate and/or filter the input of the workflow or task."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"InputFrom\","]
#[doc = "  \"description\": \"A runtime expression, if any, used to mutate and/or filter the input of the workflow or task.\","]
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
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
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
#[doc = "Configures the output of a workflow or task."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Output\","]
#[doc = "  \"description\": \"Configures the output of a workflow or task.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"as\": {"]
#[doc = "      \"title\": \"OutputAs\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to mutate and/or filter the output of the workflow or task.\","]
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
#[doc = "      \"description\": \"The schema used to describe and validate the output of the workflow or task.\","]
#[doc = "      \"$ref\": \"#/$defs/schema\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Output {
    #[doc = "A runtime expression, if any, used to mutate and/or filter the output of the workflow or task."]
    #[serde(
        rename = "as",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub as_: ::std::option::Option<OutputAs>,
    #[doc = "The schema used to describe and validate the output of the workflow or task."]
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
#[doc = "A runtime expression, if any, used to mutate and/or filter the output of the workflow or task."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"OutputAs\","]
#[doc = "  \"description\": \"A runtime expression, if any, used to mutate and/or filter the output of the workflow or task.\","]
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
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
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
#[doc = "A plain string."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"PlainString\","]
#[doc = "  \"description\": \"A plain string.\","]
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
#[doc = "The object returned by a run task when its return type has been set 'all'."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ProcessResult\","]
#[doc = "  \"description\": \"The object returned by a run task when its return type has been set 'all'.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"code\","]
#[doc = "    \"stderr\","]
#[doc = "    \"stdout\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"code\": {"]
#[doc = "      \"title\": \"ProcessExitCode\","]
#[doc = "      \"description\": \"The process's exit code.\","]
#[doc = "      \"type\": \"integer\""]
#[doc = "    },"]
#[doc = "    \"stderr\": {"]
#[doc = "      \"title\": \"ProcessStandardError\","]
#[doc = "      \"description\": \"The content of the process's STDERR.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"stdout\": {"]
#[doc = "      \"title\": \"ProcessStandardOutput\","]
#[doc = "      \"description\": \"The content of the process's STDOUT.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct ProcessResult {
    #[doc = "The process's exit code."]
    pub code: i64,
    #[doc = "The content of the process's STDERR."]
    pub stderr: ::std::string::String,
    #[doc = "The content of the process's STDOUT."]
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
#[doc = "Configures the output of the process."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"ProcessReturnType\","]
#[doc = "  \"description\": \"Configures the output of the process.\","]
#[doc = "  \"default\": \"stdout\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"enum\": ["]
#[doc = "    \"stdout\","]
#[doc = "    \"stderr\","]
#[doc = "    \"code\","]
#[doc = "    \"all\","]
#[doc = "    \"none\""]
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
pub enum ProcessReturnType {
    #[serde(rename = "stdout")]
    Stdout,
    #[serde(rename = "stderr")]
    Stderr,
    #[serde(rename = "code")]
    Code,
    #[serde(rename = "all")]
    All,
    #[serde(rename = "none")]
    None,
}
impl ::std::convert::From<&Self> for ProcessReturnType {
    fn from(value: &ProcessReturnType) -> Self {
        value.clone()
    }
}
impl ::std::fmt::Display for ProcessReturnType {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Self::Stdout => write!(f, "stdout"),
            Self::Stderr => write!(f, "stderr"),
            Self::Code => write!(f, "code"),
            Self::All => write!(f, "all"),
            Self::None => write!(f, "none"),
        }
    }
}
impl ::std::str::FromStr for ProcessReturnType {
    type Err = self::error::ConversionError;
    fn from_str(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        match value {
            "stdout" => Ok(Self::Stdout),
            "stderr" => Ok(Self::Stderr),
            "code" => Ok(Self::Code),
            "all" => Ok(Self::All),
            "none" => Ok(Self::None),
            _ => Err("invalid value".into()),
        }
    }
}
impl ::std::convert::TryFrom<&str> for ProcessReturnType {
    type Error = self::error::ConversionError;
    fn try_from(value: &str) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<&::std::string::String> for ProcessReturnType {
    type Error = self::error::ConversionError;
    fn try_from(
        value: &::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::convert::TryFrom<::std::string::String> for ProcessReturnType {
    type Error = self::error::ConversionError;
    fn try_from(
        value: ::std::string::String,
    ) -> ::std::result::Result<Self, self::error::ConversionError> {
        value.parse()
    }
}
impl ::std::default::Default for ProcessReturnType {
    fn default() -> Self {
        ProcessReturnType::Stdout
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
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"raise\": {"]
#[doc = "      \"title\": \"RaiseTaskConfiguration\","]
#[doc = "      \"description\": \"The definition of the error to raise.\","]
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
#[doc = "              \"description\": \"Defines the error to raise.\","]
#[doc = "              \"$ref\": \"#/$defs/error\""]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"RaiseErrorReference\","]
#[doc = "              \"description\": \"The name of the error to raise\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct RaiseTask {
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    pub raise: RaiseTaskConfiguration,
    #[doc = "The flow directive to be performed upon completion of the task."]
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
#[doc = "The definition of the error to raise."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RaiseTaskConfiguration\","]
#[doc = "  \"description\": \"The definition of the error to raise.\","]
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
#[doc = "          \"description\": \"Defines the error to raise.\","]
#[doc = "          \"$ref\": \"#/$defs/error\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"RaiseErrorReference\","]
#[doc = "          \"description\": \"The name of the error to raise\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
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
#[doc = "      \"description\": \"Defines the error to raise.\","]
#[doc = "      \"$ref\": \"#/$defs/error\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"RaiseErrorReference\","]
#[doc = "      \"description\": \"The name of the error to raise\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
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
#[doc = "The retry duration backoff."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RetryBackoff\","]
#[doc = "  \"description\": \"The retry duration backoff.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"ConstantBackoff\","]
#[doc = "      \"required\": ["]
#[doc = "        \"constant\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"constant\": {"]
#[doc = "          \"description\": \"The definition of the constant backoff to use, if any. value is empty object.\","]
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
#[doc = "          \"description\": \"The definition of the exponential backoff to use, if any. value is empty object.\","]
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
#[doc = "          \"description\": \"The definition of the linear backoff to use, if any. value is empty object.\","]
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
#[doc = "The retry limit, if any."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RetryLimit\","]
#[doc = "  \"description\": \"The retry limit, if any.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"attempt\": {"]
#[doc = "      \"title\": \"RetryLimitAttempt\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"properties\": {"]
#[doc = "        \"count\": {"]
#[doc = "          \"title\": \"RetryLimitAttemptCount\","]
#[doc = "          \"description\": \"The maximum amount of retry attempts, if any.\","]
#[doc = "          \"type\": \"integer\""]
#[doc = "        },"]
#[doc = "        \"duration\": {"]
#[doc = "          \"title\": \"RetryLimitAttemptDuration\","]
#[doc = "          \"description\": \"The maximum duration for each retry attempt.\","]
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
#[doc = "      \"description\": \"The maximum amount of retry attempts, if any.\","]
#[doc = "      \"type\": \"integer\""]
#[doc = "    },"]
#[doc = "    \"duration\": {"]
#[doc = "      \"title\": \"RetryLimitAttemptDuration\","]
#[doc = "      \"description\": \"The maximum duration for each retry attempt.\","]
#[doc = "      \"$ref\": \"#/$defs/duration\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug, PartialEq)]
pub struct RetryLimitAttempt {
    #[doc = "The maximum amount of retry attempts, if any."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub count: ::std::option::Option<i64>,
    #[doc = "The maximum duration for each retry attempt."]
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
#[doc = "Defines a retry policy."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RetryPolicy\","]
#[doc = "  \"description\": \"Defines a retry policy.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"backoff\": {"]
#[doc = "      \"title\": \"RetryBackoff\","]
#[doc = "      \"description\": \"The retry duration backoff.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"ConstantBackoff\","]
#[doc = "          \"required\": ["]
#[doc = "            \"constant\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"constant\": {"]
#[doc = "              \"description\": \"The definition of the constant backoff to use, if any. value is empty object.\","]
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
#[doc = "              \"description\": \"The definition of the exponential backoff to use, if any. value is empty object.\","]
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
#[doc = "              \"description\": \"The definition of the linear backoff to use, if any. value is empty object.\","]
#[doc = "              \"type\": \"object\""]
#[doc = "            }"]
#[doc = "          }"]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"delay\": {"]
#[doc = "      \"title\": \"RetryDelay\","]
#[doc = "      \"description\": \"The duration to wait between retry attempts.\","]
#[doc = "      \"$ref\": \"#/$defs/duration\""]
#[doc = "    },"]
#[doc = "    \"limit\": {"]
#[doc = "      \"title\": \"RetryLimit\","]
#[doc = "      \"description\": \"The retry limit, if any.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"properties\": {"]
#[doc = "        \"attempt\": {"]
#[doc = "          \"title\": \"RetryLimitAttempt\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"properties\": {"]
#[doc = "            \"count\": {"]
#[doc = "              \"title\": \"RetryLimitAttemptCount\","]
#[doc = "              \"description\": \"The maximum amount of retry attempts, if any.\","]
#[doc = "              \"type\": \"integer\""]
#[doc = "            },"]
#[doc = "            \"duration\": {"]
#[doc = "              \"title\": \"RetryLimitAttemptDuration\","]
#[doc = "              \"description\": \"The maximum duration for each retry attempt.\","]
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
    #[doc = "The retry duration backoff."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub backoff: ::std::option::Option<RetryBackoff>,
    #[doc = "The duration to wait between retry attempts."]
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
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"run\": {"]
#[doc = "      \"title\": \"RunTaskConfiguration\","]
#[doc = "      \"description\": \"The configuration of the process to execute.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"RunFunction\","]
#[doc = "          \"description\": \"Executes a function using a specified runtime environment (runner)\","]
#[doc = "          \"required\": ["]
#[doc = "            \"function\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"function\": {"]
#[doc = "              \"title\": \"Function\","]
#[doc = "              \"description\": \"The configuration of the function to run by runner(runtime).\","]
#[doc = "              \"type\": \"object\","]
#[doc = "              \"required\": ["]
#[doc = "                \"arguments\","]
#[doc = "                \"runnerName\""]
#[doc = "              ],"]
#[doc = "              \"properties\": {"]
#[doc = "                \"arguments\": {"]
#[doc = "                  \"title\": \"FunctionArguments\","]
#[doc = "                  \"description\": \"A key/value mapping of arguments to use when running the function. Runtime expressions are supported for value transformation.\","]
#[doc = "                  \"type\": \"object\","]
#[doc = "                  \"additionalProperties\": true"]
#[doc = "                },"]
#[doc = "                \"options\": {"]
#[doc = "                  \"title\": \"FunctionOptions\","]
#[doc = "                  \"description\": \"The options to use when running the configured function.\","]
#[doc = "                  \"type\": \"object\","]
#[doc = "                  \"properties\": {"]
#[doc = "                    \"broadcastResultsToListener\": {"]
#[doc = "                      \"title\": \"BroadcastResultsToListener\","]
#[doc = "                      \"description\": \"Whether to broadcast results to listeners.\","]
#[doc = "                      \"type\": \"boolean\""]
#[doc = "                    },"]
#[doc = "                    \"channel\": {"]
#[doc = "                      \"title\": \"FunctionChannel\","]
#[doc = "                      \"description\": \"The channel to use when running the function. (Channel controls execution concurrency)\","]
#[doc = "                      \"type\": \"string\""]
#[doc = "                    },"]
#[doc = "                    \"retry\": {"]
#[doc = "                      \"title\": \"RetryPolicyDefinition\","]
#[doc = "                      \"description\": \"The retry policy to use, if any, when catching errors.\","]
#[doc = "                      \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "                    },"]
#[doc = "                    \"storeFailure\": {"]
#[doc = "                      \"title\": \"StoreFailureResult\","]
#[doc = "                      \"description\": \"Whether to store failure results to database.\","]
#[doc = "                      \"type\": \"boolean\""]
#[doc = "                    },"]
#[doc = "                    \"storeSuccess\": {"]
#[doc = "                      \"title\": \"StoreSuccessResult\","]
#[doc = "                      \"description\": \"Whether to store successful results to database.\","]
#[doc = "                      \"type\": \"boolean\""]
#[doc = "                    },"]
#[doc = "                    \"useStatic\": {"]
#[doc = "                      \"title\": \"UseStaticFunction\","]
#[doc = "                      \"description\": \"Whether to use a static function (persist in database, pool initialized function).\","]
#[doc = "                      \"type\": \"boolean\""]
#[doc = "                    },"]
#[doc = "                    \"withBackup\": {"]
#[doc = "                      \"title\": \"FunctionWithBackup\","]
#[doc = "                      \"description\": \"Whether to backup the function call (queue) to database when queueing and running the function.\","]
#[doc = "                      \"type\": \"boolean\""]
#[doc = "                    }"]
#[doc = "                  }"]
#[doc = "                },"]
#[doc = "                \"runnerName\": {"]
#[doc = "                  \"title\": \"RunnerName\","]
#[doc = "                  \"description\": \"The name of the runtime environment that executes this function (e.g., COMMAND, HTTP, GRPC, PYTHON_COMMAND)\","]
#[doc = "                  \"type\": \"string\""]
#[doc = "                },"]
#[doc = "                \"settings\": {"]
#[doc = "                  \"title\": \"InitializeSettings\","]
#[doc = "                  \"description\": \"The initialization settings, if any. Runtime expression can be used to transform each value (not keys, no mixed plain text).\","]
#[doc = "                  \"type\": \"object\""]
#[doc = "                }"]
#[doc = "              },"]
#[doc = "              \"unevaluatedProperties\": false"]
#[doc = "            }"]
#[doc = "          }"]
#[doc = "        }"]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"await\": {"]
#[doc = "          \"title\": \"AwaitProcessCompletion\","]
#[doc = "          \"description\": \"Whether to await the process completion before continuing.\","]
#[doc = "          \"default\": true,"]
#[doc = "          \"type\": \"boolean\""]
#[doc = "        },"]
#[doc = "        \"return\": {"]
#[doc = "          \"title\": \"ProcessReturnType\","]
#[doc = "          \"description\": \"Configures the output of the process.\","]
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
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct RunTask {
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    pub run: RunTaskConfiguration,
    #[doc = "The flow directive to be performed upon completion of the task."]
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
#[doc = "The configuration of the process to execute."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RunTaskConfiguration\","]
#[doc = "  \"description\": \"The configuration of the process to execute.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"RunFunction\","]
#[doc = "      \"description\": \"Executes a function using a specified runtime environment (runner)\","]
#[doc = "      \"required\": ["]
#[doc = "        \"function\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"function\": {"]
#[doc = "          \"title\": \"Function\","]
#[doc = "          \"description\": \"The configuration of the function to run by runner(runtime).\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"required\": ["]
#[doc = "            \"arguments\","]
#[doc = "            \"runnerName\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"arguments\": {"]
#[doc = "              \"title\": \"FunctionArguments\","]
#[doc = "              \"description\": \"A key/value mapping of arguments to use when running the function. Runtime expressions are supported for value transformation.\","]
#[doc = "              \"type\": \"object\","]
#[doc = "              \"additionalProperties\": true"]
#[doc = "            },"]
#[doc = "            \"options\": {"]
#[doc = "              \"title\": \"FunctionOptions\","]
#[doc = "              \"description\": \"The options to use when running the configured function.\","]
#[doc = "              \"type\": \"object\","]
#[doc = "              \"properties\": {"]
#[doc = "                \"broadcastResultsToListener\": {"]
#[doc = "                  \"title\": \"BroadcastResultsToListener\","]
#[doc = "                  \"description\": \"Whether to broadcast results to listeners.\","]
#[doc = "                  \"type\": \"boolean\""]
#[doc = "                },"]
#[doc = "                \"channel\": {"]
#[doc = "                  \"title\": \"FunctionChannel\","]
#[doc = "                  \"description\": \"The channel to use when running the function. (Channel controls execution concurrency)\","]
#[doc = "                  \"type\": \"string\""]
#[doc = "                },"]
#[doc = "                \"retry\": {"]
#[doc = "                  \"title\": \"RetryPolicyDefinition\","]
#[doc = "                  \"description\": \"The retry policy to use, if any, when catching errors.\","]
#[doc = "                  \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "                },"]
#[doc = "                \"storeFailure\": {"]
#[doc = "                  \"title\": \"StoreFailureResult\","]
#[doc = "                  \"description\": \"Whether to store failure results to database.\","]
#[doc = "                  \"type\": \"boolean\""]
#[doc = "                },"]
#[doc = "                \"storeSuccess\": {"]
#[doc = "                  \"title\": \"StoreSuccessResult\","]
#[doc = "                  \"description\": \"Whether to store successful results to database.\","]
#[doc = "                  \"type\": \"boolean\""]
#[doc = "                },"]
#[doc = "                \"useStatic\": {"]
#[doc = "                  \"title\": \"UseStaticFunction\","]
#[doc = "                  \"description\": \"Whether to use a static function (persist in database, pool initialized function).\","]
#[doc = "                  \"type\": \"boolean\""]
#[doc = "                },"]
#[doc = "                \"withBackup\": {"]
#[doc = "                  \"title\": \"FunctionWithBackup\","]
#[doc = "                  \"description\": \"Whether to backup the function call (queue) to database when queueing and running the function.\","]
#[doc = "                  \"type\": \"boolean\""]
#[doc = "                }"]
#[doc = "              }"]
#[doc = "            },"]
#[doc = "            \"runnerName\": {"]
#[doc = "              \"title\": \"RunnerName\","]
#[doc = "              \"description\": \"The name of the runtime environment that executes this function (e.g., COMMAND, HTTP, GRPC, PYTHON_COMMAND)\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            },"]
#[doc = "            \"settings\": {"]
#[doc = "              \"title\": \"InitializeSettings\","]
#[doc = "              \"description\": \"The initialization settings, if any. Runtime expression can be used to transform each value (not keys, no mixed plain text).\","]
#[doc = "              \"type\": \"object\""]
#[doc = "            }"]
#[doc = "          },"]
#[doc = "          \"unevaluatedProperties\": false"]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"await\": {"]
#[doc = "      \"title\": \"AwaitProcessCompletion\","]
#[doc = "      \"description\": \"Whether to await the process completion before continuing.\","]
#[doc = "      \"default\": true,"]
#[doc = "      \"type\": \"boolean\""]
#[doc = "    },"]
#[doc = "    \"return\": {"]
#[doc = "      \"title\": \"ProcessReturnType\","]
#[doc = "      \"description\": \"Configures the output of the process.\","]
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
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct RunTaskConfiguration {
    #[doc = "Whether to await the process completion before continuing."]
    #[serde(rename = "await", default = "defaults::default_bool::<true>")]
    pub await_: bool,
    pub function: Function,
    #[doc = "Configures the output of the process."]
    #[serde(rename = "return", default = "defaults::run_task_configuration_return")]
    pub return_: ProcessReturnType,
}
impl ::std::convert::From<&RunTaskConfiguration> for RunTaskConfiguration {
    fn from(value: &RunTaskConfiguration) -> Self {
        value.clone()
    }
}
impl RunTaskConfiguration {
    pub fn builder() -> builder::RunTaskConfiguration {
        Default::default()
    }
}
#[doc = "A runtime expression."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"RuntimeExpression\","]
#[doc = "  \"description\": \"A runtime expression.\","]
#[doc = "  \"type\": \"string\","]
#[doc = "  \"oneof\": ["]
#[doc = "    {"]
#[doc = "      \"description\": \"A jq expression.\","]
#[doc = "      \"pattern\": \"^\\\\$\\\\{.+\\\\}$\","]
#[doc = "      \"title\": \"JqExpression\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"description\": \"A liquid template expression.\","]
#[doc = "      \"pattern\": \"^\\\\$\\\\$\\\\{\\\\{[\\\\s\\\\S]+\\\\}\\\\}$\","]
#[doc = "      \"title\": \"LiquidTemplate\""]
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
pub struct RuntimeExpression(pub ::std::string::String);
impl ::std::ops::Deref for RuntimeExpression {
    type Target = ::std::string::String;
    fn deref(&self) -> &::std::string::String {
        &self.0
    }
}
impl ::std::convert::From<RuntimeExpression> for ::std::string::String {
    fn from(value: RuntimeExpression) -> Self {
        value.0
    }
}
impl ::std::convert::From<&RuntimeExpression> for RuntimeExpression {
    fn from(value: &RuntimeExpression) -> Self {
        value.clone()
    }
}
impl ::std::convert::From<::std::string::String> for RuntimeExpression {
    fn from(value: ::std::string::String) -> Self {
        Self(value)
    }
}
impl ::std::str::FromStr for RuntimeExpression {
    type Err = ::std::convert::Infallible;
    fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
        Ok(Self(value.to_string()))
    }
}
impl ::std::fmt::Display for RuntimeExpression {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}
#[doc = "Represents the definition of a schema."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Schema\","]
#[doc = "  \"description\": \"Represents the definition of a schema.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"title\": \"SchemaInline\","]
#[doc = "      \"required\": ["]
#[doc = "        \"document\""]
#[doc = "      ],"]
#[doc = "      \"properties\": {"]
#[doc = "        \"document\": {"]
#[doc = "          \"description\": \"The schema's inline definition.\""]
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
#[doc = "          \"description\": \"The schema's external resource.\","]
#[doc = "          \"$ref\": \"#/$defs/externalResource\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"format\": {"]
#[doc = "      \"title\": \"SchemaFormat\","]
#[doc = "      \"description\": \"The schema's format. Defaults to 'json'. The (optional) version of the format can be set using `{format}:{version}`.\","]
#[doc = "      \"default\": \"json\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Schema {
    Variant0 {
        #[doc = "The schema's inline definition."]
        document: ::serde_json::Value,
        #[doc = "The schema's format. Defaults to 'json'. The (optional) version of the format can be set using `{format}:{version}`."]
        #[serde(default = "defaults::schema_variant0_format")]
        format: ::std::string::String,
    },
    Variant1 {
        #[doc = "The schema's format. Defaults to 'json'. The (optional) version of the format can be set using `{format}:{version}`."]
        #[serde(default = "defaults::schema_variant1_format")]
        format: ::std::string::String,
        #[doc = "The schema's external resource."]
        resource: ExternalResource,
    },
}
impl ::std::convert::From<&Self> for Schema {
    fn from(value: &Schema) -> Self {
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
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"set\": {"]
#[doc = "      \"title\": \"SetTaskConfiguration\","]
#[doc = "      \"description\": \"The data to set.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"minProperties\": 1,"]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SetTask {
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "The data to set."]
    pub set: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "The flow directive to be performed upon completion of the task."]
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
#[doc = "The definition of a case within a switch task, defining a condition and corresponding tasks to execute if the condition is met."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"SwitchCase\","]
#[doc = "  \"description\": \"The definition of a case within a switch task, defining a condition and corresponding tasks to execute if the condition is met.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"then\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"SwitchCaseOutcome\","]
#[doc = "      \"description\": \"The flow directive to execute when the case matches.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"when\": {"]
#[doc = "      \"title\": \"SwitchCaseCondition\","]
#[doc = "      \"description\": \"A runtime expression used to determine whether or not the case matches.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SwitchCase {
    #[doc = "The flow directive to execute when the case matches."]
    pub then: FlowDirective,
    #[doc = "A runtime expression used to determine whether or not the case matches."]
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
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"switch\": {"]
#[doc = "      \"title\": \"SwitchTaskConfiguration\","]
#[doc = "      \"description\": \"The definition of the switch to use.\","]
#[doc = "      \"type\": \"array\","]
#[doc = "      \"items\": {"]
#[doc = "        \"title\": \"SwitchItem\","]
#[doc = "        \"type\": \"object\","]
#[doc = "        \"maxProperties\": 1,"]
#[doc = "        \"minProperties\": 1,"]
#[doc = "        \"additionalProperties\": {"]
#[doc = "          \"title\": \"SwitchCase\","]
#[doc = "          \"description\": \"The definition of a case within a switch task, defining a condition and corresponding tasks to execute if the condition is met.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"required\": ["]
#[doc = "            \"then\""]
#[doc = "          ],"]
#[doc = "          \"properties\": {"]
#[doc = "            \"then\": {"]
#[doc = "              \"title\": \"SwitchCaseOutcome\","]
#[doc = "              \"description\": \"The flow directive to execute when the case matches.\","]
#[doc = "              \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "            },"]
#[doc = "            \"when\": {"]
#[doc = "              \"title\": \"SwitchCaseCondition\","]
#[doc = "              \"description\": \"A runtime expression used to determine whether or not the case matches.\","]
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
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct SwitchTask {
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "The definition of the switch to use."]
    pub switch: ::std::vec::Vec<::std::collections::HashMap<::std::string::String, SwitchCase>>,
    #[doc = "The flow directive to be performed upon completion of the task."]
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
#[doc = "A discrete unit of work that contributes to achieving the overall objectives defined by the workflow."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Task\","]
#[doc = "  \"description\": \"A discrete unit of work that contributes to achieving the overall objectives defined by the workflow.\","]
#[doc = "  \"oneOf\": ["]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/callTask\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"$ref\": \"#/$defs/doTask\""]
#[doc = "    },"]
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
#[doc = "      \"$ref\": \"#/$defs/waitTask\""]
#[doc = "    }"]
#[doc = "  ],"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Task {
    CallTask(CallTask),
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
impl ::std::convert::From<CallTask> for Task {
    fn from(value: CallTask) -> Self {
        Self::CallTask(value)
    }
}
impl ::std::convert::From<DoTask> for Task {
    fn from(value: DoTask) -> Self {
        Self::DoTask(value)
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
impl ::std::convert::From<WaitTask> for Task {
    fn from(value: WaitTask) -> Self {
        Self::WaitTask(value)
    }
}
#[doc = "An object inherited by all tasks."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"TaskBase\","]
#[doc = "  \"description\": \"An object inherited by all tasks.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TaskBase {
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "The flow directive to be performed upon completion of the task."]
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
#[doc = "List of named tasks to perform."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"TaskList\","]
#[doc = "  \"description\": \"List of named tasks to perform.\","]
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
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
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
#[doc = "      \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "      \"$ref\": \"#/$defs/timeout\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"TaskTimeoutReference\","]
#[doc = "      \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
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
#[doc = "The definition of a timeout."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"Timeout\","]
#[doc = "  \"description\": \"The definition of a timeout.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"after\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"after\": {"]
#[doc = "      \"title\": \"TimeoutAfter\","]
#[doc = "      \"description\": \"The duration after which to timeout.\","]
#[doc = "      \"$ref\": \"#/$defs/duration\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct Timeout {
    #[doc = "The duration after which to timeout."]
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
#[doc = "      \"description\": \"The object used to define the errors to catch.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"properties\": {"]
#[doc = "        \"as\": {"]
#[doc = "          \"title\": \"CatchAs\","]
#[doc = "          \"description\": \"The name of the runtime expression variable to save the error as. Defaults to 'error'.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"do\": {"]
#[doc = "          \"title\": \"TryTaskCatchDo\","]
#[doc = "          \"description\": \"The definition of the task(s) to run when catching an error.\","]
#[doc = "          \"$ref\": \"#/$defs/taskList\""]
#[doc = "        },"]
#[doc = "        \"errors\": {"]
#[doc = "          \"title\": \"CatchErrors\","]
#[doc = "          \"description\": \"static error filter\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"properties\": {"]
#[doc = "            \"with\": {"]
#[doc = "              \"$ref\": \"#/$defs/errorFilter\""]
#[doc = "            }"]
#[doc = "          }"]
#[doc = "        },"]
#[doc = "        \"exceptWhen\": {"]
#[doc = "          \"title\": \"CatchExceptWhen\","]
#[doc = "          \"description\": \"A runtime expression used to determine whether not to catch the filtered error.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"retry\": {"]
#[doc = "          \"oneOf\": ["]
#[doc = "            {"]
#[doc = "              \"title\": \"RetryPolicyDefinition\","]
#[doc = "              \"description\": \"The retry policy to use, if any, when catching errors.\","]
#[doc = "              \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "            },"]
#[doc = "            {"]
#[doc = "              \"title\": \"RetryPolicyReference\","]
#[doc = "              \"description\": \"The name of the retry policy to use, if any, when catching errors.\","]
#[doc = "              \"type\": \"string\""]
#[doc = "            }"]
#[doc = "          ]"]
#[doc = "        },"]
#[doc = "        \"when\": {"]
#[doc = "          \"title\": \"CatchWhen\","]
#[doc = "          \"description\": \"A runtime expression used to determine whether to catch the filtered error.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"try\": {"]
#[doc = "      \"title\": \"TryTaskConfiguration\","]
#[doc = "      \"description\": \"The task(s) to perform.\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TryTask {
    pub catch: TryTaskCatch,
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "The flow directive to be performed upon completion of the task."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
    #[doc = "The task(s) to perform."]
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
#[doc = "The object used to define the errors to catch."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"TryTaskCatch\","]
#[doc = "  \"description\": \"The object used to define the errors to catch.\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"properties\": {"]
#[doc = "    \"as\": {"]
#[doc = "      \"title\": \"CatchAs\","]
#[doc = "      \"description\": \"The name of the runtime expression variable to save the error as. Defaults to 'error'.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"do\": {"]
#[doc = "      \"title\": \"TryTaskCatchDo\","]
#[doc = "      \"description\": \"The definition of the task(s) to run when catching an error.\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    },"]
#[doc = "    \"errors\": {"]
#[doc = "      \"title\": \"CatchErrors\","]
#[doc = "      \"description\": \"static error filter\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"properties\": {"]
#[doc = "        \"with\": {"]
#[doc = "          \"$ref\": \"#/$defs/errorFilter\""]
#[doc = "        }"]
#[doc = "      }"]
#[doc = "    },"]
#[doc = "    \"exceptWhen\": {"]
#[doc = "      \"title\": \"CatchExceptWhen\","]
#[doc = "      \"description\": \"A runtime expression used to determine whether not to catch the filtered error.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"retry\": {"]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"RetryPolicyDefinition\","]
#[doc = "          \"description\": \"The retry policy to use, if any, when catching errors.\","]
#[doc = "          \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"RetryPolicyReference\","]
#[doc = "          \"description\": \"The name of the retry policy to use, if any, when catching errors.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"when\": {"]
#[doc = "      \"title\": \"CatchWhen\","]
#[doc = "      \"description\": \"A runtime expression used to determine whether to catch the filtered error.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  },"]
#[doc = "  \"unevaluatedProperties\": false"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct TryTaskCatch {
    #[doc = "The name of the runtime expression variable to save the error as. Defaults to 'error'."]
    #[serde(
        rename = "as",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub as_: ::std::option::Option<::std::string::String>,
    #[doc = "The definition of the task(s) to run when catching an error."]
    #[serde(
        rename = "do",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub do_: ::std::option::Option<TaskList>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub errors: ::std::option::Option<CatchErrors>,
    #[doc = "A runtime expression used to determine whether not to catch the filtered error."]
    #[serde(
        rename = "exceptWhen",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub except_when: ::std::option::Option<::std::string::String>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub retry: ::std::option::Option<TryTaskCatchRetry>,
    #[doc = "A runtime expression used to determine whether to catch the filtered error."]
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
#[doc = "      \"description\": \"The retry policy to use, if any, when catching errors.\","]
#[doc = "      \"$ref\": \"#/$defs/retryPolicy\""]
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"RetryPolicyReference\","]
#[doc = "      \"description\": \"The name of the retry policy to use, if any, when catching errors.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
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
#[doc = "    },"]
#[doc = "    {"]
#[doc = "      \"title\": \"LiteralUri\","]
#[doc = "      \"type\": \"string\","]
#[doc = "      \"format\": \"uri\","]
#[doc = "      \"pattern\": \"^[A-Za-z][A-Za-z0-9+\\\\-.]*://.*\""]
#[doc = "    }"]
#[doc = "  ]"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct UriTemplate {
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_0: ::std::option::Option<::std::string::String>,
    #[serde(
        flatten,
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub subtype_1: ::std::option::Option<::std::string::String>,
}
impl ::std::convert::From<&UriTemplate> for UriTemplate {
    fn from(value: &UriTemplate) -> Self {
        value.clone()
    }
}
impl ::std::default::Default for UriTemplate {
    fn default() -> Self {
        Self {
            subtype_0: Default::default(),
            subtype_1: Default::default(),
        }
    }
}
impl UriTemplate {
    pub fn builder() -> builder::UriTemplate {
        Default::default()
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
#[doc = "    \"export\": {"]
#[doc = "      \"title\": \"TaskBaseExport\","]
#[doc = "      \"description\": \"Export task output to context.\","]
#[doc = "      \"$ref\": \"#/$defs/export\""]
#[doc = "    },"]
#[doc = "    \"if\": {"]
#[doc = "      \"title\": \"TaskBaseIf\","]
#[doc = "      \"description\": \"A runtime expression, if any, used to determine whether or not the task should be run.\","]
#[doc = "      \"type\": \"string\""]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"TaskBaseInput\","]
#[doc = "      \"description\": \"Configure the task's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"metadata\": {"]
#[doc = "      \"title\": \"TaskMetadata\","]
#[doc = "      \"description\": \"Holds additional information about the task.\","]
#[doc = "      \"type\": \"object\","]
#[doc = "      \"additionalProperties\": true"]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"TaskBaseOutput\","]
#[doc = "      \"description\": \"Configure the task's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    },"]
#[doc = "    \"then\": {"]
#[doc = "      \"title\": \"TaskBaseThen\","]
#[doc = "      \"description\": \"The flow directive to be performed upon completion of the task.\","]
#[doc = "      \"$ref\": \"#/$defs/flowDirective\""]
#[doc = "    },"]
#[doc = "    \"timeout\": {"]
#[doc = "      \"title\": \"TaskTimeout\","]
#[doc = "      \"oneOf\": ["]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutDefinition\","]
#[doc = "          \"description\": \"The task's timeout configuration, if any.\","]
#[doc = "          \"$ref\": \"#/$defs/timeout\""]
#[doc = "        },"]
#[doc = "        {"]
#[doc = "          \"title\": \"TaskTimeoutReference\","]
#[doc = "          \"description\": \"The name of the task's timeout, if any.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        }"]
#[doc = "      ]"]
#[doc = "    },"]
#[doc = "    \"wait\": {"]
#[doc = "      \"title\": \"WaitTaskConfiguration\","]
#[doc = "      \"description\": \"The amount of time to wait.\","]
#[doc = "      \"$ref\": \"#/$defs/duration\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct WaitTask {
    #[doc = "Export task output to context."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub export: ::std::option::Option<Export>,
    #[doc = "A runtime expression, if any, used to determine whether or not the task should be run."]
    #[serde(
        rename = "if",
        default,
        skip_serializing_if = "::std::option::Option::is_none"
    )]
    pub if_: ::std::option::Option<::std::string::String>,
    #[doc = "Configure the task's input."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub input: ::std::option::Option<Input>,
    #[doc = "Holds additional information about the task."]
    #[serde(default, skip_serializing_if = "::serde_json::Map::is_empty")]
    pub metadata: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
    #[doc = "Configure the task's output."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub output: ::std::option::Option<Output>,
    #[doc = "The flow directive to be performed upon completion of the task."]
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub then: ::std::option::Option<FlowDirective>,
    #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
    pub timeout: ::std::option::Option<TaskTimeout>,
    #[doc = "The amount of time to wait."]
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
#[doc = "The version of the DSL used by the workflow."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"WorkflowDSL\","]
#[doc = "  \"description\": \"The version of the DSL used by the workflow.\","]
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
#[doc = "The workflow's name."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"WorkflowName\","]
#[doc = "  \"description\": \"The workflow's name.\","]
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
#[doc = "The workflow's namespace."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"WorkflowNamespace\","]
#[doc = "  \"description\": \"The workflow's namespace.\","]
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
#[doc = "Partial Serverless Workflow DSL with function task support. \nRuntime expressions: - jq expressions using ${..} syntax (e.g. ${.key.ckey}, ${$task.input}) - liquid templates using $${..} syntax - Available only in fields marked in their descriptions\nContext variables in expressions: - Input mode: keys from input data - Output mode: keys from output data - Workflow info: workflow.id, workflow.definition, workflow.input, workflow.context_variables - Current task info: task.definition, task.raw_input, task.raw_output, task.output, task.flow_directive - Raw data: access pre-transformation data via raw_input and raw_output (e.g. ${$task.raw_input})"]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"$id\": \"https://serverlessworkflow.io/schemas/1.0.0/workflow.yaml\","]
#[doc = "  \"title\": \"WorkflowSchema\","]
#[doc = "  \"description\": \"Partial Serverless Workflow DSL with function task support. \\nRuntime expressions: - jq expressions using ${..} syntax (e.g. ${.key.ckey}, ${$task.input}) - liquid templates using $${..} syntax - Available only in fields marked in their descriptions\\nContext variables in expressions: - Input mode: keys from input data - Output mode: keys from output data - Workflow info: workflow.id, workflow.definition, workflow.input, workflow.context_variables - Current task info: task.definition, task.raw_input, task.raw_output, task.output, task.flow_directive - Raw data: access pre-transformation data via raw_input and raw_output (e.g. ${$task.raw_input})\","]
#[doc = "  \"type\": \"object\","]
#[doc = "  \"required\": ["]
#[doc = "    \"do\","]
#[doc = "    \"document\","]
#[doc = "    \"input\""]
#[doc = "  ],"]
#[doc = "  \"properties\": {"]
#[doc = "    \"do\": {"]
#[doc = "      \"title\": \"Do\","]
#[doc = "      \"description\": \"Defines the task(s) the workflow must perform.\","]
#[doc = "      \"$ref\": \"#/$defs/taskList\""]
#[doc = "    },"]
#[doc = "    \"document\": {"]
#[doc = "      \"title\": \"Document\","]
#[doc = "      \"description\": \"Documents the workflow.\","]
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
#[doc = "          \"description\": \"The version of the DSL used by the workflow.\","]
#[doc = "          \"default\": \"0.0.1\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "        },"]
#[doc = "        \"metadata\": {"]
#[doc = "          \"title\": \"WorkflowMetadata\","]
#[doc = "          \"description\": \"Holds additional information about the workflow.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": true"]
#[doc = "        },"]
#[doc = "        \"name\": {"]
#[doc = "          \"title\": \"WorkflowName\","]
#[doc = "          \"description\": \"The workflow's name.\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "        },"]
#[doc = "        \"namespace\": {"]
#[doc = "          \"title\": \"WorkflowNamespace\","]
#[doc = "          \"description\": \"The workflow's namespace.\","]
#[doc = "          \"default\": \"default\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"pattern\": \"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$\""]
#[doc = "        },"]
#[doc = "        \"summary\": {"]
#[doc = "          \"title\": \"WorkflowSummary\","]
#[doc = "          \"description\": \"The workflow's Markdown summary.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"tags\": {"]
#[doc = "          \"title\": \"WorkflowTags\","]
#[doc = "          \"description\": \"A key/value mapping of the workflow's tags, if any.\","]
#[doc = "          \"type\": \"object\","]
#[doc = "          \"additionalProperties\": true"]
#[doc = "        },"]
#[doc = "        \"title\": {"]
#[doc = "          \"title\": \"WorkflowTitle\","]
#[doc = "          \"description\": \"The workflow's title.\","]
#[doc = "          \"type\": \"string\""]
#[doc = "        },"]
#[doc = "        \"version\": {"]
#[doc = "          \"title\": \"WorkflowVersion\","]
#[doc = "          \"description\": \"The workflow's semantic version.\","]
#[doc = "          \"default\": \"0.0.1\","]
#[doc = "          \"type\": \"string\","]
#[doc = "          \"pattern\": \"^(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)\\\\.(0|[1-9]\\\\d*)(?:-((?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\\\\.(?:0|[1-9]\\\\d*|\\\\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\\\\+([0-9a-zA-Z-]+(?:\\\\.[0-9a-zA-Z-]+)*))?$\""]
#[doc = "        }"]
#[doc = "      },"]
#[doc = "      \"unevaluatedProperties\": false"]
#[doc = "    },"]
#[doc = "    \"input\": {"]
#[doc = "      \"title\": \"Input\","]
#[doc = "      \"description\": \"Configures the workflow's input.\","]
#[doc = "      \"$ref\": \"#/$defs/input\""]
#[doc = "    },"]
#[doc = "    \"output\": {"]
#[doc = "      \"title\": \"Output\","]
#[doc = "      \"description\": \"Configures the workflow's output.\","]
#[doc = "      \"$ref\": \"#/$defs/output\""]
#[doc = "    }"]
#[doc = "  }"]
#[doc = "}"]
#[doc = r" ```"]
#[doc = r" </details>"]
#[derive(:: serde :: Deserialize, :: serde :: Serialize, Clone, Debug)]
pub struct WorkflowSchema {
    #[doc = "Defines the task(s) the workflow must perform."]
    #[serde(rename = "do")]
    pub do_: TaskList,
    pub document: Document,
    #[doc = "Configures the workflow's input."]
    pub input: Input,
    #[doc = "Configures the workflow's output."]
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
#[doc = "The workflow's semantic version."]
#[doc = r""]
#[doc = r" <details><summary>JSON schema</summary>"]
#[doc = r""]
#[doc = r" ```json"]
#[doc = "{"]
#[doc = "  \"title\": \"WorkflowVersion\","]
#[doc = "  \"description\": \"The workflow's semantic version.\","]
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
    pub struct CallTask {
        call: ::std::result::Result<::std::string::String, ::std::string::String>,
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
        with: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for CallTask {
        fn default() -> Self {
            Self {
                call: Err("no value supplied for call".to_string()),
                export: Ok(Default::default()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
                with: Ok(Default::default()),
            }
        }
    }
    impl CallTask {
        pub fn call<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.call = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for call: {}", e));
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
        pub fn with<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<
                ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            >,
            T::Error: ::std::fmt::Display,
        {
            self.with = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for with: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<CallTask> for super::CallTask {
        type Error = super::error::ConversionError;
        fn try_from(value: CallTask) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                call: value.call?,
                export: value.export?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
                output: value.output?,
                then: value.then?,
                timeout: value.timeout?,
                with: value.with?,
            })
        }
    }
    impl ::std::convert::From<super::CallTask> for CallTask {
        fn from(value: super::CallTask) -> Self {
            Self {
                call: Ok(value.call),
                export: Ok(value.export),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
                output: Ok(value.output),
                then: Ok(value.then),
                timeout: Ok(value.timeout),
                with: Ok(value.with),
            }
        }
    }
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
    pub struct DoTask {
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
            ::std::option::Option<super::ErrorDetails>,
            ::std::string::String,
        >,
        instance: ::std::result::Result<
            ::std::option::Option<super::ErrorInstance>,
            ::std::string::String,
        >,
        status: ::std::result::Result<i64, ::std::string::String>,
        title:
            ::std::result::Result<::std::option::Option<super::ErrorTitle>, ::std::string::String>,
        type_: ::std::result::Result<super::ErrorType, ::std::string::String>,
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
            T: ::std::convert::TryInto<::std::option::Option<super::ErrorDetails>>,
            T::Error: ::std::fmt::Display,
        {
            self.detail = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for detail: {}", e));
            self
        }
        pub fn instance<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::ErrorInstance>>,
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
            T: ::std::convert::TryInto<::std::option::Option<super::ErrorTitle>>,
            T::Error: ::std::fmt::Display,
        {
            self.title = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for title: {}", e));
            self
        }
        pub fn type_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ErrorType>,
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
    pub struct ErrorDetails {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::RuntimeExpression>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ErrorDetails {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl ErrorDetails {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::RuntimeExpression>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {}", e));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<ErrorDetails> for super::ErrorDetails {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ErrorDetails,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::ErrorDetails> for ErrorDetails {
        fn from(value: super::ErrorDetails) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
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
    pub struct ErrorTitle {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::RuntimeExpression>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ErrorTitle {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl ErrorTitle {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::RuntimeExpression>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {}", e));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<ErrorTitle> for super::ErrorTitle {
        type Error = super::error::ConversionError;
        fn try_from(
            value: ErrorTitle,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::ErrorTitle> for ErrorTitle {
        fn from(value: super::ErrorTitle) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
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
    pub struct FlowDirective {
        subtype_0: ::std::result::Result<
            ::std::option::Option<super::FlowDirectiveEnum>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for FlowDirective {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl FlowDirective {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FlowDirectiveEnum>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {}", e));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<FlowDirective> for super::FlowDirective {
        type Error = super::error::ConversionError;
        fn try_from(
            value: FlowDirective,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::FlowDirective> for FlowDirective {
        fn from(value: super::FlowDirective) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct ForTask {
        do_: ::std::result::Result<super::TaskList, ::std::string::String>,
        export: ::std::result::Result<::std::option::Option<super::Export>, ::std::string::String>,
        for_: ::std::result::Result<super::ForTaskConfiguration, ::std::string::String>,
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
        while_: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for ForTask {
        fn default() -> Self {
            Self {
                do_: Err("no value supplied for do_".to_string()),
                export: Ok(Default::default()),
                for_: Err("no value supplied for for_".to_string()),
                if_: Ok(Default::default()),
                input: Ok(Default::default()),
                metadata: Ok(Default::default()),
                output: Ok(Default::default()),
                then: Ok(Default::default()),
                timeout: Ok(Default::default()),
                while_: Ok(Default::default()),
            }
        }
    }
    impl ForTask {
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
                do_: value.do_?,
                export: value.export?,
                for_: value.for_?,
                if_: value.if_?,
                input: value.input?,
                metadata: value.metadata?,
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
                do_: Ok(value.do_),
                export: Ok(value.export),
                for_: Ok(value.for_),
                if_: Ok(value.if_),
                input: Ok(value.input),
                metadata: Ok(value.metadata),
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
    pub struct Function {
        arguments: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
        options: ::std::result::Result<
            ::std::option::Option<super::FunctionOptions>,
            ::std::string::String,
        >,
        runner_name: ::std::result::Result<::std::string::String, ::std::string::String>,
        settings: ::std::result::Result<
            ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for Function {
        fn default() -> Self {
            Self {
                arguments: Err("no value supplied for arguments".to_string()),
                options: Ok(Default::default()),
                runner_name: Err("no value supplied for runner_name".to_string()),
                settings: Ok(Default::default()),
            }
        }
    }
    impl Function {
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
        pub fn options<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<super::FunctionOptions>>,
            T::Error: ::std::fmt::Display,
        {
            self.options = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for options: {}", e));
            self
        }
        pub fn runner_name<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::string::String>,
            T::Error: ::std::fmt::Display,
        {
            self.runner_name = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for runner_name: {}", e));
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
    }
    impl ::std::convert::TryFrom<Function> for super::Function {
        type Error = super::error::ConversionError;
        fn try_from(value: Function) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                arguments: value.arguments?,
                options: value.options?,
                runner_name: value.runner_name?,
                settings: value.settings?,
            })
        }
    }
    impl ::std::convert::From<super::Function> for Function {
        fn from(value: super::Function) -> Self {
            Self {
                arguments: Ok(value.arguments),
                options: Ok(value.options),
                runner_name: Ok(value.runner_name),
                settings: Ok(value.settings),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct FunctionOptions {
        broadcast_results_to_listener:
            ::std::result::Result<::std::option::Option<bool>, ::std::string::String>,
        channel: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        retry:
            ::std::result::Result<::std::option::Option<super::RetryPolicy>, ::std::string::String>,
        store_failure: ::std::result::Result<::std::option::Option<bool>, ::std::string::String>,
        store_success: ::std::result::Result<::std::option::Option<bool>, ::std::string::String>,
        use_static: ::std::result::Result<::std::option::Option<bool>, ::std::string::String>,
        with_backup: ::std::result::Result<::std::option::Option<bool>, ::std::string::String>,
    }
    impl ::std::default::Default for FunctionOptions {
        fn default() -> Self {
            Self {
                broadcast_results_to_listener: Ok(Default::default()),
                channel: Ok(Default::default()),
                retry: Ok(Default::default()),
                store_failure: Ok(Default::default()),
                store_success: Ok(Default::default()),
                use_static: Ok(Default::default()),
                with_backup: Ok(Default::default()),
            }
        }
    }
    impl FunctionOptions {
        pub fn broadcast_results_to_listener<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<bool>>,
            T::Error: ::std::fmt::Display,
        {
            self.broadcast_results_to_listener = value.try_into().map_err(|e| {
                format!(
                    "error converting supplied value for broadcast_results_to_listener: {}",
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
        pub fn with_backup<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<bool>>,
            T::Error: ::std::fmt::Display,
        {
            self.with_backup = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for with_backup: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<FunctionOptions> for super::FunctionOptions {
        type Error = super::error::ConversionError;
        fn try_from(
            value: FunctionOptions,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                broadcast_results_to_listener: value.broadcast_results_to_listener?,
                channel: value.channel?,
                retry: value.retry?,
                store_failure: value.store_failure?,
                store_success: value.store_success?,
                use_static: value.use_static?,
                with_backup: value.with_backup?,
            })
        }
    }
    impl ::std::convert::From<super::FunctionOptions> for FunctionOptions {
        fn from(value: super::FunctionOptions) -> Self {
            Self {
                broadcast_results_to_listener: Ok(value.broadcast_results_to_listener),
                channel: Ok(value.channel),
                retry: Ok(value.retry),
                store_failure: Ok(value.store_failure),
                store_success: Ok(value.store_success),
                use_static: Ok(value.use_static),
                with_backup: Ok(value.with_backup),
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
    pub struct RunTask {
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
    pub struct RunTaskConfiguration {
        await_: ::std::result::Result<bool, ::std::string::String>,
        function: ::std::result::Result<super::Function, ::std::string::String>,
        return_: ::std::result::Result<super::ProcessReturnType, ::std::string::String>,
    }
    impl ::std::default::Default for RunTaskConfiguration {
        fn default() -> Self {
            Self {
                await_: Ok(super::defaults::default_bool::<true>()),
                function: Err("no value supplied for function".to_string()),
                return_: Ok(super::defaults::run_task_configuration_return()),
            }
        }
    }
    impl RunTaskConfiguration {
        pub fn await_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<bool>,
            T::Error: ::std::fmt::Display,
        {
            self.await_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for await_: {}", e));
            self
        }
        pub fn function<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::Function>,
            T::Error: ::std::fmt::Display,
        {
            self.function = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for function: {}", e));
            self
        }
        pub fn return_<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<super::ProcessReturnType>,
            T::Error: ::std::fmt::Display,
        {
            self.return_ = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for return_: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<RunTaskConfiguration> for super::RunTaskConfiguration {
        type Error = super::error::ConversionError;
        fn try_from(
            value: RunTaskConfiguration,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                await_: value.await_?,
                function: value.function?,
                return_: value.return_?,
            })
        }
    }
    impl ::std::convert::From<super::RunTaskConfiguration> for RunTaskConfiguration {
        fn from(value: super::RunTaskConfiguration) -> Self {
            Self {
                await_: Ok(value.await_),
                function: Ok(value.function),
                return_: Ok(value.return_),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct SetTask {
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
    pub struct UriTemplate {
        subtype_0: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
        subtype_1: ::std::result::Result<
            ::std::option::Option<::std::string::String>,
            ::std::string::String,
        >,
    }
    impl ::std::default::Default for UriTemplate {
        fn default() -> Self {
            Self {
                subtype_0: Ok(Default::default()),
                subtype_1: Ok(Default::default()),
            }
        }
    }
    impl UriTemplate {
        pub fn subtype_0<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_0 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_0: {}", e));
            self
        }
        pub fn subtype_1<T>(mut self, value: T) -> Self
        where
            T: ::std::convert::TryInto<::std::option::Option<::std::string::String>>,
            T::Error: ::std::fmt::Display,
        {
            self.subtype_1 = value
                .try_into()
                .map_err(|e| format!("error converting supplied value for subtype_1: {}", e));
            self
        }
    }
    impl ::std::convert::TryFrom<UriTemplate> for super::UriTemplate {
        type Error = super::error::ConversionError;
        fn try_from(
            value: UriTemplate,
        ) -> ::std::result::Result<Self, super::error::ConversionError> {
            Ok(Self {
                subtype_0: value.subtype_0?,
                subtype_1: value.subtype_1?,
            })
        }
    }
    impl ::std::convert::From<super::UriTemplate> for UriTemplate {
        fn from(value: super::UriTemplate) -> Self {
            Self {
                subtype_0: Ok(value.subtype_0),
                subtype_1: Ok(value.subtype_1),
            }
        }
    }
    #[derive(Clone, Debug)]
    pub struct WaitTask {
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
    pub struct WorkflowSchema {
        do_: ::std::result::Result<super::TaskList, ::std::string::String>,
        document: ::std::result::Result<super::Document, ::std::string::String>,
        input: ::std::result::Result<super::Input, ::std::string::String>,
        output: ::std::result::Result<::std::option::Option<super::Output>, ::std::string::String>,
    }
    impl ::std::default::Default for WorkflowSchema {
        fn default() -> Self {
            Self {
                do_: Err("no value supplied for do_".to_string()),
                document: Err("no value supplied for document".to_string()),
                input: Err("no value supplied for input".to_string()),
                output: Ok(Default::default()),
            }
        }
    }
    impl WorkflowSchema {
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
    pub(super) fn default_bool<const V: bool>() -> bool {
        V
    }
    pub(super) fn for_task_configuration_at() -> ::std::string::String {
        "index".to_string()
    }
    pub(super) fn for_task_configuration_each() -> ::std::string::String {
        "item".to_string()
    }
    pub(super) fn run_task_configuration_return() -> super::ProcessReturnType {
        super::ProcessReturnType::Stdout
    }
    pub(super) fn schema_variant0_format() -> ::std::string::String {
        "json".to_string()
    }
    pub(super) fn schema_variant1_format() -> ::std::string::String {
        "json".to_string()
    }
}
