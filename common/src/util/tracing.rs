use anyhow::{Context, Result};
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{BatchConfig, Tracer};
use serde::Deserialize;
use std::env;
use std::fs::File;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tracing::Subscriber;
// use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::layer::SubscriberExt;
// use opentelemetry::sdk::export::trace::stdout;
// use opentelemetry::{
//     propagation::Extractor,
//     trace::{Span, Tracer},
//     KeyValue,
// };
use crate::util::id_generator::iputil;
use opentelemetry_semantic_conventions::{
    resource::{DEPLOYMENT_ENVIRONMENT, SERVICE_NAME, SERVICE_VERSION},
    SCHEMA_URL,
};
use tracing_subscriber::{filter, prelude::*};

const APP_SERVICE_NAME: &str = "jobworkerp-rs";

#[derive(Deserialize, Debug)]
pub struct LoggingConfig {
    pub level: Option<String>,
    pub file_name: Option<String>,
    pub file_dir: Option<String>,
    pub use_json: bool,
    pub use_stdout: bool,
}

impl LoggingConfig {
    pub fn new() -> Self {
        Self {
            level: None,
            file_name: None,
            file_dir: None,
            use_json: false,
            use_stdout: true,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        tracing::info!("Use default LoggingConfig.");
        Self::new()
    }
}

pub async fn init_from_env_and_filename(
    prefix: impl Into<String>,
    ext: impl Into<String>,
) -> Result<()> {
    let log_filename = create_filename_with_ip_postfix(prefix, ext);
    // no env, use default
    let conf = load_tracing_config_from_env().unwrap_or_default();
    tracing_init(LoggingConfig {
        file_name: Some(log_filename),
        ..conf
    })
    .await
}
pub fn create_filename_with_ip_postfix(
    prefix: impl Into<String>,
    ext: impl Into<String>,
) -> String {
    // filename based on ip
    let ip = iputil::resolve_host_ipv4().unwrap_or(*iputil::IP_LOCAL);
    format!("{}_{:x}.{}", prefix.into(), u32::from(ip.ip()), ext.into())
}
pub fn load_tracing_config_from_env() -> Result<LoggingConfig> {
    envy::prefixed("LOG_")
        .from_env::<LoggingConfig>()
        .context("cannot read logging config from env:")
}
pub async fn tracing_init(conf: LoggingConfig) -> Result<()> {
    let layer = setup_layer_from_logging_config(&conf).await?;
    tracing::subscriber::set_global_default(layer).context("setting default subscriber failed")?;
    Ok(())
}
pub async fn tracing_init_from_env() -> Result<()> {
    match load_tracing_config_from_env() {
        Ok(conf) => tracing_init(conf).await,
        Err(e) => {
            tracing::warn!("failed to load logging config from env: {:?}", e);
            Err(e)
        }
    }
}
// TODO match type
fn jaeger_tracer_from_env() -> Result<Tracer> {
    let addr = env::var("JAEGER_ADDR").context("jaeger addr")?;
    println!("jaeger addr: {:?}", addr);
    opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name(APP_SERVICE_NAME)
        .with_endpoint(addr.to_string())
        .install_batch(opentelemetry_sdk::runtime::Tokio)
        .map_err(|e| {
            println!("failed to install zipkin tracer: {:?}", e);
            e.into()
        })
}

fn zipkin_tracer_from_env() -> Result<Tracer> {
    global::set_text_map_propagator(opentelemetry_zipkin::Propagator::new());
    let addr = env::var("ZIPKIN_ADDR").context("zipkin addr")?;
    println!("zipkin addr: {:?}", &addr);
    opentelemetry_zipkin::new_pipeline()
        .with_service_name(APP_SERVICE_NAME)
        .with_collector_endpoint(addr)
        .install_batch(opentelemetry_sdk::runtime::Tokio)
        .map_err(|e| {
            println!("failed to install zipkin tracer: {:?}", e);
            e.into()
        })
}

// Create a Resource that captures information about the entity for which telemetry is recorded.
fn resource() -> opentelemetry_sdk::Resource {
    opentelemetry_sdk::Resource::from_schema_url(
        [
            KeyValue::new(SERVICE_NAME, APP_SERVICE_NAME),
            KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
            KeyValue::new(DEPLOYMENT_ENVIRONMENT, "development"), // TODO from config
        ],
        SCHEMA_URL,
    )
}
async fn otlp_tracer_from_env() -> Result<Option<Tracer>> {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let addr: Result<String> = env::var("OTLP_ADDR").context("otlp addr");
    match addr {
        Ok(addr) => {
            println!("otlp addr: {:?}", &addr);
            match opentelemetry_otlp::new_pipeline()
                .tracing()
                .with_trace_config(
                    opentelemetry_sdk::trace::Config::default()
                        // Customize sampling strategy
                        .with_sampler(opentelemetry_sdk::trace::Sampler::ParentBased(Box::new(
                            opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(1.0),
                        )))
                        // If export trace to AWS X-Ray, you can use XrayIdGenerator
                        .with_id_generator(opentelemetry_sdk::trace::RandomIdGenerator::default())
                        .with_resource(resource()),
                )
                .with_batch_config(BatchConfig::default())
                .with_exporter(
                    opentelemetry_otlp::new_exporter()
                        .tonic()
                        .with_endpoint(&addr)
                        .with_timeout(Duration::from_secs(3)),
                )
                .install_batch(opentelemetry_sdk::runtime::Tokio)
            {
                Ok(tr) => Ok(Some(tr)),
                Err(e) => {
                    println!("failed to install otlp tracer: {:?}", e);
                    Err(e.into())
                }
            }
        }
        Err(_e) => {
            // not specified
            // println!("failed to load otlp config from env: {:?}", _e);
            Ok(None)
        }
    }
}

pub async fn setup_layer_from_logging_config(
    conf: &LoggingConfig,
) -> Result<Box<dyn Subscriber + Send + Sync + 'static>> {
    let lv = tracing::Level::from_str(conf.level.as_ref().unwrap_or(&"INFO".to_string()).as_str())
        .unwrap_or(tracing::Level::INFO);
    let filter = filter::Targets::new().with_default(lv);
    let dir = conf
        .file_dir
        .as_ref()
        .map(|d| PathBuf::from_str(d).context("invalid file_dir"))
        .unwrap_or(env::current_dir().map_err(|e| e.into()))?;
    let default_name = "out.log";
    let file_name = conf.file_name.as_deref().unwrap_or(default_name);
    let file = File::create(dir.join(file_name))?;
    let layer = Layer::new()
        .with_writer(file.with_max_level(lv))
        .with_ansi(false);
    let remote_tracer = match jaeger_tracer_from_env().or_else(|_| zipkin_tracer_from_env()) {
        Ok(tr) => Some(tr),
        Err(_) => otlp_tracer_from_env().await?,
    };
    let subscriber: Box<dyn Subscriber + Send + Sync> = if conf.use_json {
        let s = tracing_subscriber::registry()
            .with(layer.json())
            .with(filter);
        // pretty format for stdout (XXX fixed)
        if conf.use_stdout {
            if let Some(tracer) = remote_tracer {
                // for type match
                Box::new(
                    s.with(tracing_opentelemetry::layer().with_tracer(tracer))
                        .with(tracing_subscriber::fmt::layer().json()),
                )
            } else {
                Box::new(s.with(tracing_subscriber::fmt::layer().json()))
            }
        } else {
            Box::new(s)
        }
    } else if conf.use_stdout {
        // for debug
        if let Some(tracer) = remote_tracer {
            Box::new(
                tracing_subscriber::registry()
                    .with(layer)
                    .with(filter)
                    .with(tracing_opentelemetry::layer().with_tracer(tracer))
                    .with(tracing_subscriber::fmt::layer().pretty()),
            )
        } else {
            Box::new(
                tracing_subscriber::registry()
                    .with(layer)
                    .with(filter)
                    .with(tracing_subscriber::fmt::layer().pretty()),
            )
        }
    } else {
        Box::new(tracing_subscriber::registry().with(layer).with(filter))
    };
    // // TODO match type
    // if let Some(tracer) = jaeger_tracer_from_env()? {
    //     subscriber = Box::new(subscriber.with(OpenTelemetryLayer::new(tracer)));
    // }
    // if conf.use_tokio_console {
    // subscriber = Box::new(subscriber.with(console_layer));
    // }
    Ok(subscriber)
}

// use tonic::Request;
// struct MetadataMap<'a>(&'a tonic::metadata::MetadataMap);

// impl<'a> Extractor for MetadataMap<'a> {
//     /// Get a value for a key from the MetadataMap.  If the value can't be converted to &str, returns None
//     fn get(&self, key: &str) -> Option<&str> {
//         self.0.get(key).and_then(|metadata| metadata.to_str().ok())
//     }

//     /// Collect all the keys from the MetadataMap.
//     fn keys(&self) -> Vec<&str> {
//         self.0
//             .keys()
//             .map(|key| match key {
//                 tonic::metadata::KeyRef::Ascii(v) => v.as_str(),
//                 tonic::metadata::KeyRef::Binary(v) => v.as_str(),
//             })
//             .collect::<Vec<_>>()
//     }
// }

// pub trait Tracing {
//     fn trace_request<'a, T: Debug>(
//         name: &'static str,
//         span_name: &'static str,
//         request: &'a Request<T>,
//     ) -> global::BoxedSpan {
//         let parent_cx =
//             global::get_text_map_propagator(|prop| prop.extract(&MetadataMap(request.metadata())));
//         let mut span = global::tracer(name).start_with_context(span_name, &parent_cx);
//         span.set_attribute(KeyValue::new("request", format!("{:?}", request)));
//         span
//     }
// }
// for stdout logging
pub fn tracing_init_test(level: tracing::Level) {
    tracing_subscriber::fmt().with_max_level(level).init();
}

// for jeager logging
pub fn tracing_jaeger_init(addr: &SocketAddr, name: &str, level: String) -> Result<()> {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let stdout_log = tracing_subscriber::fmt::layer().pretty();
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name(name)
        .with_endpoint(addr.to_string())
        .install_simple()?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(level))
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .with(stdout_log)
        .try_init()
        .map_err(|e| e.into())
}
pub fn tracing_jaeger_init_batch(addr: &SocketAddr, name: &str) -> Result<()> {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let stdout_log = tracing_subscriber::fmt::layer().pretty();
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name(name)
        .with_endpoint(addr.to_string())
        .install_batch(opentelemetry_sdk::runtime::Tokio)?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("INFO"))
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .with(stdout_log)
        .try_init()
        .map_err(|e| e.into())
}

// for zipkin logging
pub fn tracing_zipkin_init(addr: impl Into<String>, name: &str) -> Result<()> {
    global::set_text_map_propagator(opentelemetry_zipkin::Propagator::new());
    let stdout_log = tracing_subscriber::fmt::layer().pretty();
    let tracer = opentelemetry_zipkin::new_pipeline()
        .with_service_name(name)
        .with_collector_endpoint(addr)
        .install_simple()?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("INFO"))
        .with(stdout_log)
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .try_init()
        .map_err(|e| e.into())
}
// for zipkin logging
pub fn tracing_zipkin_init_batch(addr: impl Into<String>, name: &str) -> Result<()> {
    global::set_text_map_propagator(opentelemetry_zipkin::Propagator::new());
    let stdout_log = tracing_subscriber::fmt::layer().pretty();
    let tracer = opentelemetry_zipkin::new_pipeline()
        .with_service_name(name)
        .with_collector_endpoint(addr)
        .install_batch(opentelemetry_sdk::runtime::Tokio)?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("INFO"))
        .with(stdout_log)
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .try_init()
        .map_err(|e| e.into())
}
