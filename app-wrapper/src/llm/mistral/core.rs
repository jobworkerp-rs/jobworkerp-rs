use anyhow::Result;
use mistralrs::{Model, RequestBuilder};
use std::sync::Arc;

/// Core trait for MistralRS LLM functionality
/// 
/// This trait abstracts the basic LLM operations that are common across both
/// Chat and Completion APIs, providing a clean interface for model interaction.
pub trait MistralCoreService {
    /// Send a chat request to the MistralRS model
    async fn request_chat(
        &self,
        request_builder: RequestBuilder,
    ) -> Result<mistralrs::ChatCompletionResponse>;
    
    /// Stream a chat request to the MistralRS model
    async fn stream_chat(
        &self,
        request_builder: RequestBuilder,
    ) -> Result<futures::stream::BoxStream<'static, mistralrs::Response>>;
    
    /// Get reference to the underlying model
    fn model(&self) -> &Arc<Model>;
    
    /// Get the model name for identification purposes
    fn model_name(&self) -> &str;
}