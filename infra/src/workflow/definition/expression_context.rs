//! Variables visible to workflow runtime expressions (`$input`, `$context`,
//! context variables, ...).

use super::workflow::{self, errors::ErrorFactory};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, OnceLock},
};

/// Variable holding authentication secrets for call tasks. Reserved: raw
/// workflow input must never shadow it in templates.
pub const SECRETS_VARIABLE: &str = "secrets";

type LazyBuilder = Box<dyn Fn() -> Result<Value, serde_json::Error> + Send + Sync>;

/// A variable whose value is built on first use and then memoized.
struct LazyValue {
    build: LazyBuilder,
    value: OnceLock<Arc<Value>>,
}

impl LazyValue {
    fn resolve(&self, name: &str) -> Result<Arc<Value>, Box<workflow::Error>> {
        if let Some(v) = self.value.get() {
            return Ok(v.clone());
        }
        let v = Arc::new((self.build)().map_err(|e| {
            ErrorFactory::create_from_serde_json(
                &e,
                Some(&format!("failed to build expression variable ${name}")),
                None,
                None,
            )
        })?);
        // A concurrent resolve may have won; both values are identical.
        Ok(self.value.get_or_init(|| v).clone())
    }
}

/// Variables for one round of expression evaluation.
///
/// Some variables (`$context`, `$workflow`, `$task`, ...) are serializations of
/// the whole workflow or task state, which is costly and rarely needed. They
/// are registered lazily and only built when an expression references them, so
/// the cost of an evaluation follows what the expression reads rather than the
/// total size of the state.
///
/// Cloning is cheap (pointer copies) and clones share already-built lazy
/// values; later `insert`s affect only the clone they are made on.
#[derive(Clone, Default)]
pub struct ExpressionContext {
    values: BTreeMap<String, Arc<Value>>,
    lazy: BTreeMap<String, Arc<LazyValue>>,
}

impl ExpressionContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a variable, replacing any lazy or eager variable of the same name.
    pub fn insert(&mut self, name: impl Into<String>, value: Arc<Value>) {
        let name = name.into();
        self.lazy.remove(&name);
        self.values.insert(name, value);
    }

    /// Register a variable built by `build` the first time it is referenced.
    /// `build` should capture a snapshot (not a lock) so the value reflects
    /// the state when the context was created.
    pub fn insert_lazy(
        &mut self,
        name: impl Into<String>,
        build: impl Fn() -> Result<Value, serde_json::Error> + Send + Sync + 'static,
    ) {
        let name = name.into();
        self.values.remove(&name);
        self.lazy.insert(
            name,
            Arc::new(LazyValue {
                build: Box::new(build),
                value: OnceLock::new(),
            }),
        );
    }

    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name) || self.lazy.contains_key(name)
    }

    /// Names of all variables in sorted order, without building lazy ones.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .values
            .keys()
            .chain(self.lazy.keys())
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        names
    }

    /// Value of `name`, building it if it is lazy.
    pub fn get(&self, name: &str) -> Result<Option<Arc<Value>>, Box<workflow::Error>> {
        if let Some(v) = self.values.get(name) {
            return Ok(Some(v.clone()));
        }
        self.lazy
            .get(name)
            .map(|lazy| lazy.resolve(name))
            .transpose()
    }

    /// Values of the given names that exist; unknown names are skipped so the
    /// expression engine can report them as undefined.
    pub fn resolve_subset<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<BTreeMap<String, Arc<Value>>, Box<workflow::Error>> {
        let mut resolved = BTreeMap::new();
        for name in names {
            if let Some(v) = self.get(name)? {
                resolved.insert(name.to_owned(), v);
            }
        }
        Ok(resolved)
    }
}

impl FromIterator<(String, Arc<Value>)> for ExpressionContext {
    fn from_iter<I: IntoIterator<Item = (String, Arc<Value>)>>(iter: I) -> Self {
        Self {
            values: iter.into_iter().collect(),
            lazy: BTreeMap::new(),
        }
    }
}

impl fmt::Debug for ExpressionContext {
    // Lazy values are not forced: printing a context must not trigger the
    // serialization this type exists to avoid.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExpressionContext")
            .field("values", &self.values)
            .field("lazy", &self.lazy.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting(
        counter: &Arc<AtomicUsize>,
        value: Value,
    ) -> impl Fn() -> Result<Value, serde_json::Error> + Send + Sync + 'static {
        let counter = counter.clone();
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(value.clone())
        }
    }

    #[test]
    fn lazy_value_is_not_built_unless_requested() {
        let builds = Arc::new(AtomicUsize::new(0));
        let mut ctx = ExpressionContext::new();
        ctx.insert("input", Arc::new(json!(1)));
        ctx.insert_lazy("context", counting(&builds, json!({"big": true})));
        let subset = ctx.resolve_subset(["input"]).unwrap();
        assert_eq!(subset.keys().collect::<Vec<_>>(), vec!["input"]);
        assert_eq!(builds.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn lazy_value_is_built_once_and_shared_by_clones() {
        let builds = Arc::new(AtomicUsize::new(0));
        let mut ctx = ExpressionContext::new();
        ctx.insert_lazy("task", counting(&builds, json!({"input": 1})));
        let cloned = ctx.clone();
        assert_eq!(
            ctx.get("task").unwrap(),
            Some(Arc::new(json!({"input": 1})))
        );
        assert_eq!(
            cloned.get("task").unwrap(),
            Some(Arc::new(json!({"input": 1})))
        );
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn insert_replaces_lazy_value_of_same_name() {
        let builds = Arc::new(AtomicUsize::new(0));
        let mut ctx = ExpressionContext::new();
        ctx.insert_lazy("output", counting(&builds, json!("lazy")));
        ctx.insert("output", Arc::new(json!("eager")));
        assert_eq!(ctx.get("output").unwrap(), Some(Arc::new(json!("eager"))));
        assert_eq!(builds.load(Ordering::SeqCst), 0);
        ctx.insert_lazy("output", counting(&builds, json!("lazy again")));
        assert_eq!(
            ctx.get("output").unwrap(),
            Some(Arc::new(json!("lazy again")))
        );
    }

    #[test]
    fn insert_on_clone_does_not_affect_original() {
        let mut original = ExpressionContext::new();
        original.insert("secrets", Arc::new(json!({})));
        let mut policy = original.clone();
        policy.insert("secrets", Arc::new(json!({"TOKEN": "x"})));
        policy.insert("extra", Arc::new(json!(1)));
        assert_eq!(original.get("secrets").unwrap(), Some(Arc::new(json!({}))));
        assert_eq!(original.get("extra").unwrap(), None);
    }

    #[test]
    fn resolve_subset_skips_unknown_names() {
        let mut ctx = ExpressionContext::new();
        ctx.insert("a", Arc::new(json!(1)));
        let subset = ctx.resolve_subset(["a", "missing"]).unwrap();
        assert_eq!(
            subset,
            BTreeMap::from([("a".to_owned(), Arc::new(json!(1)))])
        );
    }

    #[test]
    fn from_iter_builds_eager_values() {
        let ctx: ExpressionContext = vec![("a".to_owned(), Arc::new(json!(1)))]
            .into_iter()
            .collect();
        assert_eq!(ctx.get("a").unwrap(), Some(Arc::new(json!(1))));
    }

    #[test]
    fn build_errors_are_reported_and_retried() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut ctx = ExpressionContext::new();
        let a = attempts.clone();
        ctx.insert_lazy("broken", move || {
            a.fetch_add(1, Ordering::SeqCst);
            serde_json::from_str::<Value>("{")
        });
        let err = ctx.get("broken").expect_err("build error must surface");
        assert!(err.title.unwrap_or_default().contains("$broken"));
        assert!(ctx.get("broken").is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn names_lists_eager_and_lazy_without_building() {
        let builds = Arc::new(AtomicUsize::new(0));
        let mut ctx = ExpressionContext::new();
        ctx.insert("item", Arc::new(json!(1)));
        ctx.insert_lazy("context", counting(&builds, json!({})));
        let names = ctx.names();
        assert_eq!(names, vec!["context", "item"]);
        assert_eq!(builds.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn debug_does_not_force_lazy_values() {
        let builds = Arc::new(AtomicUsize::new(0));
        let mut ctx = ExpressionContext::new();
        ctx.insert_lazy("workflow", counting(&builds, json!(1)));
        let printed = format!("{ctx:?}");
        assert!(printed.contains("workflow"));
        assert_eq!(builds.load(Ordering::SeqCst), 0);
    }
}
