pub use super::liquid_tags::liquid_tag_bodies;
use super::{
    expression_cache::{JQ_FILTERS, LIQUID_TEMPLATES},
    expression_context::ExpressionContext,
    liquid_globals::LazyLiquidGlobals,
    workflow::{self, ExportAs, InputFrom, OutputAs},
};
use anyhow::Result;
use command_utils::util::liquid::{JsonDecode, JsonEncode};
use liquid::Parser;
use once_cell::sync::OnceCell;
use std::sync::Arc;

static LIQUID_PARSER: OnceCell<Parser> = OnceCell::new();
const FILTER_START: &str = "${";
const FILTER_END: &str = "}";
const TEMPLATE_START: &str = "$${";
const TEMPLATE_END: &str = "}";

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
    strip_delimiters(filter, FILTER_START, FILTER_END)
}

pub fn liquid_template_body(template: &str) -> &str {
    strip_delimiters(template, TEMPLATE_START, TEMPLATE_END)
}

// Strip each delimiter exactly once: the body itself may start with `${` or
// end with `}` (e.g. a jq object construction), which must be kept intact.
fn strip_delimiters<'a>(expression: &'a str, start: &str, end: &str) -> &'a str {
    let expression = expression.trim();
    let expression = expression.strip_prefix(start).unwrap_or(expression);
    expression.strip_suffix(end).unwrap_or(expression)
}

pub trait UseJqAndTemplateTransformer {
    fn execute_transform(
        input: Arc<serde_json::Value>,
        filter: &str,
        context: &ExpressionContext,
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
        context: &ExpressionContext,
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
        expression: &ExpressionContext,
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
        context: &ExpressionContext,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        Self::execute_jq_filter(raw_input, filter, context).map(Arc::new)
    }
    fn execute_jq_filter(
        raw_input: Arc<serde_json::Value>,
        filter: &str,
        context: &ExpressionContext,
    ) -> Result<serde_json::Value, Box<workflow::Error>> {
        if Self::is_transform_filter(filter) {
            let filter = Self::jq_filter_body(filter);
            let variables =
                context.resolve_subset(command_utils::util::jq::referenced_variables(filter))?;
            JQ_FILTERS
                .execute(&raw_input, filter, &variables)
                .map_err(|e| {
                    // Only referenced variables reach the engine, so list every
                    // name here to make typos in variable names diagnosable.
                    tracing::debug!(
                        "jq error for '{}'. Variables in context: {:?}",
                        filter,
                        context.names()
                    );
                    workflow::errors::ErrorFactory::new().expression(
                        "failed to parse jq filter".to_string(),
                        None,
                        Some(e.to_string()),
                    )
                })
        } else {
            // treat as value(no transform)
            Ok(serde_json::Value::String(filter.to_owned()))
        }
    }
    fn execute_liquid_template(
        input: Arc<serde_json::Value>,
        template: &str,
        context: &ExpressionContext,
    ) -> Result<String, Box<workflow::Error>> {
        if Self::is_transform_template(template) {
            // for error transformation
            fn transform_inner(
                raw_in: &serde_json::Value,
                context: &ExpressionContext,
                templ: &str,
            ) -> Result<String, liquid::Error> {
                let liquid_parser = LIQUID_PARSER.get_or_init(|| {
                    liquid::ParserBuilder::with_stdlib()
                        .filter(JsonEncode)
                        .filter(JsonDecode)
                        .build()
                        .unwrap()
                });
                let templ = LIQUID_TEMPLATES
                    .try_get_with(templ.to_owned(), || {
                        liquid_parser.parse(templ).map(Arc::new)
                    })
                    .map_err(|e| (*e).clone())?;
                templ.render(&LazyLiquidGlobals::new(raw_in, context))
            }
            let body = liquid_template_body(template);
            transform_inner(&input, context, body).map_err(|e| {
                workflow::errors::ErrorFactory::new().expression(
                    "failed to parse liquid template".to_string(),
                    None,
                    Some(e.to_string()),
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
        assert_eq!(
            DefaultTransformer::transform_expression_body("${$x + {a: 1}}"),
            Some(TransformExpression::Jq("$x + {a: 1}"))
        );
        assert_eq!(
            DefaultTransformer::transform_expression_body("$${$${{ user }}}"),
            Some(TransformExpression::Liquid("$${{ user }}"))
        );
        assert_eq!(DefaultTransformer::transform_expression_body("plain"), None);
        assert_eq!(DefaultTransformer::transform_expression_body("${ .a"), None);
        assert_eq!(
            DefaultTransformer::transform_expression_body("prefix ${ .a }"),
            None
        );
    }

    #[test]
    fn test_jq_filter_body_strips_delimiters_once() {
        assert_eq!(jq_filter_body("${ {a: 1} }"), " {a: 1} ");
        assert_eq!(jq_filter_body("${$x + {a: 1}}"), "$x + {a: 1}");
        assert_eq!(jq_filter_body("${\"}\"}"), "\"}\"");
        assert_eq!(jq_filter_body("${.key}"), ".key");
        assert_eq!(jq_filter_body("  ${.key}  "), ".key");
        assert_eq!(jq_filter_body("${${.key}}"), "${.key}");
        assert_eq!(jq_filter_body("${}"), "");
    }

    #[test]
    fn test_liquid_template_body_strips_delimiters_once() {
        assert_eq!(liquid_template_body("$${{{ user }}}"), "{{ user }}");
        assert_eq!(liquid_template_body("  $${{{ user }}}  "), "{{ user }}");
        assert_eq!(liquid_template_body("$${$${x}}"), "$${x}");
    }

    #[test]
    fn test_execute_jq_filter_ending_with_object_construction() {
        let input = Arc::new(json!({}));
        let mut context = ExpressionContext::new();
        context.insert("x", Arc::new(json!({"b": 2})));
        let cases = [
            ("${ {a: 1} }", json!({"a": 1})),
            ("${$x + {a: 1}}", json!({"b": 2, "a": 1})),
            ("${\"}\"}", json!("}")),
            ("${ {a: {b: 1}} }", json!({"a": {"b": 1}})),
            ("${{a: {b: 1}}}", json!({"a": {"b": 1}})),
        ];
        for (filter, expected) in cases {
            let result = DefaultTransformer::execute_jq_filter(input.clone(), filter, &context)
                .unwrap_or_else(|e| panic!("{filter}: {e:?}"));
            assert_eq!(result, expected, "{filter}");
            let result = DefaultTransformer::execute_transform(input.clone(), filter, &context)
                .unwrap_or_else(|e| panic!("{filter}: {e:?}"));
            assert_eq!(result, expected, "{filter}");
        }
    }

    #[test]
    fn test_execute_jq_filter() {
        let input = Arc::new(json!({
            "key": 1,
            "key2": 2,
        }));
        let filter = "${.key}";
        let context = ExpressionContext::new();
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
        let context = ExpressionContext::new();
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

        let template = "$${{{ key | plus: 1 }}}";
        let result =
            DefaultTransformer::execute_liquid_template(input, template, &context).unwrap();
        assert_eq!(result, "2");
    }

    fn context_with_poisoned_lazy(name: &str) -> ExpressionContext {
        // A lazy variable that fails if built: evaluation must succeed as long
        // as the expression never references it.
        let mut context = ExpressionContext::new();
        context.insert_lazy(name.to_string(), || serde_json::from_str("{"));
        context
    }

    #[test]
    fn jq_does_not_build_unreferenced_lazy_variables() {
        let mut context = context_with_poisoned_lazy("context");
        context.insert("item", Arc::new(json!(3)));
        let result =
            DefaultTransformer::execute_transform(Arc::new(json!({})), "${$item * 2}", &context)
                .unwrap();
        assert_eq!(result, json!(6));
        assert!(
            DefaultTransformer::execute_transform(Arc::new(json!({})), "${$context}", &context)
                .is_err()
        );
    }

    #[test]
    fn expression_failures_raise_the_spec_expression_error() {
        use crate::workflow::definition::workflow::errors::StandardErrorType;
        let input = Arc::new(json!({}));
        let failures = [
            ("jq", "${ .a | invalid( }", ExpressionContext::new()),
            ("liquid", "$${{% if %}}", ExpressionContext::new()),
            (
                "lazy variable",
                "${$context}",
                context_with_poisoned_lazy("context"),
            ),
        ];
        for (label, expression, context) in failures {
            let err = DefaultTransformer::execute_transform(input.clone(), expression, &context)
                .expect_err(label);
            assert_eq!(
                err.type_.0,
                StandardErrorType::Expression.uri(),
                "{label}: {err:?}"
            );
            assert_eq!(err.status, 400, "{label}");
        }
    }

    #[test]
    fn jq_builds_referenced_lazy_variables() {
        let mut context = ExpressionContext::new();
        context.insert_lazy("workflow", || Ok(json!({"input": {"k": "v"}})));
        let result = DefaultTransformer::execute_transform(
            Arc::new(json!({})),
            "${$workflow.input.k}",
            &context,
        )
        .unwrap();
        assert_eq!(result, json!("v"));
    }

    #[test]
    fn liquid_does_not_build_unreferenced_lazy_variables() {
        let mut context = context_with_poisoned_lazy("task");
        context.insert("name", Arc::new(json!("Taro")));
        let result = DefaultTransformer::execute_liquid_template(
            Arc::new(json!({})),
            "$${Hello {{ name }}}",
            &context,
        )
        .unwrap();
        assert_eq!(result, "Hello Taro");
    }

    #[test]
    fn liquid_resolves_variables_used_in_tags() {
        let mut context = context_with_poisoned_lazy("context");
        context.insert("items", Arc::new(json!([1, 2])));
        context.insert("flag", Arc::new(json!(true)));
        context.insert("src", Arc::new(json!("x")));
        let template = "$${{% for i in items %}{{ i }}{% endfor %}{% if flag %}T{% endif %}{% assign y = src %}{{ y }}}";
        let result =
            DefaultTransformer::execute_liquid_template(Arc::new(json!({})), template, &context)
                .unwrap();
        assert_eq!(result, "12Tx");
    }

    #[test]
    fn liquid_quoted_index_after_identifier_is_resolved() {
        let mut context = context_with_poisoned_lazy("task");
        context.insert_lazy("obj", || Ok(json!({"odd key": "found"})));
        let result = DefaultTransformer::execute_liquid_template(
            Arc::new(json!({})),
            "$${{{ obj[\"odd key\"] }}}",
            &context,
        )
        .unwrap();
        assert_eq!(result, "found");
    }

    #[test]
    fn liquid_template_cache_rebinds_variables() {
        for name in ["A", "B"] {
            let mut context = ExpressionContext::new();
            context.insert("who", Arc::new(json!(name)));
            let result = DefaultTransformer::execute_liquid_template(
                Arc::new(json!({})),
                "$${hi {{ who }}}",
                &context,
            )
            .unwrap();
            assert_eq!(result, format!("hi {name}"));
        }
    }

    #[test]
    fn liquid_resolves_variables_after_quoted_closing_delimiter() {
        let mut context = context_with_poisoned_lazy("task");
        context.insert("name", Arc::new(json!("Taro")));
        let result = DefaultTransformer::execute_liquid_template(
            Arc::new(json!({})),
            r#"$${{{ "}}" | append: name }}}"#,
            &context,
        )
        .unwrap();
        assert_eq!(result, "}}Taro");
    }

    #[test]
    fn liquid_resolves_variables_after_unbalanced_quote_in_raw_or_comment() {
        let mut context = context_with_poisoned_lazy("task");
        context.insert("name", Arc::new(json!("Taro")));
        for (template, expected) in [
            (
                r#"$${{% raw %}{{ " }}{% endraw %}{{ name }}}"#,
                r#"{{ " }}Taro"#,
            ),
            ("$${{% comment %}{{ ' }}{% endcomment %}{{ name }}}", "Taro"),
            // No closing `}}` anywhere after the unbalanced quote.
            (
                r#"$${{% raw %}{{ "{% endraw %}{% if name %}YES{% endif %}}"#,
                r#"{{ "YES"#,
            ),
            (
                r#"$${{% comment %}{{ "{% endcomment %}{% if name %}YES{% endif %}}"#,
                "YES",
            ),
            // The quote inside raw pairs with a quote after the real tag.
            (
                r#"$${{% raw %}{{ "{% endraw %}{{ name }}" junk }}}"#,
                r#"{{ "Taro" junk }}"#,
            ),
        ] {
            let result = DefaultTransformer::execute_liquid_template(
                Arc::new(json!({})),
                template,
                &context,
            )
            .unwrap_or_else(|e| panic!("{template}: {e:?}"));
            assert_eq!(result, expected, "{template}");
        }
    }

    #[test]
    fn liquid_resolves_hyphen_leading_names() {
        let mut context = context_with_poisoned_lazy("task");
        context.insert("-name", Arc::new(json!("Hyphen")));
        for template in [
            "$${{{ -name }}}",
            "$${{{ -name-}}}",
            "$${{{--name}}}",
            "$${{{--name-}}}",
            "$${{{- -name -}}}",
        ] {
            let result = DefaultTransformer::execute_liquid_template(
                Arc::new(json!({})),
                template,
                &context,
            )
            .unwrap_or_else(|e| panic!("{template}: {e:?}"));
            assert_eq!(result, "Hyphen", "{template}");
        }
    }

    #[test]
    fn liquid_whitespace_control_without_spaces_resolves_variables() {
        let mut context = context_with_poisoned_lazy("task");
        context.insert("name", Arc::new(json!("Taro")));
        context.insert("x", Arc::new(json!(true)));
        for (template, expected) in [
            ("$${[{{name-}}]}", "[Taro]"),
            ("$${[{{-name}}]}", "[Taro]"),
            ("$${[{{-name-}}]}", "[Taro]"),
            ("$${[{%-if x-%}Y{%-endif-%}]}", "[Y]"),
        ] {
            let result = DefaultTransformer::execute_liquid_template(
                Arc::new(json!({})),
                template,
                &context,
            )
            .unwrap_or_else(|e| panic!("{template}: {e:?}"));
            assert_eq!(result, expected, "{template}");
        }
    }

    #[test]
    fn liquid_raw_input_cannot_shadow_context_secrets() {
        // Context (call-task expression) injects the env-derived secret.
        let mut context = ExpressionContext::new();
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
        let mut context = ExpressionContext::new();
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
        let context = ExpressionContext::new();
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
        context: &ExpressionContext,
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
        context: &ExpressionContext,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        match transform_filter {
            OutputAs::Variant0(filter) => Self::execute_transform_ref(raw_input, filter, context),
            OutputAs::Variant1(value) => Self::transform_ref_map(raw_input, value, context),
        }
    }
    fn transform_export(
        raw_input: Arc<serde_json::Value>,
        transform_filter: &ExportAs,
        context: &ExpressionContext,
    ) -> Result<Arc<serde_json::Value>, Box<workflow::Error>> {
        match transform_filter {
            ExportAs::Variant0(filter) => Self::execute_transform_ref(raw_input, filter, context),
            ExportAs::Variant1(value) => Self::transform_ref_map(raw_input, value, context),
        }
    }
    fn transform_value(
        raw_input: Arc<serde_json::Value>,
        value: serde_json::Value,
        context: &ExpressionContext,
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
        context: &ExpressionContext,
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
        context: &ExpressionContext,
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
        context: &ExpressionContext,
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
        let context = ExpressionContext::new();
        let result = DefaultTransformer::transform_ref_map(input.clone(), &map, &context).unwrap();
        assert_eq!(result, Arc::new(json!({"otherKey": 1})));

        // no transform
        let map = serde_json::Map::from_iter(vec![("otherKey".to_owned(), json!("value1"))]);
        let context = ExpressionContext::new();
        let result = DefaultTransformer::transform_ref_map(input.clone(), &map, &context).unwrap();
        assert_eq!(result, Arc::new(json!({"otherKey": "value1"})));

        // recursive
        let map = serde_json::Map::from_iter(vec![(
            "otherKey".to_owned(),
            json!({"innerKey": "${.key3}"}),
        )]);
        let context = ExpressionContext::new();
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
        let context = ExpressionContext::new();
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
        let context = ExpressionContext::from_iter(vec![(
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
        let context = ExpressionContext::new();
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
        let context = ExpressionContext::new();
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
        let context = ExpressionContext::new();
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
        let context = ExpressionContext::new();
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
        let context = ExpressionContext::new();
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

        let context: ExpressionContext = json_context
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

        let context: ExpressionContext = json_context
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
        let context: ExpressionContext = ExpressionContext::from_iter(vec![(
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

        let context: ExpressionContext = ExpressionContext::from_iter(vec![(
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

        let context: ExpressionContext = ExpressionContext::from_iter(vec![(
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
