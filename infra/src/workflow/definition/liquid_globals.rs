//! Liquid globals that convert a variable only when the template reads it.

// liquid-core's `ValueView` / `ObjectView` signatures use kstring's deprecated
// `KStringCow` alias, so implementing them requires naming it.
#![allow(deprecated)]

use super::expression_context::{ExpressionContext, SECRETS_VARIABLE};
use liquid::model::{
    DisplayCow, KStringCow, Object, ObjectRender, ObjectSource, ObjectView, State, Value, ValueView,
};
use std::{collections::BTreeMap, fmt, sync::OnceLock};

enum Source<'a> {
    Json(&'a serde_json::Value),
    Context(&'a ExpressionContext),
}

struct Entry<'a> {
    source: Source<'a>,
    value: OnceLock<Option<Value>>,
}

/// Globals for one render. Liquid looks variables up by name, so only the
/// variables a template actually reads are converted (once per render).
///
/// Precedence matches the former eager construction: context variables, then
/// raw input keys (a non-object input is exposed as `raw_input`), then the
/// context's `secrets`, which raw input must never shadow (otherwise a workflow
/// input containing `{"secrets": {...}}` could forge authorization headers).
pub(crate) struct LazyLiquidGlobals<'a> {
    entries: BTreeMap<&'a str, Entry<'a>>,
}

impl<'a> LazyLiquidGlobals<'a> {
    pub(crate) fn new(raw_input: &'a serde_json::Value, context: &'a ExpressionContext) -> Self {
        let entry = |source| Entry {
            source,
            value: OnceLock::new(),
        };
        let mut entries: BTreeMap<&'a str, Entry<'a>> = context
            .names()
            .into_iter()
            .map(|name| (name, entry(Source::Context(context))))
            .collect();
        match raw_input {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    entries.insert(key.as_str(), entry(Source::Json(value)));
                }
            }
            other => {
                entries.insert("raw_input", entry(Source::Json(other)));
            }
        }
        if context.contains(SECRETS_VARIABLE) {
            entries.insert(SECRETS_VARIABLE, entry(Source::Context(context)));
        }
        Self { entries }
    }

    fn resolve<'s>(&'s self, name: &str, entry: &'s Entry<'a>) -> Option<&'s Value> {
        entry
            .value
            .get_or_init(|| {
                let converted = match entry.source {
                    Source::Json(value) => liquid::model::to_value(value),
                    Source::Context(context) => match context.get(name) {
                        Ok(Some(value)) => liquid::model::to_value(&*value),
                        Ok(None) => return None,
                        Err(e) => {
                            tracing::warn!("failed to build liquid variable {name}: {e:?}");
                            return None;
                        }
                    },
                };
                converted
                    .inspect_err(|e| {
                        tracing::warn!("failed to convert liquid variable {name}: {e}")
                    })
                    .ok()
            })
            .as_ref()
    }

    fn to_object(&self) -> Object {
        self.entries
            .iter()
            .filter_map(|(name, entry)| {
                let value = self.resolve(name, entry)?.clone();
                Some(((*name).to_owned().into(), value))
            })
            .collect()
    }
}

impl fmt::Debug for LazyLiquidGlobals<'_> {
    // Listing names only: printing must not force conversions.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.entries.keys()).finish()
    }
}

impl ValueView for LazyLiquidGlobals<'_> {
    fn as_debug(&self) -> &dyn fmt::Debug {
        self
    }

    fn render(&self) -> DisplayCow<'_> {
        DisplayCow::Owned(Box::new(ObjectRender::new(self)))
    }

    fn source(&self) -> DisplayCow<'_> {
        DisplayCow::Owned(Box::new(ObjectSource::new(self)))
    }

    fn type_name(&self) -> &'static str {
        "object"
    }

    fn query_state(&self, state: State) -> bool {
        match state {
            State::Truthy => true,
            State::DefaultValue | State::Empty | State::Blank => self.entries.is_empty(),
        }
    }

    fn to_kstr(&self) -> KStringCow<'_> {
        KStringCow::from_string(self.render().to_string())
    }

    fn to_value(&self) -> Value {
        Value::Object(self.to_object())
    }

    fn as_object(&self) -> Option<&dyn ObjectView> {
        Some(self)
    }
}

impl ObjectView for LazyLiquidGlobals<'_> {
    fn as_value(&self) -> &dyn ValueView {
        self
    }

    fn size(&self) -> i64 {
        self.entries.len() as i64
    }

    fn keys<'k>(&'k self) -> Box<dyn Iterator<Item = KStringCow<'k>> + 'k> {
        Box::new(self.entries.keys().map(|k| KStringCow::from_ref(k)))
    }

    fn values<'k>(&'k self) -> Box<dyn Iterator<Item = &'k dyn ValueView> + 'k> {
        Box::new(self.iter().map(|(_, v)| v))
    }

    fn iter<'k>(&'k self) -> Box<dyn Iterator<Item = (KStringCow<'k>, &'k dyn ValueView)> + 'k> {
        Box::new(self.entries.iter().filter_map(|(name, entry)| {
            let value: &dyn ValueView = self.resolve(name, entry)?;
            Some((KStringCow::from_ref(name), value))
        }))
    }

    fn contains_key(&self, index: &str) -> bool {
        self.entries.contains_key(index)
    }

    fn get<'s>(&'s self, index: &str) -> Option<&'s dyn ValueView> {
        let entry = self.entries.get(index)?;
        self.resolve(index, entry).map(|v| v as &dyn ValueView)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn render(
        template: &str,
        raw_input: &serde_json::Value,
        context: &ExpressionContext,
    ) -> String {
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let globals = LazyLiquidGlobals::new(raw_input, context);
        parser.parse(template).unwrap().render(&globals).unwrap()
    }

    #[test]
    fn converts_only_variables_the_template_reads() {
        let builds = Arc::new(AtomicUsize::new(0));
        let mut context = ExpressionContext::new();
        context.insert("name", Arc::new(json!("Taro")));
        let b = builds.clone();
        context.insert_lazy("task", move || {
            b.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"big": true}))
        });
        assert_eq!(render("{{ name }}", &json!({}), &context), "Taro");
        assert_eq!(builds.load(Ordering::SeqCst), 0);
        // A string literal that looks like a reference is not a read.
        assert_eq!(
            render(r#"{{ "{{ task }}" }}"#, &json!({}), &context),
            "{{ task }}"
        );
        assert_eq!(builds.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn repeated_reads_convert_once() {
        let builds = Arc::new(AtomicUsize::new(0));
        let mut context = ExpressionContext::new();
        let b = builds.clone();
        context.insert_lazy("items", move || {
            b.fetch_add(1, Ordering::SeqCst);
            Ok(json!([1, 2, 3]))
        });
        let out = render(
            "{% for i in (0..2) %}{{ items[i] }}{{ items.size }}{% endfor %}",
            &json!({}),
            &context,
        );
        assert_eq!(out, "132333");
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn non_object_input_is_exposed_as_raw_input() {
        let context = ExpressionContext::new();
        assert_eq!(render("{{ raw_input }}", &json!("text"), &context), "text");
    }

    #[test]
    fn unknown_variable_is_still_an_error() {
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        let context = ExpressionContext::new();
        let raw_input = json!({});
        let globals = LazyLiquidGlobals::new(&raw_input, &context);
        let err = parser
            .parse("{{ missing }}")
            .unwrap()
            .render(&globals)
            .expect_err("missing variable must fail");
        assert!(err.to_string().contains("Unknown variable"), "{err}");
    }
}
