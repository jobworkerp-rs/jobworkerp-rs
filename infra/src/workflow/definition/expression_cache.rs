//! Process-wide caches for compiled workflow expressions (jq filters and
//! Liquid templates), sized from `WORKFLOW_*` environment variables or, by
//! default, from the memory available to the process.

use command_utils::util::{
    cache::checked_time_to_idle,
    jq::{JqFilterCache, JqFilterCacheConfig},
};
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

const MIB: u64 = 1024 * 1024;

/// Share of available memory given to each cache by default, with bounds so
/// small containers still cache common expressions and large hosts do not
/// reserve memory a workflow engine will never fill.
const JQ_DEFAULT_MEMORY_DIVISOR: u64 = 64;
const JQ_DEFAULT_MIN_BYTES: u64 = 16 * MIB;
const JQ_DEFAULT_MAX_BYTES: u64 = 256 * MIB;
const JQ_FALLBACK_BYTES: u64 = 32 * MIB;
const LIQUID_DEFAULT_MEMORY_DIVISOR: u64 = 512;
const LIQUID_DEFAULT_MIN_BYTES: u64 = 4 * MIB;
const LIQUID_DEFAULT_MAX_BYTES: u64 = 32 * MIB;
const LIQUID_FALLBACK_BYTES: u64 = 8 * MIB;
const DEFAULT_TIME_TO_IDLE: Duration = Duration::from_secs(30 * 60);

/// Measured with liquid 0.26: a parsed template holds 7-12 bytes per byte of
/// template text; 12 plus a fixed overhead is used to stay on the safe side.
const LIQUID_TEMPLATE_BASE_BYTES: u64 = 256;
const LIQUID_TEMPLATE_BYTES_PER_CHAR: u64 = 12;

const JQ_CACHE_MAX_BYTES_VAR: &str = "WORKFLOW_JQ_CACHE_MAX_BYTES";
const LIQUID_CACHE_MAX_BYTES_VAR: &str = "WORKFLOW_LIQUID_CACHE_MAX_BYTES";
const EXPRESSION_CACHE_TTI_SEC_VAR: &str = "WORKFLOW_EXPRESSION_CACHE_TTI_SEC";

/// Raw settings from the environment. Unset values fall back to defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpressionCacheEnv {
    pub jq_cache_max_bytes: Option<u64>,
    pub liquid_cache_max_bytes: Option<u64>,
    pub expression_cache_tti_sec: Option<u64>,
}

impl ExpressionCacheEnv {
    fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Each variable is parsed on its own so one malformed value does not
    /// discard the others.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let parse = |name: &str| {
            let raw = lookup(name)?;
            let raw = raw.trim();
            if raw.is_empty() {
                return None;
            }
            raw.parse::<u64>()
                .inspect_err(|e| {
                    tracing::warn!("Invalid {name}={raw:?} ({e}); using the default instead")
                })
                .ok()
        };
        Self {
            jq_cache_max_bytes: parse(JQ_CACHE_MAX_BYTES_VAR),
            liquid_cache_max_bytes: parse(LIQUID_CACHE_MAX_BYTES_VAR),
            expression_cache_tti_sec: parse(EXPRESSION_CACHE_TTI_SEC_VAR),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionCacheConfig {
    pub jq_max_bytes: u64,
    pub liquid_max_bytes: u64,
    /// `None` disables idle expiry.
    pub time_to_idle: Option<Duration>,
}

impl ExpressionCacheConfig {
    /// Resolve settings, deriving unset limits from `available_memory`
    /// (bytes; `None` when it cannot be determined).
    pub fn resolve(env: &ExpressionCacheEnv, available_memory: Option<u64>) -> Self {
        let share = |divisor: u64, min: u64, max: u64, fallback: u64| {
            available_memory
                .filter(|m| *m > 0)
                .map(|m| (m / divisor).clamp(min, max))
                .unwrap_or(fallback)
        };
        Self {
            jq_max_bytes: env.jq_cache_max_bytes.unwrap_or_else(|| {
                share(
                    JQ_DEFAULT_MEMORY_DIVISOR,
                    JQ_DEFAULT_MIN_BYTES,
                    JQ_DEFAULT_MAX_BYTES,
                    JQ_FALLBACK_BYTES,
                )
            }),
            liquid_max_bytes: env.liquid_cache_max_bytes.unwrap_or_else(|| {
                share(
                    LIQUID_DEFAULT_MEMORY_DIVISOR,
                    LIQUID_DEFAULT_MIN_BYTES,
                    LIQUID_DEFAULT_MAX_BYTES,
                    LIQUID_FALLBACK_BYTES,
                )
            }),
            time_to_idle: match env.expression_cache_tti_sec {
                Some(0) => None,
                Some(sec) => Some(Duration::from_secs(sec)),
                None => Some(DEFAULT_TIME_TO_IDLE),
            },
        }
    }
}

/// Memory the process may use: the cgroup limit when running in a container
/// with a lower limit than the host, otherwise the physical memory.
fn available_memory() -> Option<u64> {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    effective_memory(
        Some(system.total_memory()),
        system.cgroup_limits().map(|c| c.total_memory),
    )
}

fn effective_memory(total: Option<u64>, cgroup: Option<u64>) -> Option<u64> {
    let total = total.filter(|m| *m > 0);
    let cgroup = cgroup.filter(|m| *m > 0);
    match (total, cgroup) {
        (Some(t), Some(c)) => Some(t.min(c)),
        (t, c) => t.or(c),
    }
}

static CONFIG: LazyLock<ExpressionCacheConfig> = LazyLock::new(|| {
    let config =
        ExpressionCacheConfig::resolve(&ExpressionCacheEnv::from_env(), available_memory());
    tracing::info!(
        jq_max_bytes = config.jq_max_bytes,
        liquid_max_bytes = config.liquid_max_bytes,
        time_to_idle_sec = config.time_to_idle.map(|d| d.as_secs()),
        "workflow expression cache configured"
    );
    config
});

pub(crate) static JQ_FILTERS: LazyLock<JqFilterCache> = LazyLock::new(|| {
    JqFilterCache::new(JqFilterCacheConfig {
        max_bytes: CONFIG.jq_max_bytes,
        time_to_idle: CONFIG.time_to_idle,
    })
});

pub(crate) static LIQUID_TEMPLATES: LazyLock<LiquidTemplateCache> =
    LazyLock::new(|| new_liquid_template_cache(CONFIG.liquid_max_bytes, CONFIG.time_to_idle));

/// Parsed Liquid templates keyed by template body. Only the parse result is
/// cached; variables are bound on every render.
pub(crate) type LiquidTemplateCache = moka::sync::Cache<String, Arc<liquid::Template>>;

fn estimated_template_bytes(body: &str) -> u32 {
    let bytes = LIQUID_TEMPLATE_BASE_BYTES + LIQUID_TEMPLATE_BYTES_PER_CHAR * body.len() as u64;
    u32::try_from(bytes).unwrap_or(u32::MAX)
}

fn new_liquid_template_cache(
    max_bytes: u64,
    time_to_idle: Option<Duration>,
) -> LiquidTemplateCache {
    let mut builder = moka::sync::Cache::builder()
        .max_capacity(max_bytes)
        .weigher(|body: &String, _| estimated_template_bytes(body));
    if let Some(tti) = checked_time_to_idle(time_to_idle) {
        builder = builder.time_to_idle(tti);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from(vars: &[(&str, &str)]) -> ExpressionCacheEnv {
        let vars: std::collections::HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ExpressionCacheEnv::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn env_vars_are_parsed() {
        let env = env_from(&[
            (JQ_CACHE_MAX_BYTES_VAR, "1048576"),
            (LIQUID_CACHE_MAX_BYTES_VAR, " 2048 "),
            (EXPRESSION_CACHE_TTI_SEC_VAR, "60"),
            ("WORKFLOW_CHECKPOINT_MAX_COUNT", "10"),
        ]);
        let config = ExpressionCacheConfig::resolve(&env, Some(64 * 1024 * MIB));
        assert_eq!(
            config,
            ExpressionCacheConfig {
                jq_max_bytes: 1048576,
                liquid_max_bytes: 2048,
                time_to_idle: Some(Duration::from_secs(60)),
            }
        );
    }

    #[test]
    fn invalid_value_falls_back_only_for_that_variable() {
        let env = env_from(&[
            (JQ_CACHE_MAX_BYTES_VAR, "64MB"),
            (LIQUID_CACHE_MAX_BYTES_VAR, "2048"),
            (EXPRESSION_CACHE_TTI_SEC_VAR, "-1"),
        ]);
        assert_eq!(
            env,
            ExpressionCacheEnv {
                jq_cache_max_bytes: None,
                liquid_cache_max_bytes: Some(2048),
                expression_cache_tti_sec: None,
            }
        );
    }

    #[test]
    fn empty_value_is_treated_as_unset() {
        let env = env_from(&[(JQ_CACHE_MAX_BYTES_VAR, "  ")]);
        assert_eq!(env, ExpressionCacheEnv::default());
    }

    #[test]
    fn zero_tti_disables_idle_expiry_and_unset_uses_default() {
        let zero = env_from(&[(EXPRESSION_CACHE_TTI_SEC_VAR, "0")]);
        assert_eq!(
            ExpressionCacheConfig::resolve(&zero, None).time_to_idle,
            None
        );
        let unset = ExpressionCacheEnv::default();
        assert_eq!(
            ExpressionCacheConfig::resolve(&unset, None).time_to_idle,
            Some(DEFAULT_TIME_TO_IDLE)
        );
    }

    #[test]
    fn oversized_tti_does_not_panic_when_building_caches() {
        let env = ExpressionCacheEnv {
            expression_cache_tti_sec: Some(u64::MAX),
            ..Default::default()
        };
        let config = ExpressionCacheConfig::resolve(&env, None);
        let liquid = new_liquid_template_cache(MIB, config.time_to_idle);
        liquid.insert(
            "x".to_string(),
            Arc::new(
                liquid::ParserBuilder::with_stdlib()
                    .build()
                    .unwrap()
                    .parse("x")
                    .unwrap(),
            ),
        );
        let _jq = JqFilterCache::new(JqFilterCacheConfig {
            max_bytes: MIB,
            time_to_idle: config.time_to_idle,
        });
    }

    #[test]
    fn defaults_scale_with_memory_within_bounds() {
        let env = ExpressionCacheEnv::default();
        // 4 GiB: 1/64 = 64 MiB for jq, 1/512 = 8 MiB for Liquid.
        let mid = ExpressionCacheConfig::resolve(&env, Some(4 * 1024 * MIB));
        assert_eq!(mid.jq_max_bytes, 64 * MIB);
        assert_eq!(mid.liquid_max_bytes, 8 * MIB);
        // 256 MiB container: lower bounds apply.
        let small = ExpressionCacheConfig::resolve(&env, Some(256 * MIB));
        assert_eq!(small.jq_max_bytes, JQ_DEFAULT_MIN_BYTES);
        assert_eq!(small.liquid_max_bytes, LIQUID_DEFAULT_MIN_BYTES);
        // 1 TiB host: upper bounds apply.
        let large = ExpressionCacheConfig::resolve(&env, Some(1024 * 1024 * MIB));
        assert_eq!(large.jq_max_bytes, JQ_DEFAULT_MAX_BYTES);
        assert_eq!(large.liquid_max_bytes, LIQUID_DEFAULT_MAX_BYTES);
    }

    #[test]
    fn unknown_memory_uses_fallback() {
        let env = ExpressionCacheEnv::default();
        for memory in [None, Some(0)] {
            let config = ExpressionCacheConfig::resolve(&env, memory);
            assert_eq!(config.jq_max_bytes, JQ_FALLBACK_BYTES);
            assert_eq!(config.liquid_max_bytes, LIQUID_FALLBACK_BYTES);
        }
    }

    #[test]
    fn effective_memory_prefers_lower_cgroup_limit() {
        assert_eq!(
            effective_memory(Some(16 * MIB), Some(4 * MIB)),
            Some(4 * MIB)
        );
        // cgroup "max" may surface as a value above physical memory.
        assert_eq!(
            effective_memory(Some(16 * MIB), Some(u64::MAX)),
            Some(16 * MIB)
        );
        assert_eq!(effective_memory(Some(16 * MIB), None), Some(16 * MIB));
        assert_eq!(effective_memory(None, Some(4 * MIB)), Some(4 * MIB));
        assert_eq!(effective_memory(Some(0), Some(0)), None);
    }

    #[test]
    fn available_memory_is_detected_on_this_host() {
        assert!(available_memory().is_some_and(|m| m > 0));
    }

    #[test]
    fn liquid_cache_respects_memory_limit() {
        let body = |i: usize| format!("{{{{ name }}}} {i:04}");
        let per_entry = u64::from(estimated_template_bytes(&body(0)));
        let cache = new_liquid_template_cache(2 * per_entry, None);
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        for i in 0..10 {
            cache.insert(body(i), Arc::new(parser.parse(&body(i)).unwrap()));
        }
        cache.run_pending_tasks();
        assert!(cache.entry_count() <= 2, "entries: {}", cache.entry_count());
    }

    #[test]
    fn liquid_cache_evicts_idle_entries() {
        let cache = new_liquid_template_cache(MIB, Some(Duration::from_millis(50)));
        let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
        cache.insert("x".to_string(), Arc::new(parser.parse("x").unwrap()));
        cache.run_pending_tasks();
        assert_eq!(cache.entry_count(), 1);
        std::thread::sleep(Duration::from_millis(200));
        cache.run_pending_tasks();
        assert_eq!(cache.entry_count(), 0);
    }
}
