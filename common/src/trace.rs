use std::fmt::Debug;

use opentelemetry::global;
use opentelemetry::{
    propagation::Extractor,
    trace::{Span, Tracer},
    KeyValue,
};
use tonic::Request;

struct MetadataMap<'a>(&'a tonic::metadata::MetadataMap);

impl<'a> Extractor for MetadataMap<'a> {
    /// Get a value for a key from the MetadataMap.  If the value can't be converted to &str, returns None
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|metadata| metadata.to_str().ok())
    }

    /// Collect all the keys from the MetadataMap.
    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(|key| match key {
                tonic::metadata::KeyRef::Ascii(v) => v.as_str(),
                tonic::metadata::KeyRef::Binary(v) => v.as_str(),
            })
            .collect::<Vec<_>>()
    }
}

pub trait Tracing {
    fn trace_request<T: Debug>(
        name: &'static str,
        span_name: &'static str,
        request: &Request<T>,
    ) -> global::BoxedSpan {
        let parent_cx =
            global::get_text_map_propagator(|prop| prop.extract(&MetadataMap(request.metadata())));
        let mut span = global::tracer(name).start_with_context(span_name, &parent_cx);
        span.set_attribute(KeyValue::new("request", format!("{:?}", request)));
        span
    }
}
