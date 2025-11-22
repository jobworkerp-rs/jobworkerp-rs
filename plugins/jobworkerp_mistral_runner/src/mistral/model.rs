use crate::mistral_runner::{
    mistral_runner_settings::{self, IsqType},
    MistralRunnerSettings,
};
use anyhow::Result;
use async_trait::async_trait;
use jobworkerp_base::error::JobWorkerError;
use mistralrs::{
    AutoDeviceMapParams, DeviceMapSetting, GgufModelBuilder, IsqType as MistralIsqType, Model,
    PagedAttentionMetaBuilder, TextModelBuilder,
};

/// Trait for loading LLM models from settings
#[async_trait]
pub trait MistralModelLoader {
    async fn load_model(mistral_settings: &MistralRunnerSettings) -> Result<Model> {
        // Extract model settings
        match &mistral_settings.model_settings {
            Some(model_settings) => match model_settings {
                // TextModel implementation
                mistral_runner_settings::ModelSettings::TextModel(text_model) => {
                    // Initialize TextModelBuilder
                    let mut builder = TextModelBuilder::new(&text_model.model_name_or_path);

                    // Set ISQ type if specified
                    if let Some(isq_type) = text_model.isq_type {
                        let mistral_isq = Self::convert_isq_type(isq_type);
                        builder = builder.with_isq(mistral_isq);
                    }

                    // Set logging if enabled
                    if text_model.with_logging {
                        builder = builder.with_logging();
                    }
                    if let Some(chat_template) = &text_model.chat_template {
                        if chat_template.trim_end().ends_with(".json") {
                            builder = builder.with_chat_template(chat_template);
                        } else if chat_template.trim_end().ends_with(".jinja") {
                            builder = builder.with_jinja_explicit(chat_template.to_string());
                        } else {
                            tracing::warn!(
                                "Invalid chat template provided: {}, ignored.",
                                chat_template
                            );
                        }
                    }

                    // Setup paged attention if enabled
                    if text_model.with_paged_attn {
                        builder = builder
                            .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())?;
                    }

                    // Set auto device mapping if configured
                    if let Some(adm) = &mistral_settings.auto_device_map {
                        let auto_map_params = AutoDeviceMapParams::Text {
                            max_seq_len: adm.max_seq_len as usize,
                            max_batch_size: adm.max_batch_size as usize,
                        };
                        builder =
                            builder.with_device_mapping(DeviceMapSetting::Auto(auto_map_params));
                    }

                    // Build the model
                    tracing::info!("Building text model: {}", text_model.model_name_or_path);
                    let model = builder.build().await?;
                    tracing::info!("Text model initialized successfully");
                    Ok(model)
                }

                // GgufModel implementation
                mistral_runner_settings::ModelSettings::GgufModel(gguf_model) => {
                    // Initialize GgufModelBuilder
                    let mut builder = GgufModelBuilder::new(
                        &gguf_model.model_name_or_path,
                        gguf_model.gguf_files.clone(),
                    );
                    if let Some(chat_template) = &gguf_model.chat_template {
                        if chat_template.trim_end().ends_with(".json") {
                            builder = builder.with_chat_template(chat_template);
                        } else if chat_template.trim_end().ends_with(".jinja") {
                            builder = builder.with_jinja_explicit(chat_template.to_string());
                        } else {
                            tracing::warn!(
                                "Invalid chat template provided: {}, ignored.",
                                chat_template
                            );
                        }
                    }

                    // Set tokenizer model ID if specified
                    if let Some(tok_id) = &gguf_model.tok_model_id {
                        builder = builder.with_tok_model_id(tok_id);
                    }

                    // Set logging if enabled
                    if gguf_model.with_logging {
                        builder = builder.with_logging();
                    }

                    // Setup paged attention if enabled (same logic as TextModel)
                    if gguf_model.with_paged_attn {
                        builder = builder
                            .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())?;
                    }

                    // Set auto device mapping if configured
                    if let Some(adm) = &mistral_settings.auto_device_map {
                        let auto_map_params = AutoDeviceMapParams::Text {
                            max_seq_len: adm.max_seq_len as usize,
                            max_batch_size: adm.max_batch_size as usize,
                        };
                        builder =
                            builder.with_device_mapping(DeviceMapSetting::Auto(auto_map_params));
                    }

                    // Build the model
                    tracing::info!(
                        "Building GGUF model: {} with files: {:?}",
                        gguf_model.model_name_or_path,
                        gguf_model.gguf_files
                    );
                    let model = builder.build().await?;
                    tracing::info!("GGUF model initialized successfully");
                    Ok(model)
                }
            },
            None => Err(
                JobWorkerError::InvalidParameter("No model settings provided".to_string()).into(),
            ),
        }
    }

    fn convert_isq_type(isq_type: i32) -> MistralIsqType {
        match IsqType::try_from(isq_type).unwrap_or(IsqType::Unspecified) {
            IsqType::Unspecified => MistralIsqType::Q4K, // Default value
            IsqType::Q40 => MistralIsqType::Q4_0,
            IsqType::Q41 => MistralIsqType::Q4_1,
            IsqType::Q50 => MistralIsqType::Q5_0,
            IsqType::Q51 => MistralIsqType::Q5_1,
            IsqType::Q80 => MistralIsqType::Q8_0,
            IsqType::Q81 => MistralIsqType::Q8_1,
            IsqType::Q2k => MistralIsqType::Q2K,
            IsqType::Q3k => MistralIsqType::Q3K,
            IsqType::Q4k => MistralIsqType::Q4K,
            IsqType::Q5k => MistralIsqType::Q5K,
            IsqType::Q6k => MistralIsqType::Q6K,
            IsqType::Q8k => MistralIsqType::Q8K,
            IsqType::Hqq8 => MistralIsqType::HQQ8,
            IsqType::Hqq4 => MistralIsqType::HQQ4,
            IsqType::F8e4m3 => MistralIsqType::F8E4M3,
        }
    }
}
