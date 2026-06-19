use super::workflow::{self, ExportAs, InputFrom, OutputAs};
use anyhow::Result;
use command_utils::util::liquid::{JsonDecode, JsonEncode};
use liquid::Parser;
use once_cell::sync::OnceCell;
use std::{collections::BTreeMap, sync::Arc};

static LIQUID_PARSER: OnceCell<Parser> = OnceCell::new();
const FILTER_START: &str = "${";
const FILTER_END: &str = "}";
const TEMPLATE_START: &str = "$${";
const TEMPLATE_END: &str = "}";
/// Expression-context key holding authentication secrets. Reserved so that raw
/// workflow input cannot shadow it during Liquid global construction.
const RESERVED_SECRETS_KEY: &str = "secrets";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransformExpression<'a> {
    Jq(&'a str),
    Liquid(&'a str),
}

pub fn transform_expression_body(filter: &str) -> Option<TransformExpression<'_>> {
    if is_transform_template(filter) {
        Some(TransformExpression::Liquid(liquid_template_body(filter)))
    } else if is_transform_filter(filter) {
        Some(TransformExpression::Jq(jq_filter_body(filter)))
    } else {
        None
    }
}

pub fn is_transform_filter(filter: &str) -> bool {
    filter.trim_start().starts_with(FILTER_START) && filter.trim_end().ends_with(FILTER_END)
}

pub fn is_transform_template(filter: &str) -> bool {
    filter.trim_start().starts_with(TEMPLATE_START) && filter.trim_end().ends_with(TEMPLATE_END)
}

pub fn jq_filter_body(filter: &str) -> &str {
    filter
        .trim()
        .trim_start_matches(FILTER_START)
        .trim_end_matches(FILTER_END)
}

pub fn liquid_template_body(template: &str) -> &str {
    let template = template
        .trim()
        .trim_start_matches(TEMPLATE_START)
        .trim_end();
    template.strip_suffix(TEMPLATE_END).unwrap_or(template)
}

pub fn liquid_template_tag_bodies(template: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    let mut rest = template;
    while let Some(start_rel) = rest.find("{") {
        let after_start = &rest[start_rel..];
        let (inner_start_len, close) = if after_start.starts_with("{{") {
            (2, "}}")
        } else if after_start.starts_with("{%") {
            (2, "%}")
        } else {
            rest = &after_start[1..];
            continue;
        };
        let inner = &after_start[inner_start_len..];
        let Some(end_rel) = inner.find(close) else {
            break;
        };
        spans.push(&inner[..end_rel]);
        rest = &inner[end_rel + close.len()..];
    }
    spans
}

pub trait UseJqAndTemplateTransformer {
    fn execute_transform(
        input: Arc<serde_json::Value>,
        filter: &str,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<serde_json::Value, Box<workflow::Error>> {
        if Self::is_transform_template(filter) {
            Self::execute_liquid_template(input, filter, context).map(|r| {
                // parse as primitive types (not obj, arr)
                if let Ok(v) = r.parse::<i64>() {
                    serde_json::Value::Number(v.into())
                } else if let Ok(v) = r.parse::<f64>() {
                    match serde_json::Number::from_f64(v) {
                        Some(n) => serde_json::Value::Number(n),
                        None => serde_json::Value::String(r), // inf or nan
                    }
                } else if let Ok(v) = r.parse::<bool>() {
                    // "true" or "false" only
                    serde_json::Value::Bool(v)
                } else {
                    serde_json::Value::String(r)
                }
            })
        } else if Self::is_transform_filter(filter) {
            Self::execute_jq_filter(input, filter, context)
        } else {
            Ok(serde_json::Value::String(filter.to_owned()))
        }
    }
    fn execute_transform_ref(
        raw_input: Arc<serde_json::Value>,
        filter: &str,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        Self::execute_transform(raw_input, filter, context).map(Arc::new)
    }

    fn is_transform_filter(filter: &str) -> bool {
        is_transform_filter(filter)
    }
    fn is_transform_template(filter: &str) -> bool {
        is_transform_template(filter)
    }
    fn transform_expression_body(filter: &str) -> Option<TransformExpression<'_>> {
        transform_expression_body(filter)
    }
    fn jq_filter_body(filter: &str) -> &str {
        jq_filter_body(filter)
    }
    fn liquid_template_body(template: &str) -> &str {
        liquid_template_body(template)
    }
    fn liquid_template_tag_bodies(template: &str) -> Vec<&str> {
        liquid_template_tag_bodies(template)
    }
    fn eval_as_bool(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
            serde_json::Value::String(s) => !s.is_empty(),
            serde_json::Value::Array(a) => !a.is_empty(),
            serde_json::Value::Object(o) => !o.is_empty(),
            serde_json::Value::Null => false,
        }
    }
    fn execute_transform_as_bool(
        input: Arc<serde_json::Value>,
        if_cond_filter: &str,
        expression: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<bool, Box<workflow::Error>> {
        Self::execute_transform(input.clone(), if_cond_filter, expression)
            .map(|v| Self::eval_as_bool(&v))
    }

    //
    // internal functions
    //
    fn execute_jq_filter_ref(
        raw_input: Arc<serde_json::Value>,
        filter: &str,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        Self::execute_jq_filter(raw_input, filter, context).map(Arc::new)
    }
    fn execute_jq_filter(
        raw_input: Arc<serde_json::Value>,
        filter: &str,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<serde_json::Value, Box<workflow::Error>> {
        if Self::is_transform_filter(filter) {
            let filter = Self::jq_filter_body(filter);
            command_utils::util::jq::execute_jq((*raw_input).clone(), filter, context).map_err(
                |e| {
                    workflow::errors::ErrorFactory::create(
                        workflow::errors::ErrorCode::BadArgument,
                        Some("failed to parse jq filter".to_string()),
                        None,
                        Some(&e),
                    )
                },
            )
        } else {
            // treat as value(no transform)
            Ok(serde_json::Value::String(filter.to_owned()))
        }
    }
    fn execute_liquid_template(
        input: Arc<serde_json::Value>,
        template: &str,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<String, Box<workflow::Error>> {
        if Self::is_transform_template(template) {
            // for error transformation
            fn transform_inner(
                raw_in: &serde_json::Value,
                con: &BTreeMap<String, Arc<serde_json::Value>>,
                templ: &str,
            ) -> Result<String, liquid::Error> {
                let templ = liquid_template_body(templ);
                let liquid_parser = LIQUID_PARSER.get_or_init(|| {
                    liquid::ParserBuilder::with_stdlib()
                        .filter(JsonEncode)
                        .filter(JsonDecode)
                        .build()
                        .unwrap()
                });
                let templ = liquid_parser.parse(templ)?;

                let mut globals = liquid::to_object(con)?;
                // overwrite with raw_input if key is duplicated
                match raw_in {
                    serde_json::Value::Object(map) => {
                        globals.extend(liquid::to_object(&map)?);
                    }
                    _ => {
                        globals.extend(liquid::to_object(
                            &serde_json::json!({"raw_input":(*raw_in).clone()}),
                        )?);
                    }
                }
                // `secrets` is reserved for authentication material injected by the
                // call-task context; raw input must never shadow it (otherwise a
                // workflow input containing {"secrets": {...}} could override the
                // env-derived secret and forge Authorization headers). Re-apply the
                // context value after the raw-input merge so it always wins.
                if let Some(secrets) = con.get(RESERVED_SECRETS_KEY) {
                    globals.insert(
                        RESERVED_SECRETS_KEY.into(),
                        liquid::model::to_value(&**secrets)?,
                    );
                }
                let output = templ.render(&globals)?;
                Ok(output)
            }
            transform_inner(&input, context, template).map_err(|e| {
                workflow::errors::ErrorFactory::create_from_liquid(
                    &e,
                    Some("failed to parse liquid template"),
                    None,
                    None,
                )
            })
        } else {
            // treat as value(no transform)
            Ok(template.to_owned())
        }
    }
}

pub trait UseBoolTransformer: UseJqAndTemplateTransformer {}

#[cfg(test)]
mod test_use_jq_and_template_transformer {
    use super::*;
    use serde_json::json;

    pub struct DefaultTransformer;
    impl UseJqAndTemplateTransformer for DefaultTransformer {}

    #[test]
    fn test_is_transform_filter() {
        assert!(DefaultTransformer::is_transform_filter("${.key}"));
        assert!(DefaultTransformer::is_transform_filter("${.key | @text}"));
        assert!(!DefaultTransformer::is_transform_filter("key"));
        assert!(!DefaultTransformer::is_transform_filter("key | @text"));
    }

    #[test]
    fn test_is_transform_template() {
        assert!(DefaultTransformer::is_transform_template("$${{.key}}"));
        assert!(DefaultTransformer::is_transform_template(
            "$${{.key | @text}}"
        ));
        assert!(!DefaultTransformer::is_transform_template("key"));
        assert!(!DefaultTransformer::is_transform_template("key | @text"));
    }

    #[test]
    fn test_transform_expression_body_matches_transformer_trimming() {
        assert_eq!(
            DefaultTransformer::transform_expression_body(
                "${ {prefix:\"Bearer \"}.prefix + $secrets.API_TOKEN }"
            ),
            Some(TransformExpression::Jq(
                " {prefix:\"Bearer \"}.prefix + $secrets.API_TOKEN "
            ))
        );
        assert_eq!(
            DefaultTransformer::transform_expression_body("$${{{ user }}{{ secrets.API_TOKEN }}}"),
            Some(TransformExpression::Liquid(
                "{{ user }}{{ secrets.API_TOKEN }}"
            ))
        );
        assert_eq!(DefaultTransformer::transform_expression_body("plain"), None);
    }

    #[test]
    fn test_liquid_template_tag_bodies_extracts_only_liquid_tags() {
        assert_eq!(
            DefaultTransformer::liquid_template_tag_bodies(
                "literal secrets.PUBLIC_PATH {{ secrets.API_TOKEN }} {% assign x = y %}"
            ),
            vec![" secrets.API_TOKEN ", " assign x = y "]
        );
        assert!(
            DefaultTransformer::liquid_template_tag_bodies("literal secrets.PUBLIC_PATH")
                .is_empty()
        );
    }

    #[test]
    fn test_execute_jq_filter() {
        let input = Arc::new(json!({
            "key": 1,
            "key2": 2,
        }));
        let filter = "${.key}";
        let context = BTreeMap::new();
        let result =
            DefaultTransformer::execute_jq_filter(input.clone(), filter, &context).unwrap();
        assert_eq!(result, json!(1));

        let filter = "${.key2}";
        let result =
            DefaultTransformer::execute_jq_filter(input.clone(), filter, &context).unwrap();
        assert_eq!(result, json!(2));

        let filter = "${.key | @text}";
        let result =
            DefaultTransformer::execute_jq_filter(input.clone(), filter, &context).unwrap();
        assert_eq!(result, json!("1"));

        let filter = "${.key2 | @text}";
        let result =
            DefaultTransformer::execute_jq_filter(input.clone(), filter, &context).unwrap();
        assert_eq!(result, json!("2"));

        let filter = "key";
        let result =
            DefaultTransformer::execute_jq_filter(input.clone(), filter, &context).unwrap();
        assert_eq!(result, json!("key"));

        let filter = "key | @text";
        let result =
            DefaultTransformer::execute_jq_filter(input.clone(), filter, &context).unwrap();
        assert_eq!(result, json!("key | @text"));
    }

    #[test]
    fn test_execute_liquid_template() {
        let input = Arc::new(json!({
            "key": 1,
            "key2": 2,
        }));
        let template = "$${{{ key }}}";
        let context = BTreeMap::new();
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, "1");

        let template = "$${{{ key2 }}}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, "2");

        let template = "$${{{ key | append: 'hoge' }}}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, "1hoge");
    }

    #[test]
    fn liquid_raw_input_cannot_shadow_context_secrets() {
        // Context (call-task expression) injects the env-derived secret.
        let mut context = BTreeMap::new();
        context.insert(
            "secrets".to_string(),
            Arc::new(json!({ "API_TOKEN": "env-secret" })),
        );
        // Attacker-controlled workflow input tries to override `secrets`.
        let input = Arc::new(json!({
            "secrets": { "API_TOKEN": "attacker-controlled" },
        }));
        let template = "$${{{ secrets.API_TOKEN }}}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        // The context secret must win, not the raw input.
        assert_eq!(result, "env-secret");
    }

    #[test]
    fn liquid_raw_input_non_secret_keys_still_shadow_context() {
        // Non-reserved keys keep the documented raw-input-wins behavior.
        let mut context = BTreeMap::new();
        context.insert("key".to_string(), Arc::new(json!("from-context")));
        let input = Arc::new(json!({ "key": "from-input" }));
        let template = "$${{{ key }}}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, "from-input");
    }

    #[test]
    fn test_execute_transform() {
        let input = Arc::new(json!({
            "key": 1,
            "key2": 2.55,
        }));
        let filter = "${.key}";
        let context = BTreeMap::new();
        let result =
            DefaultTransformer::execute_transform(input.clone(), filter, &context).unwrap();
        assert_eq!(result, json!(1));

        let filter = "$${{{ key }}}";
        let result =
            DefaultTransformer::execute_transform(input.clone(), filter, &context).unwrap();
        assert_eq!(result, serde_json::Value::Number(1.into()));

        let filter = "$${{{ key2 }}}";
        let result =
            DefaultTransformer::execute_transform(input.clone(), filter, &context).unwrap();
        assert_eq!(
            result,
            serde_json::Value::Number(serde_json::Number::from_f64(2.55).unwrap())
        );

        let filter = "key";
        let result =
            DefaultTransformer::execute_transform(input.clone(), filter, &context).unwrap();
        assert_eq!(result, serde_json::Value::String("key".to_string()));

        let filter = "$${{\"hoge\": {{ key2 }}}}";
        let result =
            DefaultTransformer::execute_transform(input.clone(), filter, &context).unwrap();
        assert_eq!(
            result,
            serde_json::Value::String("{\"hoge\": 2.55}".to_string())
        ); // not object
    }
}

pub trait UseExpressionTransformer: UseJqAndTemplateTransformer {
    // use jq to transform input
    fn transform_input(
        raw_input: Arc<serde_json::Value>,
        transform_filter: &InputFrom,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        match transform_filter {
            InputFrom::Variant0(filter) => Self::execute_transform_ref(raw_input, filter, context),
            InputFrom::Variant1(value) => Self::transform_ref_map(raw_input, value, context),
        }
    }
    // use jq to transform input
    fn transform_output(
        raw_input: Arc<serde_json::Value>,
        transform_filter: &OutputAs,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        match transform_filter {
            OutputAs::Variant0(filter) => Self::execute_transform_ref(raw_input, filter, context),
            OutputAs::Variant1(value) => Self::transform_ref_map(raw_input, value, context),
        }
    }
    fn transform_export(
        raw_input: Arc<serde_json::Value>,
        transform_filter: &ExportAs,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        match transform_filter {
            ExportAs::Variant0(filter) => Self::execute_transform_ref(raw_input, filter, context),
            ExportAs::Variant1(value) => Self::transform_ref_map(raw_input, value, context),
        }
    }
    fn transform_value(
        raw_input: Arc<serde_json::Value>,
        value: serde_json::Value,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<serde_json::Value, Box<workflow::Error>> {
        match value {
            serde_json::Value::Object(map) => {
                // recursive
                Self::transform_map(raw_input.clone(), map, context)
            }
            serde_json::Value::Array(v) => {
                // recursive
                Ok(serde_json::Value::Array(
                    v.into_iter()
                        .flat_map(|i| {
                            Self::transform_value(raw_input.clone(), i, context)
                                .inspect_err(|e| tracing::warn!("transform error: {:?}", e))
                                .ok()
                        })
                        .collect(),
                ))
            }
            serde_json::Value::String(filter) => {
                Self::execute_transform(raw_input.clone(), filter.as_str(), context)
            }
            v => Ok(v),
        }
    }
    fn transform_map(
        raw_input: Arc<serde_json::Value>,
        map_filter_or_value: serde_json::Map<String, serde_json::Value>,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<serde_json::Value, Box<workflow::Error>> {
        let mut result = serde_json::Map::new();
        for (key, value) in map_filter_or_value {
            match value {
                serde_json::Value::Object(map) => {
                    // recursive
                    let evaluated = Self::transform_map(raw_input.clone(), map, context)?;
                    result.insert(key, evaluated);
                }
                serde_json::Value::Array(v) => {
                    // recursive
                    let evaluated = v
                        .into_iter()
                        .flat_map(|i| {
                            Self::transform_value(raw_input.clone(), i, context)
                                .inspect_err(|e| tracing::warn!("transform error: {:?}", e))
                                .ok()
                        })
                        .collect();
                    result.insert(key, serde_json::Value::Array(evaluated));
                }
                serde_json::Value::String(filter) => {
                    let evaluated =
                        Self::execute_transform(raw_input.clone(), filter.as_str(), context)?;
                    // tracing::debug!("evaluated: {} -> {:#?}", key, evaluated);
                    result.insert(key, evaluated);
                }
                v => {
                    // tracing::debug!("plain: {} -> {:#?}", key, v);
                    result.insert(key, v);
                }
            }
        }
        Ok(serde_json::Value::Object(result))
    }

    fn transform_ref_value(
        raw_input: Arc<serde_json::Value>,
        value_filter: &serde_json::Value,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        match value_filter {
            serde_json::Value::String(filter) => {
                Self::execute_transform_ref(raw_input, filter, context)
            }
            serde_json::Value::Object(map) => Self::transform_ref_map(raw_input, map, context),
            serde_json::Value::Array(v) => {
                let arr = v
                    .iter()
                    .map(|value| {
                        // XXX clone all twice(jq and map)
                        Self::transform_ref_value(raw_input.clone(), value, context)
                            .map(|v| (*v).clone()) // XXX clone all twice(jq and map)
                    })
                    .collect::<Result<Vec<_>, Box<workflow::Error>>>()?;
                Ok(Arc::new(serde_json::Value::Array(arr)))
            }
            // no transform (low cost)
            _v => Ok(Arc::new(value_filter.clone())),
        }
    }

    fn transform_ref_map(
        raw_input: Arc<serde_json::Value>,
        map_filter_or_value: &serde_json::Map<String, serde_json::Value>,
        context: &BTreeMap<String, Arc<serde_json::Value>>,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        let mut result = serde_json::Map::new();
        for (key, value) in map_filter_or_value {
            match value {
                serde_json::Value::Object(map) => {
                    // recursive
                    let evaluated = Self::transform_ref_map(raw_input.clone(), map, context)?;
                    result.insert(key.clone(), (*evaluated).clone());
                }
                serde_json::Value::Array(v) => {
                    // recursive
                    let evaluated: Vec<_> = v
                        .iter()
                        .filter_map(|i| {
                            Self::transform_ref_value(raw_input.clone(), i, context)
                                .inspect_err(|e| tracing::warn!("transform error: {:?}", e))
                                .ok()
                                .map(|v| (*v).clone())
                        })
                        .collect();
                    result.insert(key.clone(), serde_json::Value::Array(evaluated));
                }
                serde_json::Value::String(filter) => {
                    let evaluated =
                        Self::execute_transform_ref(raw_input.clone(), filter, context)?;
                    // tracing::debug!("evaluated: {} -> {:#?}", key, evaluated);
                    result.insert(key.clone(), (*evaluated).clone());
                }
                v => {
                    // tracing::debug!("plain: {} -> {:#?}", key, v);
                    result.insert(key.clone(), v.clone()); // XXX clone
                }
            }
        }
        Ok(Arc::new(serde_json::Value::Object(result)))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json::json;

    // TODO static jq filter
    pub struct DefaultTransformer;
    impl UseJqAndTemplateTransformer for DefaultTransformer {}
    impl UseExpressionTransformer for DefaultTransformer {}

    #[test]
    fn test_transform_map() {
        // transform
        let input = Arc::new(json!({
            "key1": 1,
            "key3": 3
        }));
        let map = serde_json::Map::from_iter(vec![("otherKey".to_owned(), json!("${.key1}"))]);
        let context = BTreeMap::new();
        let result = DefaultTransformer::transform_ref_map(input.clone(), &map, &context).unwrap();
        assert_eq!(result, Arc::new(json!({"otherKey": 1})));

        // no transform
        let map = serde_json::Map::from_iter(vec![("otherKey".to_owned(), json!("value1"))]);
        let context = BTreeMap::new();
        let result = DefaultTransformer::transform_ref_map(input.clone(), &map, &context).unwrap();
        assert_eq!(result, Arc::new(json!({"otherKey": "value1"})));

        // recursive
        let map = serde_json::Map::from_iter(vec![(
            "otherKey".to_owned(),
            json!({"innerKey": "${.key3}"}),
        )]);
        let context = BTreeMap::new();
        let result = DefaultTransformer::transform_ref_map(input.clone(), &map, &context).unwrap();
        assert_eq!(result, Arc::new(json!({"otherKey": {"innerKey": 3}})));
    }

    #[test]
    fn test_transform_value() {
        let input = Arc::new(json!({
            "key1": 1,
            "key2": 2,
        }));
        let value = json!("${.}");
        let context = BTreeMap::new();
        let result =
            DefaultTransformer::transform_ref_value(input.clone(), &value, &context).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn test_transform_value_context() {
        let input = Arc::new(json!({
            "key1": 1,
            "key2": 2,
        }));
        let value = json!(r#"${"My Name is \($name)"}"#); // double quote is required for string
        let context = BTreeMap::from_iter(vec![(
            "name".to_owned(),
            Arc::new(serde_json::Value::String("Taro".to_string())),
        )]);
        let result = DefaultTransformer::transform_ref_value(input.clone(), &value, &context)
            .map_err(|e| {
                eprintln!("Failed to transform value: {e:#?}");
                e
            })
            .unwrap();
        assert_eq!(result, Arc::new(serde_json::json!("My Name is Taro")));
    }

    #[test]
    fn test_no_transform() {
        let input = Arc::new(json!({
            "key1": 1,
            "key2": 2,
        }));
        let value = json!("."); // no transform without '${}'
        let context = BTreeMap::new();
        let result =
            DefaultTransformer::transform_ref_value(input.clone(), &value, &context).unwrap();
        assert_eq!(result, Arc::new(serde_json::Value::String(".".to_string())));
    }

    #[test]
    fn test_transform_output() {
        let input = Arc::new(json!({
            "key1": 1,
            "key2": 2,
        }));
        let output = OutputAs::Variant0("${.}".to_owned());
        let context = BTreeMap::new();
        let result =
            DefaultTransformer::transform_output(input.clone(), &output, &context).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn test_transform_input() {
        let input = Arc::new(json!({
            "key1": 1,
            "key2": 2,
        }));
        let input_transform = InputFrom::Variant0("${.key1}".to_owned());
        let context = BTreeMap::new();
        let result =
            DefaultTransformer::transform_input(input.clone(), &input_transform, &context).unwrap();
        assert_eq!(result, Arc::new(json!(1)));
    }

    #[test]
    fn test_transform_input2() {
        let input = Arc::new(json!({
            "key1": "hoge",
            "key2": {"key3":"fuga\n piyo"},
        }));
        let input_transform = InputFrom::Variant0("${. | @text}".to_owned());
        let context = BTreeMap::new();
        let result =
            DefaultTransformer::transform_input(input.clone(), &input_transform, &context).unwrap();
        assert_eq!(
            result,
            Arc::new(serde_json::Value::String(
                r#"{"key1":"hoge","key2":{"key3":"fuga\n piyo"}}"#.to_string()
            ))
        );
    }

    #[test]
    fn test_transform_input3() {
        let input = Arc::new(json!({
            "key1": "hoge",
            "key2": {"key3":"fuga\n piyo"},
        }));
        let val = json!({
            "input1": "${. | @text}",
            "input2": "${.key1}",
            "input3": json!([{
                "key3": "${.key2.key3}"
            }]),
        });
        let input_transform = InputFrom::Variant1(val.as_object().unwrap().clone());
        let context = BTreeMap::new();
        let result =
            DefaultTransformer::transform_input(input.clone(), &input_transform, &context).unwrap();
        assert_eq!(
            result,
            Arc::new(serde_json::json!({
                "input1": r#"{"key1":"hoge","key2":{"key3":"fuga\n piyo"}}"#,
                "input2": "hoge",
                "input3": [{"key3": "fuga\n piyo"}]
            }))
        );
    }

    #[test]
    fn test_execute_liquid_template() {
        let json_value = json!({
            "name": "John",
            "age": 30,
            "nested": {
                "value": "test"
            }
        });
        let json_context = json_value.as_object().unwrap();

        let context: BTreeMap<String, Arc<serde_json::Value>> = json_context
            .iter()
            .map(|(k, v)| (k.clone(), Arc::new(v.clone())))
            .collect();
        let input = Arc::new(json!({"greeting": "Hello"}));

        // Test simple template
        let template = "$${{{ greeting }}, {{ name }}!}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, String::from("Hello, John!"));

        // Test using nested values
        let template = "$${{{ nested.value }} - {{ age }}}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, String::from("test - 30"));

        // Test with no transformation (regular string)
        let template = r#"
        Hello,
        World!
        "#;
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(
            result,
            String::from("\n        Hello,\n        World!\n        ")
        );

        // Test with empty template
        let template = "$${}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, String::from(""));

        // Test with unknown variable template
        let template = "$${{{unknown}}}";
        let result = DefaultTransformer::execute_liquid_template(input.clone(), template, &context);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("requested variable=unknown")
        );

        // Test with context overriding input
        let input = Arc::new(json!({"name": "Alice"}));
        let template = "$${{{ name }}}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, String::from("Alice"));
    }

    // use date function
    // https://github.com/cobalt-org/liquid-rust/blob/8a2bc481aaea410caa4ef73bd02f23b7655faff7/crates/core/src/model/scalar/datetime.rs#L185
    #[test]
    fn test_execute_liquid_template_date() {
        let json_value = json!({
            "name": "John",
            "age": 30,
            "nested": {
                "value": "test"
            }
        });
        let json_context = json_value.as_object().unwrap();

        let context: BTreeMap<String, Arc<serde_json::Value>> = json_context
            .iter()
            .map(|(k, v)| (k.clone(), Arc::new(v.clone())))
            .collect();

        let input = Arc::new(json!({"time": "2024-01-03 12:34:56"}));
        let template = "$${{{ time | date: '%Y/%m/%dT%H:%M' }}}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, String::from("2024/01/03T12:34"));

        let input = Arc::new(json!({"time": "2024-02-03 12:34:56"}));
        let template = "$${{%- assign year = time | date: '%m' | minus: 1 -%}\n    /home/user/ほげ/{{ year }}.txt}";
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result, String::from("/home/user/ほげ/1.txt"));

        // use date function with now
        // let input = Arc::new(json!({"time": "now"}));
        let template = r#"$${{{ "now" | date: '%Y-%m-%d' }}}"#;
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        assert_eq!(result.len(), 10);

        let template = r#"$${
          {%- assign year = 'now' | date: '%Y' -%}
          {%- assign month = 'now' | date: '%m' | minus: 1 -%}
          /home/user/日記/{{ year }}/{{ month }}/}"#;
        let result =
            DefaultTransformer::execute_liquid_template(input.clone(), template, &context).unwrap();
        let result_text = result.clone();
        let re = regex::Regex::new(r"^/home/user/日記/(\d{4})/(\d{1,2})/$").unwrap();
        assert!(
            re.is_match(&result_text),
            "Result '{result_text}' doesn't match the expected format"
        );

        if let Some(captures) = re.captures(&result_text) {
            let year = captures.get(1).unwrap().as_str();
            let month = captures.get(2).unwrap().as_str();

            assert_eq!(year.len(), 4, "Year should be 4 digits");
            assert!(month.len() <= 2, "Month should be 1 or 2 digits");

            let year_num: i32 = year.parse().unwrap();
            let month_num: i32 = month.parse().unwrap();

            assert!(
                (2022..=9999).contains(&year_num),
                "Year should be reasonable"
            );
            assert!((0..=11).contains(&month_num), "Month should be 0-11");
        }
    }

    /// Test for workflow.input nested reference in Liquid template
    /// This reproduces the issue where workflow.input.limit returns schema default
    /// instead of the actual input value
    #[test]
    fn test_workflow_input_nested_reference() {
        // Simulate the workflow descriptor structure
        let workflow_descriptor = json!({
            "id": "test-workflow-id",
            "input": {
                "grpcHost": "rss-crawler-admin-grpc-service.news-aggregator.svc.cluster.local",
                "grpcPort": 9000,
                "limit": 200,
                "offset": 0,
                "isAsc": false,
                "maxPages": 1,
                "webdriver_url": "http://selenium-hub.selenium.svc.cluster.local:4444"
            },
            "started_at": "2024-01-01T00:00:00Z"
        });

        // Build context as expression.rs does
        let context: BTreeMap<String, Arc<serde_json::Value>> = BTreeMap::from_iter(vec![(
            "workflow".to_string(),
            Arc::new(workflow_descriptor),
        )]);

        // Task input - this would be the workflow input at task execution time
        let task_input = Arc::new(json!({
            "grpcHost": "rss-crawler-admin-grpc-service.news-aggregator.svc.cluster.local",
            "grpcPort": 9000,
            "limit": 200,
            "offset": 0,
            "isAsc": false
        }));

        // This is the actual template from listing-page-checker.yaml
        let template = r#"$${{"limit": {{workflow.input.limit}}, "offset": {{workflow.input.offset}}, "is_asc": {{workflow.input.isAsc}}}}"#;

        let result =
            DefaultTransformer::execute_liquid_template(task_input.clone(), template, &context);

        println!("Template result: {:?}", result);

        match result {
            Ok(output) => {
                println!("Output: {}", output);
                // Parse the result as JSON to verify values
                let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
                assert_eq!(
                    parsed["limit"],
                    json!(200),
                    "limit should be 200, not schema default 100"
                );
                assert_eq!(parsed["offset"], json!(0), "offset should be 0");
                assert_eq!(
                    parsed["is_asc"],
                    json!(false),
                    "is_asc should be false, not schema default true"
                );
            }
            Err(e) => {
                panic!("Template execution failed: {:?}", e);
            }
        }
    }

    /// Test for simpler workflow.input reference
    #[test]
    fn test_workflow_input_simple_reference() {
        let workflow_descriptor = json!({
            "id": "test-id",
            "input": {
                "limit": 200,
                "isAsc": false
            },
            "started_at": "2024-01-01T00:00:00Z"
        });

        let context: BTreeMap<String, Arc<serde_json::Value>> = BTreeMap::from_iter(vec![(
            "workflow".to_string(),
            Arc::new(workflow_descriptor),
        )]);

        let task_input = Arc::new(json!({}));

        // Test simple reference
        let template = "$${{{ workflow.input.limit }}}";
        let result =
            DefaultTransformer::execute_liquid_template(task_input.clone(), template, &context)
                .unwrap();
        println!("Simple reference result: {}", result);
        assert_eq!(result, "200");

        // Test boolean reference
        let template = "$${{{ workflow.input.isAsc }}}";
        let result =
            DefaultTransformer::execute_liquid_template(task_input.clone(), template, &context)
                .unwrap();
        println!("Boolean reference result: {}", result);
        assert_eq!(result, "false");
    }

    /// Test for jq filter with $workflow variable
    #[test]
    fn test_jq_workflow_input_reference() {
        let workflow_descriptor = json!({
            "id": "test-id",
            "input": {
                "grpcHost": "rss-crawler-admin-grpc-service.news-aggregator.svc.cluster.local",
                "grpcPort": 9000,
                "limit": 200,
                "isAsc": false
            },
            "started_at": "2024-01-01T00:00:00Z"
        });

        let context: BTreeMap<String, Arc<serde_json::Value>> = BTreeMap::from_iter(vec![(
            "workflow".to_string(),
            Arc::new(workflow_descriptor),
        )]);

        let task_input = Arc::new(json!({}));

        // Test jq reference to $workflow.input.grpcHost
        let filter = "${$workflow.input.grpcHost}";
        let result =
            DefaultTransformer::execute_jq_filter(task_input.clone(), filter, &context).unwrap();
        println!("jq $workflow.input.grpcHost result: {:?}", result);
        assert_eq!(
            result,
            json!("rss-crawler-admin-grpc-service.news-aggregator.svc.cluster.local")
        );

        // Test jq reference to $workflow.input.grpcPort
        let filter = "${$workflow.input.grpcPort}";
        let result =
            DefaultTransformer::execute_jq_filter(task_input.clone(), filter, &context).unwrap();
        println!("jq $workflow.input.grpcPort result: {:?}", result);
        assert_eq!(result, json!(9000));

        // Test jq reference to $workflow.input.limit
        let filter = "${$workflow.input.limit}";
        let result =
            DefaultTransformer::execute_jq_filter(task_input.clone(), filter, &context).unwrap();
        println!("jq $workflow.input.limit result: {:?}", result);
        assert_eq!(result, json!(200));
    }
}
