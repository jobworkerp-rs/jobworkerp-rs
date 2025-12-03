//! MistralRS model loading functionality
//!
//! Ported from app-wrapper/src/llm/mistral/model.rs

use anyhow::Result;
use async_trait::async_trait;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
    local_runner_settings::IsqType, LocalRunnerSettings,
};
use mistralrs::{
    AutoDeviceMapParams, DeviceMapSetting, GgufModelBuilder, IsqType as MistralIsqType, Model,
    PagedAttentionMetaBuilder, TextModelBuilder,
};

/// Trait for loading LLM models from settings
#[async_trait]
pub trait MistralModelLoader {
    async fn load_model(mistral_settings: &LocalRunnerSettings) -> Result<Model> {
        match &mistral_settings.model_settings {
            Some(model_settings) => match model_settings {
                // TextModel implementation
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::ModelSettings::TextModel(
                    text_model,
                ) => {
                    let mut builder = TextModelBuilder::new(&text_model.model_name_or_path);

                    if let Some(isq_type) = &text_model.isq_type {
                        let mistral_isq = Self::convert_isq_type(*isq_type);
                        builder = builder.with_isq(mistral_isq);
                    }

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

                    if text_model.with_paged_attn {
                        builder = builder
                            .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())?;
                    }

                    if let Some(adm) = &mistral_settings.auto_device_map {
                        let auto_map_params = AutoDeviceMapParams::Text {
                            max_seq_len: adm.max_seq_len as usize,
                            max_batch_size: adm.max_batch_size as usize,
                        };
                        builder = builder
                            .with_device_mapping(DeviceMapSetting::Auto(auto_map_params));
                    }

                    tracing::info!("Building text model: {}", text_model.model_name_or_path);
                    let model = builder.build().await?;
                    tracing::info!("Text model initialized successfully");
                    Ok(model)
                }

                // GgufModel implementation
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::local_runner_settings::ModelSettings::GgufModel(
                    gguf_model,
                ) => {
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

                    if let Some(tok_id) = &gguf_model.tok_model_id {
                        builder = builder.with_tok_model_id(tok_id);
                    }

                    if gguf_model.with_logging {
                        builder = builder.with_logging();
                    }

                    if gguf_model.with_paged_attn {
                        builder = builder
                            .with_paged_attn(|| PagedAttentionMetaBuilder::default().build())?;
                    }

                    if let Some(adm) = &mistral_settings.auto_device_map {
                        let auto_map_params = AutoDeviceMapParams::Text {
                            max_seq_len: adm.max_seq_len as usize,
                            max_batch_size: adm.max_batch_size as usize,
                        };
                        builder = builder
                            .with_device_mapping(DeviceMapSetting::Auto(auto_map_params));
                    }

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
            None => Err(anyhow::anyhow!("No model settings provided")),
        }
    }

    fn convert_isq_type(isq_type: i32) -> MistralIsqType {
        match IsqType::try_from(isq_type).unwrap_or(IsqType::Unspecified) {
            IsqType::Unspecified => MistralIsqType::Q4K,
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
