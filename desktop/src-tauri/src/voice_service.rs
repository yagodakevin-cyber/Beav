use base64::Engine;
use reqwest::blocking::{multipart, Client};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

use crate::commands::library::persist_media_workspace_catalog;
use crate::helpers::file_url_for_path;
use crate::logging::{
    self,
    event::{LogLevel, LogSource},
};
use crate::persistence::{ensure_store_hydrated_for_subjects, with_store, with_store_mut};
use crate::store::{media as media_store, settings as settings_store, subjects as subject_store};
use crate::{
    ffmpeg_executable, file_content_hash, guess_mime_and_kind, make_id, media_root,
    normalize_legacy_workspace_path, now_iso, now_rfc3339, official_ai_api_key_from_settings,
    official_base_url_from_settings, payload_field, payload_string, persist_subjects_workspace,
    subjects_root, workspace_root, AppState, MediaAssetRecord, SubjectRecord,
};

const ADAPTER_SYSTEM_VOICES_JSON: &str = include_str!("../resources/adapter-system-voices.json");

/// 火山引擎豆包 TTS 2.0（官方网关 `volc-speech-tts` 通道）。
const DOUBAO_TTS_MODEL: &str = "doubao-seed-tts-2.0";
/// 豆包适配通道 CosyVoice 3.5 Flash（官方 `dashscope-cosyvoice-tts` 通道）。
const COSYVOICE_TTS_MODEL: &str = "cosyvoice-v3.5-flash";
const COSYVOICE_CLONE_MODEL: &str = "cosyvoice-v3.5-flash-voice-clone";
/// 豆包适配通道在官方网关模型注册表中的通道模型 ID：网关 /models 仅注册
/// `qwen-audio-3.1-tts-flash`（api_type=dashscope-cosyvoice-tts），应用层模型名
/// 保持用户指定的 `cosyvoice-v3.5-flash`，仅在构造网关请求体时做通道映射。
const GATEWAY_COSYVOICE_CHANNEL_MODEL: &str = "qwen-audio-3.1-tts-flash";
/// 默认音色占位 ID 映射为空串（通道默认音色）；真实音色 ID（豆包官方
/// uranus 2.0 系列 / 百炼复刻音色）原样透传。旧版 1.0 系列音色 ID
/// （mars/moon bigtts，网关已拒绝）一并归一为默认音色，保证存量角色
/// 配置无需迁移即可继续合成。
const ADAPTER_SYSTEM_VOICE_IDS: &[&str] = &[
    "volc-tts-default",
    "cosyvoice-tts-default",
    "zh_female_cancan_mars_bigtts",
    "zh_female_wanwanxiaohe_moon_bigtts",
];

#[derive(Debug, Clone)]
struct VoiceGatewayConfig {
    base_url: String,
    api_key: Option<String>,
    clone_model: String,
    tts_model: String,
}

fn log_voice_clone_event(level: LogLevel, event: &str, message: String, fields: Value) {
    eprintln!("[redbox][voice_clone][{event}] {message} {fields}");
    logging::emit_legacy_line(
        LogSource::Host,
        level,
        "voice_clone",
        event,
        message,
        fields,
        None,
    );
}

fn payload_string_alias(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| payload_string(payload, key))
}

fn normalized_model_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn is_cosyvoice_model(model: &str) -> bool {
    normalized_model_key(model).contains("cosyvoice")
}

/// 网关通道模型映射：应用层模型名不变，适配通道模型在网关侧用注册 ID 发请求。
fn gateway_tts_model(model: &str) -> String {
    if is_cosyvoice_model(model) {
        GATEWAY_COSYVOICE_CHANNEL_MODEL.to_string()
    } else {
        model.trim().to_string()
    }
}

/// 适配层系统音色占位 ID 映射为通道默认音色（网关请求 voice_id 传空串）。
fn wire_voice_id(voice_id: &str) -> String {
    let trimmed = voice_id.trim();
    if trimmed.is_empty() || ADAPTER_SYSTEM_VOICE_IDS.contains(&trimmed) {
        String::new()
    } else {
        trimmed.to_string()
    }
}

/// CosyVoice 走 SSML/`<speak>` 参数，不接受 MiniMax 式 `emotion`；火山豆包 TTS 2.0 支持 `emotion`。
fn tts_model_supports_emotion(model: &str) -> bool {
    !is_cosyvoice_model(model)
}

fn extract_xml_tag_names(input: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = input;
    while let Some(start) = rest.find('<') {
        let after_start = &rest[start + 1..];
        let after_slash = after_start.trim_start_matches('/');
        let Some(first) = after_slash.chars().next() else {
            rest = after_start;
            continue;
        };
        if !first.is_ascii_alphabetic() {
            rest = after_start;
            continue;
        }
        let name: String = after_slash
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            .collect();
        if !name.is_empty() {
            names.push(name.to_ascii_lowercase());
        }
        rest = after_start;
    }
    names
}

fn xml_attr_value(tag: &str, attr: &str) -> Option<String> {
    let lower_tag = tag.to_ascii_lowercase();
    let pattern = format!("{}=", attr.to_ascii_lowercase());
    let attr_start = lower_tag.find(&pattern)? + pattern.len();
    let rest = tag[attr_start..].trim_start();
    let mut chars = rest.chars();
    let first = chars.next()?;
    if first == '"' || first == '\'' {
        let value: String = chars.take_while(|ch| *ch != first).collect();
        return Some(value);
    }
    Some(
        rest.chars()
            .take_while(|ch| !ch.is_whitespace() && *ch != '>' && *ch != '/')
            .collect(),
    )
}

fn validate_cosyvoice_speak_attr(tag: &str, attr: &str, min: f64, max: f64) -> Result<(), String> {
    let Some(raw) = xml_attr_value(tag, attr) else {
        return Ok(());
    };
    let value = raw.parse::<f64>().map_err(|_| {
        format!(
            "CosyVoice SSML `<speak>` attribute `{attr}` must be a number in [{min}, {max}], got `{raw}`"
        )
    })?;
    if !(min..=max).contains(&value) {
        return Err(format!(
            "CosyVoice SSML `<speak>` attribute `{attr}` must be in [{min}, {max}], got `{raw}`"
        ));
    }
    Ok(())
}

fn validate_cosyvoice_speak_volume_scale(tag: &str) -> Result<(), String> {
    let Some(raw) = xml_attr_value(tag, "volume") else {
        return Ok(());
    };
    let value = raw.parse::<f64>().map_err(|_| {
        format!(
            "CosyVoice SSML `<speak>` attribute `volume` must be a number in [0, 100], got `{raw}`"
        )
    })?;
    if value > 0.0 && value < 20.0 {
        return Err(format!(
            "CosyVoice SSML `<speak>` attribute `volume` uses a 0-100 scale, got `{raw}`. Values below 20 are near silent; use about 45-70 for normal narration."
        ));
    }
    Ok(())
}

fn validate_cosyvoice_speak_tags(input: &str) -> Result<(), String> {
    let mut rest = input;
    while let Some(start) = rest.to_ascii_lowercase().find("<speak") {
        let after_start = &rest[start..];
        let Some(end) = after_start.find('>') else {
            return Err("CosyVoice SSML `<speak>` tag is not closed".to_string());
        };
        let tag = &after_start[..=end];
        validate_cosyvoice_speak_attr(tag, "rate", 0.5, 2.0)?;
        validate_cosyvoice_speak_attr(tag, "pitch", 0.5, 2.0)?;
        validate_cosyvoice_speak_attr(tag, "volume", 0.0, 100.0)?;
        validate_cosyvoice_speak_volume_scale(tag)?;
        rest = &after_start[end + 1..];
    }
    Ok(())
}

/// CosyVoice 3.5 Flash 官方规范：只接受 `<speak>` 家族标签，语速/语调为 0.5-2.0 倍数，音量 0-100。
fn validate_cosyvoice_speech_input(model: &str, input: &str) -> Result<(), String> {
    if !is_cosyvoice_model(model) {
        return Ok(());
    }
    let lower = input.to_ascii_lowercase();
    for (needle, reason) in [
        (
            "<prosody",
            "CosyVoice does not support `<prosody>`; use `<speak rate=\"0.9\" pitch=\"0.95\" volume=\"60\">...</speak>` or plain text.",
        ),
        (
            "</prosody",
            "CosyVoice does not support `<prosody>`; remove the tag before calling voice.speech.",
        ),
        (
            "<emphasis",
            "CosyVoice does not support `<emphasis>`; express emphasis with punctuation and `<break/>`.",
        ),
        (
            "<voice",
            "CosyVoice does not support W3C `<voice>`; use payload `voiceId` instead.",
        ),
        (
            "<#",
            "CosyVoice does not support MiniMax pause markers like `<#0.6#>`; use `<break time=\"600ms\"/>`.",
        ),
    ] {
        if lower.contains(needle) {
            return Err(reason.to_string());
        }
    }
    for marker in ["(laughs)", "(sighs)", "(breath)"] {
        if lower.contains(marker) {
            return Err(format!(
                "CosyVoice does not support MiniMax tone marker `{marker}`; remove it from input."
            ));
        }
    }
    let allowed = ["speak", "break", "sub", "phoneme", "soundevent", "say-as"];
    for tag in extract_xml_tag_names(input) {
        if !allowed.contains(&tag.as_str()) {
            return Err(format!(
                "CosyVoice SSML tag `<{tag}>` is not supported. Supported tags: speak, break, sub, phoneme, soundEvent, say-as."
            ));
        }
    }
    validate_cosyvoice_speak_tags(input)
}

/// 采样率/音频格式与两个通道各自的官方取值对齐。
fn validate_speech_audio_params(payload: &Value, model: &str) -> Result<(), String> {
    let sample_rates: &[i64] = if is_cosyvoice_model(model) {
        &[8000, 16000, 22050, 24000, 44100, 48000]
    } else {
        &[8000, 16000, 22050, 24000, 32000, 44100, 48000]
    };
    if let Some(raw) = payload_field_alias(payload, &["sample_rate", "sampleRate"]) {
        let value = raw
            .as_i64()
            .or_else(|| raw.as_str().and_then(|text| text.trim().parse::<i64>().ok()))
            .ok_or_else(|| "voice.speech sample_rate must be an integer".to_string())?;
        if !sample_rates.contains(&value) {
            return Err(format!(
                "voice.speech sample_rate {value} is not supported by model {model}; use one of {}",
                sample_rates
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let formats: &[&str] = if is_cosyvoice_model(model) {
        &["mp3", "wav", "pcm", "opus", "ogg_opus"]
    } else {
        &["mp3", "wav", "pcm", "ogg_opus", "opus"]
    };
    if let Some(raw) = payload_field_alias(payload, &["format"]) {
        if let Some(value) = raw.as_str().map(|text| text.trim().to_ascii_lowercase()) {
            if !value.is_empty() && !formats.contains(&value.as_str()) {
                return Err(format!(
                    "voice.speech format `{value}` is not supported by model {model}; use one of {}",
                    formats.join(", ")
                ));
            }
        }
    }
    Ok(())
}

fn clone_model_target_tts_model(clone_model: &str, fallback_tts_model: &str) -> String {
    let clone_key = normalized_model_key(clone_model);
    if clone_key == COSYVOICE_CLONE_MODEL || clone_key.contains("cosyvoice") {
        return COSYVOICE_TTS_MODEL.to_string();
    }
    if clone_key.ends_with("-voice-clone") {
        let target = clone_key.trim_end_matches("-voice-clone").to_string();
        return if target == "doubao" {
            DOUBAO_TTS_MODEL.to_string()
        } else {
            target
        };
    }
    fallback_tts_model.trim().to_string()
}

fn clone_target_tts_model_from_payload(
    payload: &Value,
    clone_model: &str,
    fallback_tts_model: &str,
) -> String {
    payload_string_alias(
        payload,
        &[
            "targetTtsModel",
            "target_tts_model",
            "ttsModel",
            "tts_model",
        ],
    )
    .unwrap_or_else(|| {
        let fallback = if normalized_model_key(fallback_tts_model)
            == normalized_model_key(clone_model)
            && normalized_model_key(clone_model).contains("clone")
        {
            ""
        } else {
            fallback_tts_model
        };
        clone_model_target_tts_model(clone_model, fallback)
    })
}

fn voice_target_tts_model(value: &Value) -> Option<String> {
    payload_string_alias(
        value,
        &[
            "targetTtsModel",
            "target_tts_model",
            "ttsModel",
            "tts_model",
            "model",
        ],
    )
}

fn voice_mapping_matches_model(value: &Value, tts_model: &str) -> bool {
    let selected = normalized_model_key(tts_model);
    if selected.is_empty() {
        return true;
    }
    voice_target_tts_model(value)
        .map(|model| normalized_model_key(&model) == selected)
        .unwrap_or(false)
}

fn payload_bool_alias(payload: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| payload_field(payload, key).and_then(Value::as_bool))
}

fn payload_f64_alias(payload: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| payload_field(payload, key).and_then(Value::as_f64))
}

fn payload_i64_alias(payload: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| payload_field(payload, key).and_then(Value::as_i64))
}

fn payload_field_alias<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| payload_field(payload, key))
}

fn clean_base_url(value: String) -> String {
    value.trim().trim_end_matches('/').to_string()
}

fn resolve_voice_config(
    state: &State<'_, AppState>,
    payload: Option<&Value>,
) -> Result<VoiceGatewayConfig, String> {
    let settings = with_store(state, |store| Ok(settings_store::settings_snapshot(&store)))?;
    let tts_route = crate::ai_model_manager::AiModelManager::resolve(
        &settings,
        crate::ai_model_manager::AiModelScope::VoiceTts,
        payload,
    );
    let clone_route = crate::ai_model_manager::AiModelManager::resolve(
        &settings,
        crate::ai_model_manager::AiModelScope::VoiceClone,
        payload,
    );
    let base_url = payload
        .and_then(|value| payload_string_alias(value, &["baseUrl", "base_url", "endpoint"]))
        .or_else(|| {
            tts_route
                .as_ref()
                .map(|route| route.base_url.clone())
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            clone_route
                .as_ref()
                .map(|route| route.base_url.clone())
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| payload_string(&settings, "voice_endpoint"))
        .or_else(|| payload_string(&settings, "tts_endpoint"))
        .or_else(|| payload_string(&settings, "api_endpoint"))
        .unwrap_or_else(|| official_base_url_from_settings(&settings));
    let api_key = payload
        .and_then(|value| payload_string_alias(value, &["apiKey", "api_key"]))
        .or_else(|| tts_route.as_ref().and_then(|route| route.api_key.clone()))
        .or_else(|| clone_route.as_ref().and_then(|route| route.api_key.clone()))
        .or_else(|| payload_string(&settings, "voice_api_key"))
        .or_else(|| payload_string(&settings, "tts_api_key"))
        .or_else(|| payload_string(&settings, "api_key"))
        .or_else(|| official_ai_api_key_from_settings(&settings))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let clone_model = payload
        .and_then(|value| payload_string_alias(value, &["cloneModel", "clone_model"]))
        .or_else(|| {
            clone_route
                .as_ref()
                .map(|route| route.model_name.clone())
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| payload_string(&settings, "voice_clone_model"))
        .unwrap_or_default();
    let tts_model = payload
        .and_then(|value| {
            payload_string_alias(
                value,
                &["ttsModel", "tts_model", "voiceTtsModel", "voice_tts_model"],
            )
        })
        .or_else(|| {
            tts_route
                .as_ref()
                .map(|route| route.model_name.clone())
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| payload_string(&settings, "voice_tts_model"))
        .or_else(|| payload_string(&settings, "tts_model"))
        .unwrap_or_default();

    let base_url = clean_base_url(base_url);
    if base_url.is_empty() {
        return Err("voice gateway endpoint is not configured".to_string());
    }
    Ok(VoiceGatewayConfig {
        base_url,
        api_key,
        clone_model,
        tts_model,
    })
}

fn gateway_url(config: &VoiceGatewayConfig, path: &str) -> String {
    format!("{}/{}", config.base_url, path.trim_start_matches('/'))
}

fn authorized_request(
    client: &Client,
    method: reqwest::Method,
    url: &str,
    api_key: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    let request = client.request(method, url);
    match api_key.map(str::trim).filter(|value| !value.is_empty()) {
        Some(key) => request.bearer_auth(key),
        None => request,
    }
}

fn extract_voice_id(value: &Value) -> Option<String> {
    payload_string_alias(value, &["voice_id", "voiceId"]).or_else(|| {
        value
            .get("data")
            .and_then(|data| payload_string_alias(data, &["voice_id", "voiceId"]))
    })
}

fn normalize_voice_response(value: Value, fallback_name: Option<String>) -> Result<Value, String> {
    let voice_id = extract_voice_id(&value)
        .ok_or_else(|| "voice clone response did not include voice_id".to_string())?;
    Ok(json!({
        "voiceId": voice_id,
        "voice_id": voice_id,
        "name": payload_string_alias(&value, &["name", "voiceName"])
            .or(fallback_name)
            .unwrap_or_default(),
        "language": payload_string(&value, "language"),
        "status": payload_string(&value, "status").unwrap_or_else(|| "ready".to_string()),
        "createdAt": payload_field(&value, "created_at")
            .or_else(|| payload_field(&value, "createdAt"))
            .cloned()
            .unwrap_or_else(|| json!(now_iso())),
    }))
}

fn enrich_cloned_voice_metadata(
    voice: &mut Value,
    clone_model: &str,
    target_tts_model: &str,
    payload: &Value,
) {
    let Some(object) = voice.as_object_mut() else {
        return;
    };
    if !clone_model.trim().is_empty() {
        object.insert("cloneModel".to_string(), json!(clone_model.trim()));
    }
    if !target_tts_model.trim().is_empty() {
        object.insert("targetTtsModel".to_string(), json!(target_tts_model.trim()));
        object.insert(
            "target_tts_model".to_string(),
            json!(target_tts_model.trim()),
        );
        object.insert("ttsModel".to_string(), json!(target_tts_model.trim()));
    }
    if let Some(provider) =
        payload_string(payload, "provider").filter(|value| !value.trim().is_empty())
    {
        object.insert("provider".to_string(), json!(provider));
    } else if is_cosyvoice_model(clone_model) || is_cosyvoice_model(target_tts_model) {
        object.insert("provider".to_string(), json!("cosyvoice"));
    }
}

fn voice_list_item_id(value: &Value) -> Option<String> {
    payload_string_alias(value, &["voice_id", "voiceId", "id", "value"]).or_else(|| {
        value
            .get("data")
            .and_then(|data| payload_string_alias(data, &["voice_id", "voiceId", "id", "value"]))
    })
}

fn voice_list_item_name(value: &Value) -> Option<String> {
    payload_string_alias(value, &["name", "title", "voiceName"]).or_else(|| {
        value
            .get("data")
            .and_then(|data| payload_string_alias(data, &["name", "title", "voiceName"]))
    })
}

fn voice_list_item_is_usable(value: &Value) -> bool {
    let status = payload_string(value, "status")
        .or_else(|| {
            value
                .get("data")
                .and_then(|data| payload_string(data, "status"))
        })
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    !matches!(
        status.as_str(),
        "failed" | "error" | "dead_lettered" | "deleted" | "cancelled" | "canceled"
    )
}

fn voice_matches_selected_tts_model(value: &Value, selected_tts_model: &str) -> bool {
    let model = selected_tts_model.trim();
    if model.is_empty() {
        return true;
    }
    if voice_target_tts_model(value).is_some() {
        return voice_mapping_matches_model(value, model);
    }
    for key in [
        "supportedModels",
        "supported_models",
        "ttsModels",
        "tts_models",
    ] {
        if let Some(items) = value.get(key).and_then(Value::as_array) {
            return items.iter().any(|item| {
                item.as_str()
                    .map(|candidate| normalized_model_key(candidate) == normalized_model_key(model))
                    .unwrap_or(false)
            });
        }
    }
    false
}

fn delete_platform_voice(config: &VoiceGatewayConfig, voice_id: &str) -> Result<(), String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let response = authorized_request(
        &client,
        reqwest::Method::DELETE,
        &gateway_url(config, &format!("/audio/voices/{voice_id}")),
        config.api_key.as_deref(),
    )
    .send()
    .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(format!("voice delete failed with HTTP {status}: {body}"));
    }
    Ok(())
}

fn voice_list_items_from_value(value: &Value) -> Vec<Value> {
    if let Some(items) = value.as_array() {
        return items.clone();
    }
    if voice_list_item_id(value).is_some() {
        return vec![value.clone()];
    }
    for key in ["voices", "items", "data", "results"] {
        if let Some(nested) = value.get(key) {
            let items = voice_list_items_from_value(nested);
            if !items.is_empty() {
                return items;
            }
        }
    }
    Vec::new()
}

/// 适配层双通道内置音色（豆包），替代官方 327 条注入。
/// 注意：不携带 systemVoice/source:"system" 标记，provider 标记为 adapter。
fn adapter_system_voice_list_items() -> Vec<Value> {
    let Ok(catalog) = serde_json::from_str::<Value>(ADAPTER_SYSTEM_VOICES_JSON) else {
        return Vec::new();
    };
    let Some(items) = catalog.get("voices").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let voice_id = payload_string(item, "voice_id")?;
            let voice_name = payload_string(item, "name").unwrap_or_else(|| voice_id.clone());
            let language_zh = payload_string(item, "language_zh").unwrap_or_default();
            let gender_hint = payload_string(item, "gender_hint").unwrap_or_default();
            let supported_models = item
                .get("supportedModels")
                .cloned()
                .unwrap_or_else(|| json!([]));
            Some(json!({
                "id": voice_id,
                "value": voice_id,
                "voiceId": voice_id,
                "voice_id": voice_id,
                "name": voice_name,
                "title": voice_name,
                "status": "ready",
                "source": "adapter",
                "provider": "adapter",
                "supportedModels": supported_models,
                "languageZh": language_zh,
                "language_zh": language_zh,
                "genderHint": gender_hint,
                "gender_hint": gender_hint,
            }))
        })
        .collect()
}

/// 只有当前所选 TTS 模型命中的适配层音色才会注入，避免出现“暂无音色”。
fn append_adapter_system_voices(
    voices: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    selected_tts_model: &str,
) {
    for item in adapter_system_voice_list_items() {
        if !voice_matches_selected_tts_model(&item, selected_tts_model) {
            continue;
        }
        if let Some(id) = voice_list_item_id(&item) {
            if seen.insert(id) {
                voices.push(item);
            }
        }
    }
}

/// 用户在设置页手动添加的百炼复刻音色 ID（settings.bailian_custom_voices_json，
/// 数组元素形如 {"id":"cosyvoice-v3.5-flash-bailian-<hex>","name":"备注"}）。
fn append_bailian_custom_voices(
    voices: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    settings: &Value,
) {
    let parsed = settings
        .get("bailian_custom_voices_json")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .unwrap_or(Value::Null);
    let Some(items) = parsed.as_array() else {
        return;
    };
    for item in items {
        let Some(voice_id) = item
            .get("id")
            .or_else(|| item.get("voiceId"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if !seen.insert(voice_id.to_string()) {
            continue;
        }
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                let tail: String = voice_id
                    .chars()
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("自定义复刻音色 · {tail}")
            });
        voices.push(json!({
            "id": voice_id,
            "value": voice_id,
            "voiceId": voice_id,
            "voice_id": voice_id,
            "name": name,
            "title": name,
            "status": "ready",
            "provider": "custom",
            "source": "custom",
            "language": "中文 (普通话)",
            "languageZh": "中文 (普通话)",
            "language_zh": "中文 (普通话)",
            "supportedModels": ["cosyvoice-v3.5-flash"],
        }));
    }
}

fn subject_voice_list_items(
    state: &State<'_, AppState>,
    selected_tts_model: Option<&str>,
) -> Result<Vec<Value>, String> {
    ensure_store_hydrated_for_subjects(state)?;
    let subjects = with_store(state, |store| Ok(subject_store::list_subjects(&store)))?;
    let selected = selected_tts_model
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut items = Vec::new();
    for subject in &subjects {
        let Some(voice) = subject.voice.as_ref() else {
            continue;
        };
        if let Some(mappings) = voice.get("voiceMappings").and_then(Value::as_object) {
            for mapping in mappings.values() {
                if selected
                    .map(|model| !voice_mapping_matches_model(mapping, model))
                    .unwrap_or(false)
                {
                    continue;
                }
                if let Some(item) = subject_voice_list_item(subject, mapping) {
                    items.push(item);
                }
            }
        }

        let include_legacy = selected.map(|model| voice_mapping_matches_model(voice, model)).unwrap_or(true);
        if include_legacy {
            if let Some(item) = subject_voice_list_item(subject, voice) {
                if items
                    .iter()
                    .all(|existing| voice_list_item_id(existing) != voice_list_item_id(&item))
                {
                    items.push(item);
                }
            }
        }
    }
    Ok(items)
}

fn subject_voice_list_item(subject: &SubjectRecord, voice: &Value) -> Option<Value> {
    let voice_id = payload_string_alias(voice, &["voiceId", "voice_id"])?;
    let status = payload_string(voice, "status").unwrap_or_else(|| "ready".to_string());
    if !voice_list_item_is_usable(voice) {
        return None;
    }
    Some(json!({
        "id": voice_id,
        "value": voice_id,
        "voiceId": voice_id,
        "voice_id": voice_id,
        "name": subject.name,
        "title": subject.name,
        "status": status,
        "source": "subject",
        "ownerAssetId": subject.id,
        "assetId": subject.id,
        "subjectId": subject.id,
        "sampleFilePath": subject.voice_path,
        "language": payload_string(voice, "language"),
        "targetTtsModel": voice_target_tts_model(voice),
        "target_tts_model": voice_target_tts_model(voice),
        "ttsModel": voice_target_tts_model(voice),
        "cloneModel": payload_string(voice, "cloneModel"),
        "provider": payload_string(voice, "provider"),
    }))
}

fn subject_voice_id_for_tts_model_state(
    state: &State<'_, AppState>,
    subject_id: &str,
    tts_model: &str,
) -> Result<Option<String>, String> {
    ensure_store_hydrated_for_subjects(state)?;
    let subject = with_store(state, |store| {
        Ok(subject_store::get_subject(&store, subject_id))
    })?;
    Ok(subject
        .as_ref()
        .and_then(|subject| subject_voice_id_for_tts_model(subject, tts_model)))
}

fn subject_voice_job_matches_state(
    state: &State<'_, AppState>,
    subject_id: &str,
    job_id: &str,
    tts_model: &str,
) -> Result<bool, String> {
    let job_id = job_id.trim();
    if job_id.is_empty() {
        return Ok(true);
    }
    ensure_store_hydrated_for_subjects(state)?;
    let Some(subject) = with_store(state, |store| {
        Ok(subject_store::get_subject(&store, subject_id))
    })?
    else {
        return Ok(false);
    };
    let Some(voice) = subject.voice.as_ref() else {
        return Ok(false);
    };
    if let Some(mappings) = voice.get("voiceMappings").and_then(Value::as_object) {
        for mapping in mappings.values() {
            if !voice_mapping_matches_model(mapping, tts_model) {
                continue;
            }
            let mapping_job_id = payload_string(mapping, "jobId").unwrap_or_default();
            if !mapping_job_id.trim().is_empty() {
                return Ok(mapping_job_id.trim() == job_id);
            }
        }
    }
    let current_job_id = payload_string(voice, "jobId").unwrap_or_default();
    let current_tts_model = voice_target_tts_model(voice).unwrap_or_default();
    if !current_tts_model.trim().is_empty()
        && normalized_model_key(&current_tts_model) != normalized_model_key(tts_model)
    {
        return Ok(true);
    }
    if current_job_id.trim() != job_id {
        return Ok(false);
    }
    Ok(current_tts_model.trim().is_empty()
        || normalized_model_key(&current_tts_model) == normalized_model_key(tts_model))
}

fn subject_voice_id_for_tts_model(record: &SubjectRecord, tts_model: &str) -> Option<String> {
    let voice = record.voice.as_ref()?;
    if let Some(mappings) = voice.get("voiceMappings").and_then(Value::as_object) {
        for mapping in mappings.values() {
            if voice_mapping_matches_model(mapping, tts_model) {
                return payload_string_alias(mapping, &["voiceId", "voice_id"]);
            }
        }
    }
    if voice_mapping_matches_model(voice, tts_model) {
        return payload_string_alias(voice, &["voiceId", "voice_id"]);
    }
    None
}

fn tts_model_for_voice_id_from_subjects(
    subjects: &[SubjectRecord],
    voice_id: &str,
) -> Option<String> {
    let target_voice_id = voice_id.trim();
    if target_voice_id.is_empty() {
        return None;
    }
    for subject in subjects {
        let Some(voice) = subject.voice.as_ref() else {
            continue;
        };
        if let Some(mappings) = voice.get("voiceMappings").and_then(Value::as_object) {
            for mapping in mappings.values() {
                let mapping_matches_voice = payload_string_alias(mapping, &["voiceId", "voice_id"])
                    .map(|id| id == target_voice_id)
                    .unwrap_or(false);
                if mapping_matches_voice {
                    if let Some(model) =
                        voice_target_tts_model(mapping).filter(|value| !value.trim().is_empty())
                    {
                        return Some(model);
                    }
                }
            }
        }
        let legacy_matches = payload_string_alias(voice, &["voiceId", "voice_id"])
            .map(|id| id == target_voice_id)
            .unwrap_or(false);
        if legacy_matches {
            if let Some(model) =
                voice_target_tts_model(voice).filter(|value| !value.trim().is_empty())
            {
                return Some(model);
            }
        }
    }
    None
}

fn tts_model_for_voice_id_state(
    state: &State<'_, AppState>,
    voice_id: &str,
) -> Result<Option<String>, String> {
    ensure_store_hydrated_for_subjects(state)?;
    let subjects = with_store(state, |store| Ok(subject_store::list_subjects(&store)))?;
    Ok(tts_model_for_voice_id_from_subjects(&subjects, voice_id))
}

fn delete_stale_cloned_voice(
    config: &VoiceGatewayConfig,
    subject_id: &str,
    target_tts_model: &str,
    job_id: Option<&str>,
    voice: &Value,
) {
    let Some(voice_id) = payload_string_alias(voice, &["voiceId", "voice_id"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    match delete_platform_voice(config, &voice_id) {
        Ok(()) => log_voice_clone_event(
            LogLevel::Info,
            "cleanup_stale_voice",
            format!("deleted stale voice clone result for subject={subject_id} target_tts_model={target_tts_model}"),
            json!({
                "ownerAssetId": subject_id,
                "targetTtsModel": target_tts_model,
                "jobId": job_id,
                "deletedVoiceId": voice_id,
            }),
        ),
        Err(error) => log_voice_clone_event(
            LogLevel::Warn,
            "cleanup_stale_voice_failed",
            format!("failed to delete stale voice clone result for subject={subject_id}: {error}"),
            json!({
                "ownerAssetId": subject_id,
                "targetTtsModel": target_tts_model,
                "jobId": job_id,
                "deletedVoiceId": voice_id,
                "error": error,
            }),
        ),
    }
}

fn cleanup_replaced_subject_voice(
    config: &VoiceGatewayConfig,
    subject_id: &str,
    target_tts_model: &str,
    previous_voice_id: Option<String>,
    next_voice: &Value,
) {
    let Some(previous_voice_id) = previous_voice_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let next_voice_id = payload_string_alias(next_voice, &["voiceId", "voice_id"])
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    if previous_voice_id == next_voice_id {
        return;
    }
    match delete_platform_voice(config, &previous_voice_id) {
        Ok(()) => log_voice_clone_event(
            LogLevel::Info,
            "cleanup_replaced_voice",
            format!("deleted replaced voice id for subject={subject_id} target_tts_model={target_tts_model}"),
            json!({
                "ownerAssetId": subject_id,
                "targetTtsModel": target_tts_model,
                "deletedVoiceId": previous_voice_id,
                "nextVoiceId": next_voice_id,
            }),
        ),
        Err(error) => log_voice_clone_event(
            LogLevel::Warn,
            "cleanup_replaced_voice_failed",
            format!("failed to delete replaced voice id for subject={subject_id}: {error}"),
            json!({
                "ownerAssetId": subject_id,
                "targetTtsModel": target_tts_model,
                "deletedVoiceId": previous_voice_id,
                "nextVoiceId": next_voice_id,
                "error": error,
            }),
        ),
    }
}

fn resolve_sample_path(
    state: &State<'_, AppState>,
    payload: &Value,
) -> Result<(PathBuf, Option<String>), String> {
    let path = payload_string_alias(
        payload,
        &[
            "samplePath",
            "sampleFilePath",
            "filePath",
            "path",
            "voicePath",
        ],
    )
    .ok_or_else(|| "voice.clone requires samplePath".to_string())?;
    let owner_asset_id = payload_string_alias(payload, &["ownerAssetId", "assetId", "subjectId"]);
    let candidate = PathBuf::from(path.trim());
    if candidate.is_absolute() {
        return Ok((normalize_legacy_workspace_path(&candidate), owner_asset_id));
    }
    if let Some(asset_id) = owner_asset_id.as_deref().filter(|value| !value.is_empty()) {
        let resolved = subjects_root(state)?.join(asset_id).join(&candidate);
        return Ok((normalize_legacy_workspace_path(&resolved), owner_asset_id));
    }
    let resolved = workspace_root(state)?.join(candidate);
    Ok((normalize_legacy_workspace_path(&resolved), owner_asset_id))
}

fn voice_clone_sample_extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_direct_voice_clone_sample(path: &Path) -> bool {
    matches!(
        voice_clone_sample_extension(path).as_str(),
        "mp3" | "wav" | "m4a"
    )
}

fn is_transcodable_voice_clone_sample(path: &Path) -> bool {
    matches!(
        voice_clone_sample_extension(path).as_str(),
        "aac" | "flac" | "ogg" | "opus" | "webm" | "mp4" | "m4v" | "mov" | "mkv" | "avi"
    )
}

fn transcode_voice_clone_sample_to_wav(
    app: Option<&AppHandle>,
    path: &Path,
) -> Result<PathBuf, String> {
    let output_path = std::env::temp_dir().join(format!("{}-voice-clone.wav", make_id("redbox")));
    let output = crate::background_command(ffmpeg_executable(app)?)
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-vn")
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("24000")
        .arg("-f")
        .arg("wav")
        .arg(&output_path)
        .output()
        .map_err(|error| {
            format!("声音复刻样本需要抽取为 wav，但无法启动 ffmpeg：{error}。请改用带音轨的视频或 mp3、wav、m4a 文件。")
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let _ = fs::remove_file(&output_path);
        return Err(if detail.is_empty() {
            "声音复刻样本抽取失败，请确认视频包含音轨，或改用 mp3、wav、m4a 文件。".to_string()
        } else {
            format!("声音复刻样本抽取失败：{detail}")
        });
    }
    Ok(output_path)
}

fn prepare_voice_clone_sample_upload(
    app: Option<&AppHandle>,
    path: &Path,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    if is_direct_voice_clone_sample(path) {
        return Ok((path.to_path_buf(), None));
    }
    if is_transcodable_voice_clone_sample(path) {
        let converted = transcode_voice_clone_sample_to_wav(app, path)?;
        return Ok((converted.clone(), Some(converted)));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown");
    Err(format!(
        "声音复刻样本格式不支持：{}。请使用带音轨的视频或 mp3、wav、m4a 文件。",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(extension)
    ))
}

pub(crate) fn clone_voice(
    app: Option<&AppHandle>,
    state: &State<'_, AppState>,
    payload: &Value,
) -> Result<Value, String> {
    let config = resolve_voice_config(state, Some(payload))?;
    let Some(api_key) = config.api_key.as_deref() else {
        return Err("voice clone requires an API key".to_string());
    };
    if config.clone_model.trim().is_empty() {
        return Err(
            "voice clone model is not configured; choose a voice clone model in Settings first"
                .to_string(),
        );
    }
    if config.tts_model.trim().is_empty() {
        return Err(
            "voice clone target TTS model is not configured; choose a TTS model in Settings first"
                .to_string(),
        );
    }
    let owner_asset_id = payload_string_alias(payload, &["ownerAssetId", "assetId", "subjectId"]);
    if let Some(sample_file_key) =
        payload_string_alias(payload, &["sampleFileKey", "sample_file_key"])
            .filter(|value| !value.trim().is_empty())
    {
        return clone_voice_from_managed_key(
            state,
            payload,
            &config,
            api_key,
            owner_asset_id,
            sample_file_key,
        );
    }
    let (sample_path, owner_asset_id) = resolve_sample_path(state, payload)?;
    let (upload_path, temporary_upload_path) =
        prepare_voice_clone_sample_upload(app, &sample_path)?;
    let expected_bytes = fs::metadata(&upload_path)
        .map_err(|error| {
            format!(
                "failed to read voice sample metadata {}: {error}",
                upload_path.display()
            )
        })?
        .len();
    let bytes = fs::read(&upload_path).map_err(|error| {
        format!(
            "failed to read voice sample {}: {error}",
            upload_path.display()
        )
    })?;
    if bytes.len() as u64 != expected_bytes {
        return Err(format!(
            "音频采样文件读取不完整：{expected_bytes} 字节预期，实际 {} 字节",
            bytes.len()
        ));
    }
    let file_name = upload_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("voice-sample.wav")
        .to_string();
    let (mime_type, _, _) = guess_mime_and_kind(&upload_path);
    if let Some(temp_path) = temporary_upload_path.as_deref() {
        let _ = fs::remove_file(temp_path);
    }
    let part = match multipart::Part::bytes(bytes.clone())
        .file_name(file_name.clone())
        .mime_str(&mime_type)
    {
        Ok(typed_part) => typed_part,
        Err(_) => multipart::Part::bytes(bytes).file_name(file_name),
    };
    let name = payload_string(payload, "name").or_else(|| {
        sample_path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(ToString::to_string)
    });
    let mut form = multipart::Form::new().part("file", part);
    if let Some(value) = name.as_deref().filter(|value| !value.trim().is_empty()) {
        form = form.text("name", value.to_string());
    }
    if let Some(value) =
        payload_string(payload, "language").filter(|value| !value.trim().is_empty())
    {
        form = form.text("language", value);
    }
    let model = payload_string(payload, "model").unwrap_or_else(|| config.clone_model.clone());
    let target_tts_model = clone_target_tts_model_from_payload(payload, &model, &config.tts_model);
    let job_id = payload_string(payload, "jobId");
    log_voice_clone_event(
        LogLevel::Info,
        "request",
        format!("voice clone request model={model} target_tts_model={target_tts_model}"),
        json!({
            "jobId": job_id,
            "ownerAssetId": owner_asset_id,
            "model": model,
            "targetTtsModel": target_tts_model,
            "configuredCloneModel": config.clone_model,
            "configuredTtsModel": config.tts_model,
            "baseUrl": config.base_url,
            "samplePath": sample_path.display().to_string(),
            "uploadPath": upload_path.display().to_string(),
            "mimeType": mime_type,
            "expectedBytes": expected_bytes,
        }),
    );
    if !model.trim().is_empty() {
        form = form.text("model", model.clone());
    }
    if !target_tts_model.trim().is_empty() {
        form = form.text("target_tts_model", target_tts_model.clone());
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|error| error.to_string())?;
    let url = gateway_url(&config, "/audio/voices/clone");
    let response = authorized_request(&client, reqwest::Method::POST, &url, Some(api_key))
        .multipart(form)
        .send()
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().map_err(|error| error.to_string())?;
    if !status.is_success() {
        log_voice_clone_event(
            LogLevel::Error,
            "upstream_error",
            format!(
                "voice clone upstream failed status={status} model={model} target_tts_model={target_tts_model}"
            ),
            json!({
                "jobId": job_id,
                "ownerAssetId": owner_asset_id,
                "model": model,
                "targetTtsModel": target_tts_model,
                "httpStatus": status.as_u16(),
                "baseUrl": config.base_url,
                "upstreamBody": body,
            }),
        );
        return Err(format!("voice clone failed with HTTP {status}: {body}"));
    }
    let raw = serde_json::from_str::<Value>(&body).map_err(|error| error.to_string())?;
    let mut voice = normalize_voice_response(raw.clone(), name)?;
    enrich_cloned_voice_metadata(&mut voice, &model, &target_tts_model, payload);
    log_voice_clone_event(
        LogLevel::Info,
        "success",
        format!("voice clone completed model={model} target_tts_model={target_tts_model}"),
        json!({
            "jobId": job_id,
            "ownerAssetId": owner_asset_id,
            "model": model,
            "targetTtsModel": target_tts_model,
            "voiceId": payload_string_alias(&voice, &["voiceId", "voice_id"]),
            "baseUrl": config.base_url,
        }),
    );
    if owner_asset_id.is_some()
        && payload_bool_alias(payload, &["writeBack", "write_back"]).unwrap_or(true)
    {
        if let Some(subject_id) = owner_asset_id.as_deref() {
            if let Some(job_id) = job_id.as_deref() {
                if !subject_voice_job_matches_state(state, subject_id, job_id, &target_tts_model)? {
                    delete_stale_cloned_voice(
                        &config,
                        subject_id,
                        &target_tts_model,
                        Some(job_id),
                        &voice,
                    );
                    return Ok(json!({
                        "success": true,
                        "stale": true,
                        "voice": voice,
                        "ownerAssetId": owner_asset_id,
                        "samplePath": sample_path.display().to_string(),
                        "raw": raw,
                    }));
                }
            }
            let previous_voice_id =
                subject_voice_id_for_tts_model_state(state, subject_id, &target_tts_model)?;
            patch_subject_voice_state(state, subject_id, voice.clone())?;
            cleanup_replaced_subject_voice(
                &config,
                subject_id,
                &target_tts_model,
                previous_voice_id,
                &voice,
            );
        }
    }
    Ok(json!({
        "success": true,
        "voice": voice,
        "ownerAssetId": owner_asset_id,
        "samplePath": sample_path.display().to_string(),
        "raw": raw,
    }))
}

fn clone_voice_from_managed_key(
    state: &State<'_, AppState>,
    payload: &Value,
    config: &VoiceGatewayConfig,
    api_key: &str,
    owner_asset_id: Option<String>,
    sample_file_key: String,
) -> Result<Value, String> {
    let model = payload_string(payload, "model").unwrap_or_else(|| config.clone_model.clone());
    let target_tts_model = clone_target_tts_model_from_payload(payload, &model, &config.tts_model);
    let name = payload_string(payload, "name");
    let job_id = payload_string(payload, "jobId");
    log_voice_clone_event(
        LogLevel::Info,
        "request",
        format!("voice clone request model={model} target_tts_model={target_tts_model}"),
        json!({
            "jobId": job_id,
            "ownerAssetId": owner_asset_id,
            "model": model,
            "targetTtsModel": target_tts_model,
            "configuredCloneModel": config.clone_model,
            "configuredTtsModel": config.tts_model,
            "baseUrl": config.base_url,
            "sampleFileKey": sample_file_key,
        }),
    );
    let mut body = Map::new();
    body.insert(
        "sample_file_key".to_string(),
        json!(sample_file_key.clone()),
    );
    if let Some(value) = name.as_deref().filter(|value| !value.trim().is_empty()) {
        body.insert("name".to_string(), json!(value));
    }
    if let Some(value) =
        payload_string(payload, "language").filter(|value| !value.trim().is_empty())
    {
        body.insert("language".to_string(), json!(value));
    }
    if !model.trim().is_empty() {
        body.insert("model".to_string(), json!(model.clone()));
    }
    if !target_tts_model.trim().is_empty() {
        body.insert(
            "target_tts_model".to_string(),
            json!(target_tts_model.clone()),
        );
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|error| error.to_string())?;
    let response = authorized_request(
        &client,
        reqwest::Method::POST,
        &gateway_url(config, "/audio/voices/clone"),
        Some(api_key),
    )
    .json(&Value::Object(body))
    .send()
    .map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().map_err(|error| error.to_string())?;
    if !status.is_success() {
        log_voice_clone_event(
            LogLevel::Error,
            "upstream_error",
            format!(
                "voice clone upstream failed status={status} model={model} target_tts_model={target_tts_model}"
            ),
            json!({
                "jobId": job_id,
                "ownerAssetId": owner_asset_id,
                "model": model,
                "targetTtsModel": target_tts_model,
                "httpStatus": status.as_u16(),
                "baseUrl": config.base_url,
                "sampleFileKey": sample_file_key,
                "upstreamBody": body,
            }),
        );
        return Err(format!("voice clone failed with HTTP {status}: {body}"));
    }
    let raw = serde_json::from_str::<Value>(&body).map_err(|error| error.to_string())?;
    let mut voice = normalize_voice_response(raw.clone(), name)?;
    enrich_cloned_voice_metadata(&mut voice, &model, &target_tts_model, payload);
    if let Some(object) = voice.as_object_mut() {
        object.insert("sampleFileKey".to_string(), json!(sample_file_key.clone()));
    }
    if owner_asset_id.is_some()
        && payload_bool_alias(payload, &["writeBack", "write_back"]).unwrap_or(true)
    {
        if let Some(subject_id) = owner_asset_id.as_deref() {
            if let Some(job_id) = job_id.as_deref() {
                if !subject_voice_job_matches_state(state, subject_id, job_id, &target_tts_model)? {
                    delete_stale_cloned_voice(
                        config,
                        subject_id,
                        &target_tts_model,
                        Some(job_id),
                        &voice,
                    );
                    return Ok(json!({
                        "success": true,
                        "stale": true,
                        "voice": voice,
                        "ownerAssetId": owner_asset_id,
                        "sampleFileKey": sample_file_key,
                        "raw": raw,
                    }));
                }
            }
            let previous_voice_id =
                subject_voice_id_for_tts_model_state(state, subject_id, &target_tts_model)?;
            patch_subject_voice_state(state, subject_id, voice.clone())?;
            cleanup_replaced_subject_voice(
                config,
                subject_id,
                &target_tts_model,
                previous_voice_id,
                &voice,
            );
        }
    }
    log_voice_clone_event(
        LogLevel::Info,
        "success",
        format!("voice clone completed model={model} target_tts_model={target_tts_model}"),
        json!({
            "jobId": job_id,
            "ownerAssetId": owner_asset_id,
            "model": model,
            "targetTtsModel": target_tts_model,
            "voiceId": payload_string_alias(&voice, &["voiceId", "voice_id"]),
            "baseUrl": config.base_url,
            "sampleFileKey": sample_file_key,
        }),
    );
    Ok(json!({
        "success": true,
        "voice": voice,
        "ownerAssetId": owner_asset_id,
        "sampleFileKey": sample_file_key,
        "raw": raw,
    }))
}

pub(crate) fn list_voices(state: &State<'_, AppState>, payload: &Value) -> Result<Value, String> {
    let mut voices = Vec::new();
    let mut seen = HashSet::new();
    let mut local_subject_voice_names = HashSet::new();
    let requested_model = payload_string_alias(payload, &["model", "ttsModel", "tts_model"]);
    for item in subject_voice_list_items(state, requested_model.as_deref())? {
        if voice_list_item_is_usable(&item) {
            if let Some(name) = voice_list_item_name(&item) {
                local_subject_voice_names.insert(name.trim().to_ascii_lowercase());
            }
            if let Some(id) = voice_list_item_id(&item) {
                if seen.insert(id) {
                    voices.push(item);
                }
            }
        }
    }

    let push_remote_voice = |voices: &mut Vec<Value>,
                             seen: &mut HashSet<String>,
                             local_subject_voice_names: &HashSet<String>,
                             config: &VoiceGatewayConfig,
                             item: Value| {
        if !voice_list_item_is_usable(&item) {
            if let Some(id) = voice_list_item_id(&item) {
                let _ = delete_platform_voice(config, &id);
            }
            return;
        }
        if let Some(name) = voice_list_item_name(&item) {
            if local_subject_voice_names.contains(&name.trim().to_ascii_lowercase()) {
                return;
            }
        }
        if let Some(id) = voice_list_item_id(&item) {
            if seen.insert(id) {
                voices.push(item);
            }
        }
    };

    let mut selected_tts_model = requested_model.clone().unwrap_or_default();
    let config = match resolve_voice_config(state, Some(payload)) {
        Ok(config) => config,
        Err(error) => {
            append_adapter_system_voices(&mut voices, &mut seen, &selected_tts_model);
            return Ok(json!({ "success": true, "voices": voices, "configError": error }));
        }
    };
    if selected_tts_model.trim().is_empty() {
        selected_tts_model = config.tts_model.clone();
    }
    // 百炼直连就绪时跳过网关远程列表：网关 cosyvoice 通道未注册该模型，
    // 固定音色列表由本地适配层数据（adapter-system-voices.json）注入。
    if is_cosyvoice_model(&selected_tts_model) {
        let settings = with_store(state, |store| Ok(settings_store::settings_snapshot(&store)))?;
        if crate::dashscope_tts::resolve_bailian_config(&settings).is_some() {
            append_adapter_system_voices(&mut voices, &mut seen, &selected_tts_model);
            append_bailian_custom_voices(&mut voices, &mut seen, &settings);
            return Ok(json!({ "success": true, "voices": voices }));
        }
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|error| error.to_string())?;
    let response = match authorized_request(
        &client,
        reqwest::Method::GET,
        &gateway_url(&config, "/audio/voices"),
        config.api_key.as_deref(),
    )
    .query(&[("model", selected_tts_model.as_str())])
    .send()
    {
        Ok(response) => response,
        Err(error) => {
            append_adapter_system_voices(&mut voices, &mut seen, &selected_tts_model);
            return Ok(json!({
                "success": true,
                "voices": voices,
                "remoteError": error.to_string(),
            }));
        }
    };
    let status = response.status();
    let body = response.text().map_err(|error| error.to_string())?;
    if !status.is_success() {
        append_adapter_system_voices(&mut voices, &mut seen, &selected_tts_model);
        return Ok(json!({
            "success": true,
            "voices": voices,
            "remoteError": format!("voice list failed with HTTP {status}: {body}"),
        }));
    }
    let parsed = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({ "raw": body }));
    for item in voice_list_items_from_value(&parsed) {
        if !voice_matches_selected_tts_model(&item, &selected_tts_model) {
            continue;
        }
        push_remote_voice(
            &mut voices,
            &mut seen,
            &local_subject_voice_names,
            &config,
            item,
        );
    }
    append_adapter_system_voices(&mut voices, &mut seen, &selected_tts_model);
    Ok(json!({ "success": true, "voices": voices, "raw": parsed }))
}

pub(crate) fn get_voice(state: &State<'_, AppState>, payload: &Value) -> Result<Value, String> {
    let voice_id = payload_string_alias(payload, &["voiceId", "voice_id", "id"])
        .ok_or_else(|| "voice.get requires voiceId".to_string())?;
    let config = resolve_voice_config(state, Some(payload))?;
    let client = Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|error| error.to_string())?;
    let response = authorized_request(
        &client,
        reqwest::Method::GET,
        &gateway_url(&config, &format!("/audio/voices/{voice_id}")),
        config.api_key.as_deref(),
    )
    .send()
    .map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("voice get failed with HTTP {status}: {body}"));
    }
    let voice = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({ "raw": body }));
    Ok(json!({ "success": true, "voice": voice }))
}

pub(crate) fn delete_voice(state: &State<'_, AppState>, payload: &Value) -> Result<Value, String> {
    let voice_id = payload_string_alias(payload, &["voiceId", "voice_id", "id"])
        .ok_or_else(|| "voice.delete requires voiceId".to_string())?;
    let config = resolve_voice_config(state, Some(payload))?;
    let client = Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|error| error.to_string())?;
    let response = authorized_request(
        &client,
        reqwest::Method::DELETE,
        &gateway_url(&config, &format!("/audio/voices/{voice_id}")),
        config.api_key.as_deref(),
    )
    .send()
    .map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!("voice delete failed with HTTP {status}: {body}"));
    }
    Ok(json!({ "success": true, "voiceId": voice_id }))
}

fn content_type_to_extension(content_type: Option<&str>, fallback: &str) -> String {
    let normalized = content_type.unwrap_or("").to_ascii_lowercase();
    if normalized.contains("wav") {
        "wav".to_string()
    } else if normalized.contains("mpeg") || normalized.contains("mp3") {
        "mp3".to_string()
    } else if normalized.contains("ogg") {
        "ogg".to_string()
    } else if normalized.contains("webm") {
        "webm".to_string()
    } else {
        fallback.to_string()
    }
}

fn decode_audio_response(
    content_type: Option<&str>,
    bytes: Vec<u8>,
    fallback_ext: &str,
) -> Result<(Vec<u8>, String), String> {
    let is_json = content_type
        .map(|value| value.to_ascii_lowercase().contains("json"))
        .unwrap_or(false);
    if !is_json {
        return Ok((bytes, content_type_to_extension(content_type, fallback_ext)));
    }
    let value = serde_json::from_slice::<Value>(&bytes).map_err(|error| error.to_string())?;
    let audio_base64 = payload_string_alias(&value, &["b64_json", "audio", "audio_base64"])
        .or_else(|| {
            value
                .get("data")
                .and_then(|data| payload_string_alias(data, &["b64_json", "audio", "audio_base64"]))
        })
        .ok_or_else(|| "speech response JSON did not include audio bytes".to_string())?;
    let audio = base64::engine::general_purpose::STANDARD
        .decode(audio_base64.trim())
        .map_err(|error| error.to_string())?;
    Ok((audio, fallback_ext.to_string()))
}

fn nested_payload_field<'a>(payload: &'a Value, object_key: &str, key: &str) -> Option<&'a Value> {
    payload
        .get(object_key)
        .and_then(Value::as_object)
        .and_then(|object| object.get(key))
}

fn nested_payload_f64_alias(payload: &Value, object_key: &str, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| nested_payload_field(payload, object_key, key).and_then(Value::as_f64))
}

fn nested_payload_i64_alias(payload: &Value, object_key: &str, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| nested_payload_field(payload, object_key, key).and_then(Value::as_i64))
}

fn nested_payload_string_alias(payload: &Value, object_key: &str, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        nested_payload_field(payload, object_key, key)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn speech_speed(payload: &Value) -> Result<Option<f64>, String> {
    let speed = payload_f64_alias(payload, &["speed", "speed_rate"])
        .or_else(|| nested_payload_f64_alias(payload, "voice_setting", &["speed", "speed_rate"]));
    if let Some(value) = speed {
        if !(0.5..=2.0).contains(&value) {
            return Err("voice.speech speed must be between 0.5 and 2.0".to_string());
        }
    }
    Ok(speed)
}

fn speech_pitch(payload: &Value) -> Result<Option<i64>, String> {
    let pitch = payload_i64_alias(payload, &["pitch"])
        .or_else(|| nested_payload_i64_alias(payload, "voice_setting", &["pitch"]));
    if let Some(value) = pitch {
        if !(-12..=12).contains(&value) {
            return Err("voice.speech pitch must be between -12 and 12".to_string());
        }
    }
    Ok(pitch)
}

/// 火山豆包 TTS 2.0 用整数半音（-12~12）；CosyVoice 3.5 Flash 官方用 0.5~2.0 的倍数。
fn speech_pitch_for_model(payload: &Value, model: &str) -> Result<Option<Value>, String> {
    if is_cosyvoice_model(model) {
        let pitch = payload_f64_alias(payload, &["pitch"])
            .or_else(|| nested_payload_f64_alias(payload, "voice_setting", &["pitch"]));
        if let Some(value) = pitch {
            if !(0.5..=2.0).contains(&value) {
                return Err(
                    "CosyVoice 语调 pitch 必须是 0.5 到 2.0 之间的倍数（官方规范）".to_string(),
                );
            }
            return Ok(Some(json!(value)));
        }
        return Ok(None);
    }
    Ok(speech_pitch(payload)?.map(|value| json!(value)))
}

fn speech_emotion(payload: &Value) -> Result<Option<String>, String> {
    let emotion = payload_string(payload, "emotion")
        .or_else(|| nested_payload_string_alias(payload, "voice_setting", &["emotion"]))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value == "whisper" {
                "whipser".to_string()
            } else {
                value
            }
        });
    if let Some(value) = emotion.as_deref() {
        const SUPPORTED: &[&str] = &[
            "happy",
            "sad",
            "angry",
            "fearful",
            "disgusted",
            "surprised",
            "calm",
            "fluent",
            "whipser",
        ];
        if !SUPPORTED.contains(&value) {
            return Err(format!(
                "voice.speech emotion must be one of {}",
                SUPPORTED.join(", ")
            ));
        }
    }
    Ok(emotion)
}

fn build_speech_request_body(
    payload: &Value,
    input: String,
    voice_id: String,
    model: String,
    response_format: String,
) -> Result<Map<String, Value>, String> {
    let mut body = Map::new();
    let supports_emotion = tts_model_supports_emotion(&model);
    validate_cosyvoice_speech_input(&model, &input)?;
    validate_speech_audio_params(payload, &model)?;
    body.insert("model".to_string(), json!(gateway_tts_model(&model)));
    body.insert("input".to_string(), json!(input));
    body.insert("voice_id".to_string(), json!(wire_voice_id(&voice_id)));
    body.insert("response_format".to_string(), json!(response_format));
    body.insert("return_audio_binary".to_string(), json!(true));

    if let Some(prompt) =
        payload_string_alias(payload, &["prompt", "stylePrompt", "style_prompt"])
            .filter(|value| !value.trim().is_empty())
    {
        body.insert("prompt".to_string(), json!(prompt));
    }
    if let Some(language_hints) = payload_field_alias(payload, &["language_hints", "languageHints"])
    {
        body.insert("language_hints".to_string(), language_hints.clone());
    }
    if let Some(speed) = speech_speed(payload)? {
        body.insert("speed".to_string(), json!(speed));
    }
    if let Some(pitch) = speech_pitch_for_model(payload, &model)? {
        body.insert("pitch".to_string(), pitch);
    }
    if supports_emotion {
        if let Some(emotion) = speech_emotion(payload)? {
            body.insert("emotion".to_string(), json!(emotion));
        }
    }
    if let Some(add_silence) =
        payload_f64_alias(payload, &["addSilence", "add_silence"]).or_else(|| {
            nested_payload_f64_alias(payload, "voice_setting", &["addSilence", "add_silence"])
        })
    {
        body.insert("add_silence".to_string(), json!(add_silence));
    }
    for key in ["prefer_sync_tts", "prefer_async_tts", "async_tts"] {
        if let Some(value) = payload_field(payload, key).and_then(Value::as_bool) {
            body.insert(key.to_string(), json!(value));
        }
    }
    if let Some(audio_setting) = payload
        .get("audio_setting")
        .filter(|value| value.is_object())
    {
        body.insert("audio_setting".to_string(), audio_setting.clone());
    }
    for (source, target) in [
        ("sample_rate", "sample_rate"),
        ("bitrate", "bitrate"),
        ("channel", "channel"),
        ("format", "format"),
    ] {
        if let Some(value) = payload_field(payload, source) {
            body.insert(target.to_string(), value.clone());
        }
    }

    if let Some(voice_setting) = payload
        .get("voice_setting")
        .and_then(Value::as_object)
        .filter(|object| !object.is_empty())
    {
        let mut merged = voice_setting.clone();
        merged
            .entry("voice_id".to_string())
            .or_insert_with(|| json!(wire_voice_id(&voice_id)));
        body.insert("voice_setting".to_string(), Value::Object(merged));
    }

    if let Some(language_boost) =
        payload_string_alias(payload, &["languageBoost", "language_boost"])
    {
        body.insert("language_boost".to_string(), json!(language_boost));
    }
    if let Some(extra) = payload_field(payload, "extra").and_then(Value::as_object) {
        for (key, value) in extra {
            body.entry(key.clone()).or_insert(value.clone());
        }
    }
    Ok(body)
}

pub(crate) fn speech_sequence_segment_count(payload: &Value) -> usize {
    payload
        .get("segments")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

pub(crate) fn is_speech_sequence_payload(payload: &Value) -> bool {
    speech_sequence_segment_count(payload) > 1
}

pub(crate) fn speech_sequence_segments(payload: &Value) -> Result<Vec<Value>, String> {
    let segments = payload
        .get("segments")
        .and_then(Value::as_array)
        .ok_or_else(|| "voice.speech sequence requires segments".to_string())?;
    if segments.is_empty() {
        return Err("voice.speech sequence requires at least one segment".to_string());
    }
    if segments.len() > 50 {
        return Err("voice.speech sequence supports at most 50 segments".to_string());
    }
    let mut normalized = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        let input = payload_string(segment, "input")
            .or_else(|| payload_string(segment, "text"))
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("voice.speech sequence segment {} requires input", index + 1))?;
        let mut item = segment.clone();
        if let Some(object) = item.as_object_mut() {
            object.insert("input".to_string(), json!(input));
        }
        normalized.push(item);
    }
    Ok(normalized)
}

fn merge_object_field(target: &mut Map<String, Value>, parent: &Value, child: &Value, key: &str) {
    let mut merged = parent
        .get(key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(child_object) = child.get(key).and_then(Value::as_object) {
        for (child_key, child_value) in child_object {
            merged.insert(child_key.clone(), child_value.clone());
        }
    }
    if !merged.is_empty() {
        target.insert(key.to_string(), Value::Object(merged));
    }
}

pub(crate) fn speech_sequence_segment_payload(
    parent: &Value,
    segment: &Value,
    index: usize,
) -> Result<Value, String> {
    let mut object = Map::new();
    for key in [
        "voiceId",
        "voice_id",
        "voice",
        "voiceRef",
        "model",
        "responseFormat",
        "response_format",
        "format",
        "prompt",
        "stylePrompt",
        "style_prompt",
        "language_hints",
        "languageHints",
        "languageBoost",
        "language_boost",
        "speed",
        "speed_rate",
        "pitch",
        "emotion",
        "addSilence",
        "add_silence",
        "prefer_sync_tts",
        "prefer_async_tts",
        "async_tts",
        "projectId",
        "boundManuscriptPath",
        "sessionId",
        "ownerSessionId",
    ] {
        if let Some(value) = payload_field(parent, key) {
            object.insert(key.to_string(), value.clone());
        }
    }
    merge_object_field(&mut object, parent, segment, "voice_setting");
    merge_object_field(&mut object, parent, segment, "audio_setting");
    if let Some(parent_extra) = parent.get("extra").and_then(Value::as_object) {
        object.insert("extra".to_string(), Value::Object(parent_extra.clone()));
    }
    if let Some(segment_object) = segment.as_object() {
        for (key, value) in segment_object {
            if key == "voice_setting" || key == "audio_setting" {
                continue;
            }
            object.insert(key.clone(), value.clone());
        }
    }
    let input = payload_string(&Value::Object(object.clone()), "input")
        .or_else(|| payload_string(&Value::Object(object.clone()), "text"))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("voice.speech sequence segment {} requires input", index + 1))?;
    object.insert("input".to_string(), json!(input));
    let parent_title =
        payload_string(parent, "title").unwrap_or_else(|| "TTS sequence".to_string());
    object
        .entry("title".to_string())
        .or_insert_with(|| json!(format!("{parent_title} segment {}", index + 1)));
    object.insert("runtimeBypass".to_string(), json!(true));
    Ok(Value::Object(object))
}

fn synthesize_speech_inner(
    state: &State<'_, AppState>,
    payload: &Value,
    register_asset: bool,
    provider_template: &str,
) -> Result<Value, String> {
    let input = payload_string(payload, "input")
        .or_else(|| payload_string(payload, "text"))
        .ok_or_else(|| "voice.speech requires input".to_string())?;
    let voice_id = payload_string_alias(payload, &["voiceId", "voice_id", "voice"])
        .ok_or_else(|| "voice.speech requires voiceId".to_string())?;
    let config = resolve_voice_config(state, Some(payload))?;
    let model = payload_string(payload, "model")
        .or_else(|| {
            tts_model_for_voice_id_state(state, &voice_id)
                .ok()
                .flatten()
        })
        .or_else(|| (!config.tts_model.trim().is_empty()).then_some(config.tts_model.clone()))
        .ok_or_else(|| {
            "TTS model is not configured; choose a TTS model in Settings first".to_string()
        })?;
    if let Some(target_model) = payload_string_alias(
        payload,
        &[
            "voiceTargetTtsModel",
            "voice_target_tts_model",
            "targetTtsModel",
            "target_tts_model",
            "ttsModel",
            "tts_model",
        ],
    ) {
        if !target_model.trim().is_empty()
            && normalized_model_key(&target_model) != normalized_model_key(&model)
        {
            return Err(format!(
                "音色 {voice_id} 绑定到 TTS 模型 {target_model}，不能用于当前模型 {model}"
            ));
        }
    }
    validate_speech_voice_for_model(state, &voice_id, &model)?;
    let response_format =
        payload_string_alias(payload, &["responseFormat", "response_format", "format"])
            .or_else(|| nested_payload_string_alias(payload, "audio_setting", &["format"]))
            .unwrap_or_else(|| "mp3".to_string());
    let body = build_speech_request_body(
        payload,
        input.clone(),
        voice_id.clone(),
        model.clone(),
        response_format.clone(),
    )?;
    let bailian_config = if is_cosyvoice_model(&model) {
        let settings = with_store(state, |store| Ok(settings_store::settings_snapshot(&store)))?;
        crate::dashscope_tts::resolve_bailian_config(&settings)
    } else {
        None
    };
    let (audio_bytes, extension) = match bailian_config.as_ref() {
        Some(bailian) => {
            // 百炼专属端点直连：官方网关 cosyvoice 通道绑定固定默认音色，
            // 不透传百炼复刻音色，改走 DashScope WebSocket 协议联合成。
            let bytes = crate::dashscope_tts::synthesize(bailian, &model, &voice_id, &input)?;
            (bytes, response_format.as_str().to_string())
        }
        None => {
            let url = gateway_url(&config, "/audio/speech");
            let request_body = Value::Object(body);

            let client = Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .map_err(|error| error.to_string())?;
            let response = authorized_request(
                &client,
                reqwest::Method::POST,
                &url,
                config.api_key.as_deref(),
            )
            .json(&request_body)
            .send()
            .map_err(|error| error.to_string())?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(ToString::to_string);
            let bytes = response
                .bytes()
                .map_err(|error| error.to_string())?
                .to_vec();
            if !status.is_success() {
                let text = String::from_utf8_lossy(&bytes);
                return Err(format!("voice speech failed with HTTP {status}: {text}"));
            }
            decode_audio_response(content_type.as_deref(), bytes, response_format.as_str())?
        }
    };
    let title = payload_string(payload, "title").unwrap_or_else(|| {
        let stem = input.chars().take(24).collect::<String>();
        if stem.trim().is_empty() {
            "TTS".to_string()
        } else {
            stem
        }
    });
    let file_stem = make_id("tts");
    let relative_path = format!("generated/tts/{file_stem}.{extension}");
    let absolute_path = media_root(state)?.join(&relative_path);
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&absolute_path, audio_bytes).map_err(|error| error.to_string())?;
    let (mime_type, _, _) = guess_mime_and_kind(&absolute_path);
    let now = now_rfc3339();
    let asset = MediaAssetRecord {
        id: make_id("media"),
        source: "generated".to_string(),
        source_domain: Some("voice".to_string()),
        source_link: Some(format!("voice:{voice_id}")),
        project_id: payload_string(payload, "projectId"),
        title: Some(title),
        prompt: Some(input),
        provider: Some("voice".to_string()),
        provider_template: Some(provider_template.to_string()),
        model: Some(model),
        aspect_ratio: None,
        size: None,
        quality: None,
        mime_type: Some(mime_type),
        content_hash: file_content_hash(&absolute_path).ok(),
        relative_path: Some(relative_path.clone()),
        bound_manuscript_path: payload_string(payload, "boundManuscriptPath"),
        created_at: now.clone(),
        updated_at: now,
        absolute_path: Some(absolute_path.display().to_string()),
        preview_url: Some(file_url_for_path(&absolute_path)),
        thumbnail_url: None,
        exists: true,
    };
    if register_asset {
        with_store_mut(state, |store| {
            media_store::prepend_asset(store, asset.clone());
            Ok(())
        })?;
        persist_media_workspace_catalog(state)?;
    }
    Ok(json!({
        "success": true,
        "asset": asset,
        "voiceId": voice_id,
        "path": absolute_path.display().to_string(),
        "relativePath": relative_path,
    }))
}

fn validate_speech_voice_for_model(
    state: &State<'_, AppState>,
    voice_id: &str,
    model: &str,
) -> Result<(), String> {
    ensure_store_hydrated_for_subjects(state)?;
    let subjects = with_store(state, |store| Ok(subject_store::list_subjects(&store)))?;
    for subject in subjects {
        let Some(voice) = subject.voice.as_ref() else {
            continue;
        };
        let legacy_matches = payload_string_alias(voice, &["voiceId", "voice_id"])
            .map(|id| id == voice_id)
            .unwrap_or(false);
        if legacy_matches && voice_mapping_matches_model(voice, model) {
            return Ok(());
        }
        if let Some(mappings) = voice.get("voiceMappings").and_then(Value::as_object) {
            for mapping in mappings.values() {
                let mapping_matches_voice = payload_string_alias(mapping, &["voiceId", "voice_id"])
                    .map(|id| id == voice_id)
                    .unwrap_or(false);
                if mapping_matches_voice {
                    if voice_mapping_matches_model(mapping, model) {
                        return Ok(());
                    }
                    return Err(format!(
                        "音色 {voice_id} 不属于当前 TTS 模型 {model}，请切换模型或重新选择音色"
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn synthesize_speech(
    state: &State<'_, AppState>,
    payload: &Value,
) -> Result<Value, String> {
    synthesize_speech_inner(state, payload, true, "tts")
}

pub(crate) fn synthesize_speech_artifact(
    state: &State<'_, AppState>,
    payload: &Value,
) -> Result<Value, String> {
    synthesize_speech_inner(state, payload, false, "tts.segment")
}

fn voice_status(record: &SubjectRecord) -> Option<String> {
    record
        .voice
        .as_ref()
        .and_then(|value| payload_string(value, "status"))
}

fn subject_voice_has_id_for_tts_model(record: &SubjectRecord, tts_model: &str) -> bool {
    subject_voice_id_for_tts_model(record, tts_model).is_some()
}

fn subject_voice_sample_relative_path(record: &SubjectRecord) -> Option<String> {
    record
        .voice_path
        .clone()
        .or_else(|| record.video_path.clone())
}

pub(crate) fn spawn_subject_voice_clone_if_needed(
    app: &AppHandle,
    record: &SubjectRecord,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let config = resolve_voice_config(&state, None)?;
    let target_tts_model = clone_model_target_tts_model(&config.clone_model, &config.tts_model);
    if subject_voice_sample_relative_path(record).is_none()
        || subject_voice_has_id_for_tts_model(record, &target_tts_model)
    {
        return Ok(());
    }
    if !matches!(
        voice_status(record).as_deref(),
        Some("queued") | Some("failed") | None
    ) {
        return Ok(());
    }
    let payload = voice_clone_payload_for_subject(record, &config.clone_model, &target_tts_model)?;
    let submitted =
        match crate::media_runtime::submit_media_job(app, &state, "voice_clone", &payload) {
            Ok(value) => value,
            Err(error) => {
                let _ = patch_subject_voice_failure(&state, &record.id, error);
                return Ok(());
            }
        };
    let job_id = submitted
        .get("jobId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut queued = json!({
        "status": "queued",
        "jobId": job_id,
        "updatedAt": now_iso(),
    });
    if let Some(path) = payload.get("samplePath").cloned() {
        queued["sampleFilePath"] = path;
    }
    patch_subject_voice_state(&state, &record.id, queued)?;
    Ok(())
}

fn voice_clone_payload_for_subject(
    subject: &SubjectRecord,
    clone_model: &str,
    target_tts_model: &str,
) -> Result<Value, String> {
    let relative_path = subject_voice_sample_relative_path(subject)
        .ok_or_else(|| "subject has no voice sample".to_string())?;
    let sample_source = if subject.voice_path.is_some() {
        "voice"
    } else {
        "video"
    };
    let mut payload = json!({
        "ownerAssetId": subject.id,
        "samplePath": relative_path,
        "sampleSource": sample_source,
        "name": subject.name,
        "writeBack": true,
        "model": clone_model,
        "targetTtsModel": target_tts_model,
        "target_tts_model": target_tts_model,
    });
    if let Some(language) = subject
        .voice
        .as_ref()
        .and_then(|value| payload_string(value, "language"))
    {
        payload["language"] = json!(language);
    }
    Ok(payload)
}

fn merge_subject_voice_state(existing: Option<&Value>, voice: &Value) -> Value {
    let mut merged = existing.cloned().unwrap_or_else(|| json!({}));
    if !merged.is_object() {
        merged = json!({});
    }
    if let (Some(target), Some(source)) = (merged.as_object_mut(), voice.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
        if let Some(target_tts_model) = voice_target_tts_model(voice) {
            let mapping_key = normalized_model_key(&target_tts_model);
            let mut mapping = source.clone();
            // 排队阶段 voiceId 可能为 null，不能把空值写进按模型的映射，否则按键读取会拿到假音色。
            for key in ["voiceId", "voice_id"] {
                if mapping.get(key).map(Value::is_null).unwrap_or(false) {
                    mapping.remove(key);
                }
            }
            if let Some(voice_id) = payload_string_alias(voice, &["voiceId", "voice_id"]) {
                mapping.insert("voiceId".to_string(), json!(voice_id.clone()));
                mapping.insert("voice_id".to_string(), json!(voice_id));
            }
            mapping.insert(
                "targetTtsModel".to_string(),
                json!(target_tts_model.clone()),
            );
            mapping.insert(
                "target_tts_model".to_string(),
                json!(target_tts_model.clone()),
            );
            mapping.insert("ttsModel".to_string(), json!(target_tts_model));
            mapping.insert("updatedAt".to_string(), json!(now_iso()));
            let voice_mappings = target
                .entry("voiceMappings".to_string())
                .or_insert_with(|| json!({}));
            if !voice_mappings.is_object() {
                *voice_mappings = json!({});
            }
            if let Some(object) = voice_mappings.as_object_mut() {
                object.insert(mapping_key, Value::Object(mapping));
            }
        }
        target.insert("updatedAt".to_string(), json!(now_iso()));
        target.remove("lastError");
    }
    merged
}

fn patch_subject_voice_state(
    state: &State<'_, AppState>,
    subject_id: &str,
    voice: Value,
) -> Result<(), String> {
    ensure_store_hydrated_for_subjects(state)?;
    let root = subjects_root(state)?;
    let (categories, mut subjects) =
        with_store(state, |store| Ok(subject_store::catalog_snapshot(&store)))?;
    let Some(index) = subjects.iter().position(|subject| subject.id == subject_id) else {
        return Ok(());
    };
    let merged = merge_subject_voice_state(subjects[index].voice.as_ref(), &voice);
    subjects[index].voice = Some(merged);
    subjects[index].updated_at = now_iso();
    persist_subjects_workspace(&root, &categories, &subjects)?;
    with_store_mut(state, |store| {
        subject_store::replace_catalog(store, categories, subjects);
        Ok(())
    })
}

pub(crate) fn bind_subject_voice(
    state: &State<'_, AppState>,
    payload: &Value,
) -> Result<Value, String> {
    let subject_id = payload_string_alias(payload, &["ownerAssetId", "assetId", "subjectId", "id"])
        .ok_or_else(|| "voice.bindAsset requires ownerAssetId".to_string())?;
    let voice_id = payload_string_alias(payload, &["voiceId", "voice_id", "voice"])
        .ok_or_else(|| "voice.bindAsset requires voiceId".to_string())?;
    ensure_store_hydrated_for_subjects(state)?;
    let exists = with_store(state, |store| {
        Ok(subject_store::get_subject(&store, &subject_id).is_some())
    })?;
    if !exists {
        return Ok(json!({ "success": false, "error": "资产不存在" }));
    }
    let mut voice = json!({
        "voiceId": voice_id,
        "voice_id": voice_id,
        "status": payload_string(payload, "status").unwrap_or_else(|| "ready".to_string()),
        "updatedAt": now_iso(),
    });
    for key in [
        "name",
        "language",
        "cloneModel",
        "targetTtsModel",
        "target_tts_model",
        "ttsModel",
        "tts_model",
        "provider",
        "sampleFileKey",
        "sampleFilePath",
    ] {
        if let Some(value) = payload_field(payload, key).cloned() {
            voice[key] = value;
        }
    }
    patch_subject_voice_state(state, &subject_id, voice.clone())?;
    Ok(json!({ "success": true, "ownerAssetId": subject_id, "voice": voice }))
}

pub(crate) fn patch_subject_voice_queued(
    state: &State<'_, AppState>,
    subject_id: &str,
    job_id: &str,
    payload: &Value,
) -> Result<(), String> {
    let mut voice = json!({
        "status": "queued",
        "jobId": job_id,
        "voiceId": Value::Null,
        "voice_id": Value::Null,
        "updatedAt": now_iso(),
    });
    if let Some(path) = payload_string_alias(payload, &["samplePath", "sampleFilePath"]) {
        voice["sampleFilePath"] = json!(path);
    }
    if let Some(key) = payload_string_alias(payload, &["sampleFileKey", "sample_file_key"]) {
        voice["sampleFileKey"] = json!(key);
    }
    if let Some(language) = payload_string(payload, "language") {
        voice["language"] = json!(language);
    }
    for key in [
        "model",
        "targetTtsModel",
        "target_tts_model",
        "ttsModel",
        "tts_model",
    ] {
        if let Some(value) = payload_field(payload, key).cloned() {
            voice[key] = value;
        }
    }
    patch_subject_voice_state(state, subject_id, voice)
}

pub(crate) fn patch_subject_voice_failure(
    state: &State<'_, AppState>,
    subject_id: &str,
    error: String,
) -> Result<(), String> {
    ensure_store_hydrated_for_subjects(state)?;
    let root = subjects_root(state)?;
    let (categories, mut subjects) =
        with_store(state, |store| Ok(subject_store::catalog_snapshot(&store)))?;
    let Some(index) = subjects.iter().position(|subject| subject.id == subject_id) else {
        return Ok(());
    };
    let mut merged = subjects[index].voice.clone().unwrap_or_else(|| json!({}));
    if !merged.is_object() {
        merged = json!({});
    }
    if let Some(target) = merged.as_object_mut() {
        target.insert("status".to_string(), json!("failed"));
        target.insert("lastError".to_string(), json!(error));
        target.insert("voiceId".to_string(), Value::Null);
        target.insert("voice_id".to_string(), Value::Null);
        target.insert("updatedAt".to_string(), json!(now_iso()));
    }
    subjects[index].voice = Some(merged);
    subjects[index].updated_at = now_iso();
    persist_subjects_workspace(&root, &categories, &subjects)?;
    with_store_mut(state, |store| {
        subject_store::replace_catalog(store, categories, subjects);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_subject_with_voice(voice: Value) -> SubjectRecord {
        SubjectRecord {
            id: "subject-test".to_string(),
            name: "Test".to_string(),
            category_id: None,
            description: None,
            tags: Vec::new(),
            attributes: Vec::new(),
            image_paths: Vec::new(),
            voice_path: None,
            video_path: None,
            voice_script: None,
            voice: Some(voice),
            brand_id: None,
            skus: Vec::new(),
            created_at: "2026-05-19T00:00:00Z".to_string(),
            updated_at: "2026-05-19T00:00:00Z".to_string(),
            absolute_image_paths: Vec::new(),
            preview_urls: Vec::new(),
            primary_preview_url: None,
            absolute_voice_path: None,
            voice_preview_url: None,
            absolute_video_path: None,
            video_preview_url: None,
        }
    }

    #[test]
    fn subject_voice_merge_replaces_same_model_mapping() {
        let existing = json!({
            "voiceId": "voice_old",
            "voice_id": "voice_old",
            "targetTtsModel": "cosyvoice-v3.5-flash",
            "voiceMappings": {
                "cosyvoice-v3.5-flash": {
                    "voiceId": "voice_old",
                    "voice_id": "voice_old",
                    "targetTtsModel": "cosyvoice-v3.5-flash"
                },
                "speech-2.8-hd": {
                    "voiceId": "voice_minimax",
                    "voice_id": "voice_minimax",
                    "targetTtsModel": "speech-2.8-hd"
                }
            }
        });
        let next = json!({
            "voiceId": "voice_new",
            "voice_id": "voice_new",
            "targetTtsModel": "cosyvoice-v3.5-flash",
            "target_tts_model": "cosyvoice-v3.5-flash",
            "ttsModel": "cosyvoice-v3.5-flash",
            "status": "ready"
        });

        let merged = merge_subject_voice_state(Some(&existing), &next);

        assert_eq!(
            payload_string_alias(&merged, &["voiceId", "voice_id"]).as_deref(),
            Some("voice_new")
        );
        assert_eq!(
            merged
                .pointer("/voiceMappings/cosyvoice-v3.5-flash/voiceId")
                .and_then(Value::as_str),
            Some("voice_new")
        );
        assert_eq!(
            merged
                .pointer("/voiceMappings/speech-2.8-hd/voiceId")
                .and_then(Value::as_str),
            Some("voice_minimax")
        );
    }

    #[test]
    fn subject_voice_merge_tracks_pending_job_per_model() {
        let existing = json!({
            "voiceId": "voice_cosy",
            "voice_id": "voice_cosy",
            "targetTtsModel": "cosyvoice-v3.5-flash",
            "voiceMappings": {
                "cosyvoice-v3.5-flash": {
                    "voiceId": "voice_cosy",
                    "voice_id": "voice_cosy",
                    "targetTtsModel": "cosyvoice-v3.5-flash"
                }
            }
        });
        let queued = json!({
            "status": "queued",
            "jobId": "media-job-minimax",
            "voiceId": null,
            "voice_id": null,
            "targetTtsModel": "minimax",
            "target_tts_model": "minimax",
            "ttsModel": "minimax"
        });

        let merged = merge_subject_voice_state(Some(&existing), &queued);

        assert_eq!(
            merged
                .pointer("/voiceMappings/cosyvoice-v3.5-flash/voiceId")
                .and_then(Value::as_str),
            Some("voice_cosy")
        );
        assert_eq!(
            merged
                .pointer("/voiceMappings/minimax/jobId")
                .and_then(Value::as_str),
            Some("media-job-minimax")
        );
        assert!(merged.pointer("/voiceMappings/minimax/voiceId").is_none());
    }

    #[test]
    fn tts_model_for_voice_id_reads_model_bound_voice_mapping() {
        let subjects = vec![test_subject_with_voice(json!({
            "voiceId": "voice_legacy",
            "targetTtsModel": "speech-2.8-hd",
            "voiceMappings": {
                "cosyvoice-v3.5-flash": {
                    "voiceId": "voice_cosy",
                    "targetTtsModel": "cosyvoice-v3.5-flash"
                }
            }
        }))];

        assert_eq!(
            tts_model_for_voice_id_from_subjects(&subjects, "voice_cosy").as_deref(),
            Some("cosyvoice-v3.5-flash")
        );
    }

    #[test]
    fn voice_list_model_filter_requires_explicit_model_match() {
        assert!(voice_matches_selected_tts_model(
            &json!({
                "voiceId": "voice_cosy",
                "targetTtsModel": "cosyvoice-v3.5-flash"
            }),
            "cosyvoice-v3.5-flash"
        ));
        assert!(voice_matches_selected_tts_model(
            &json!({
                "voiceId": "voice_multi",
                "supportedModels": ["speech-2.8-hd"]
            }),
            "speech-2.8-hd"
        ));
        assert!(!voice_matches_selected_tts_model(
            &json!({
                "voiceId": "voice_cosy",
                "targetTtsModel": "cosyvoice-v3.5-flash"
            }),
            "speech-2.8-hd"
        ));
        assert!(!voice_matches_selected_tts_model(
            &json!({
                "voiceId": "voice_unknown"
            }),
            "speech-2.8-hd"
        ));
    }

    #[test]
    fn speech_request_body_forwards_delivery_controls() {
        let payload = json!({
            "speed": 1.05,
            "pitch": -1,
            "emotion": "whisper",
            "add_silence": 0.25,
            "prefer_sync_tts": true,
            "audio_setting": {
                "sample_rate": 32000,
                "bitrate": 128000,
                "channel": 1
            }
        });

        let body = build_speech_request_body(
            &payload,
            "第一段<#0.6#>第二段(laughs)。".to_string(),
            "male-qn-qingse".to_string(),
            "doubao-seed-tts-2.0".to_string(),
            "mp3".to_string(),
        )
        .expect("request body");

        assert_eq!(body.get("speed").and_then(Value::as_f64), Some(1.05));
        assert_eq!(body.get("pitch").and_then(Value::as_i64), Some(-1));
        assert_eq!(body.get("emotion").and_then(Value::as_str), Some("whipser"));
        assert_eq!(body.get("add_silence").and_then(Value::as_f64), Some(0.25));
        assert_eq!(
            body.get("prefer_sync_tts").and_then(Value::as_bool),
            Some(true)
        );
        let body_value = Value::Object(body.clone());
        assert_eq!(
            body_value
                .pointer("/audio_setting/sample_rate")
                .and_then(Value::as_i64),
            Some(32000)
        );
        assert_eq!(
            body.get("return_audio_binary").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn speech_request_body_reads_nested_voice_setting_controls() {
        let payload = json!({
            "voice_setting": {
                "speed": 0.92,
                "pitch": 2,
                "emotion": "calm"
            }
        });

        let body = build_speech_request_body(
            &payload,
            "慢一点，稳一点。".to_string(),
            "voice_xxx".to_string(),
            "doubao-seed-tts-2.0".to_string(),
            "mp3".to_string(),
        )
        .expect("request body");

        assert_eq!(body.get("speed").and_then(Value::as_f64), Some(0.92));
        assert_eq!(body.get("pitch").and_then(Value::as_i64), Some(2));
        assert_eq!(body.get("emotion").and_then(Value::as_str), Some("calm"));
        let body_value = Value::Object(body.clone());
        assert_eq!(
            body_value
                .pointer("/voice_setting/voice_id")
                .and_then(Value::as_str),
            Some("voice_xxx")
        );
    }

    #[test]
    fn speech_request_body_forwards_prompt_and_language_hints() {
        let payload = json!({
            "prompt": "请用温柔、平稳的语气朗读。",
            "language_hints": ["zh"],
            "sample_rate": 24000,
            "emotion": "happy",
            "voice_setting": {
                "emotion": "sad"
            }
        });

        let body = build_speech_request_body(
            &payload,
            "这里是开场白，接下来进入重点。".to_string(),
            "voice_xxx".to_string(),
            "doubao-seed-tts-2.0".to_string(),
            "mp3".to_string(),
        )
        .expect("request body");
        let body_value = Value::Object(body);

        assert_eq!(
            body_value.get("prompt").and_then(Value::as_str),
            Some("请用温柔、平稳的语气朗读。")
        );
        assert_eq!(
            body_value
                .get("language_hints")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(Value::as_str),
            Some("zh")
        );
        assert_eq!(
            body_value.get("sample_rate").and_then(Value::as_i64),
            Some(24000)
        );
        assert_eq!(
            body_value.get("emotion").and_then(Value::as_str),
            Some("happy")
        );
        assert_eq!(
            body_value.pointer("/voice_setting/emotion").and_then(Value::as_str),
            Some("sad")
        );
    }

    #[test]
    fn speech_request_body_accepts_plain_doubao_input() {
        let payload = json!({
            "prompt": "请用清晰、平稳的说明语气朗读。",
            "language_hints": ["zh"]
        });
        let body = build_speech_request_body(
            &payload,
            "在 Mac 上彻底删除文件，不是移到废纸篓，有几种方法。".to_string(),
            "voice_xxx".to_string(),
            "doubao-seed-tts-2.0".to_string(),
            "mp3".to_string(),
        )
        .expect("plain doubao input should pass");

        assert_eq!(
            body.get("input").and_then(Value::as_str),
            Some("在 Mac 上彻底删除文件，不是移到废纸篓，有几种方法。")
        );
    }

    #[test]
    fn speech_request_body_rejects_invalid_delivery_controls() {
        let payload = json!({
            "speed": 2.5,
            "pitch": 0,
            "emotion": "happy"
        });
        let error = build_speech_request_body(
            &payload,
            "文本".to_string(),
            "voice_xxx".to_string(),
            "doubao-seed-tts-2.0".to_string(),
            "mp3".to_string(),
        )
        .expect_err("speed should be rejected");

        assert!(error.contains("speed"));
    }

    #[test]
    fn speech_request_body_rejects_unsupported_cosyvoice_ssml() {
        let payload = json!({});
        let error = build_speech_request_body(
            &payload,
            "<prosody rate=\"0.9\">你好</prosody>".to_string(),
            "voice_xxx".to_string(),
            "cosyvoice-v3.5-flash".to_string(),
            "mp3".to_string(),
        )
        .expect_err("unsupported CosyVoice SSML should be rejected");

        assert!(error.contains("CosyVoice does not support `<prosody>`"));
    }

    #[test]
    fn speech_request_body_accepts_cosyvoice_speak_attributes() {
        let payload = json!({
            "pitch": 0.95,
            "emotion": "happy"
        });
        let body = build_speech_request_body(
            &payload,
            "<speak rate=\"0.9\" pitch=\"0.95\" volume=\"60\">你好</speak>".to_string(),
            "voice_xxx".to_string(),
            "cosyvoice-v3.5-flash".to_string(),
            "mp3".to_string(),
        )
        .expect("valid CosyVoice input should pass");

        assert_eq!(body.get("pitch").and_then(Value::as_f64), Some(0.95));
        assert!(body.get("emotion").is_none());
    }

    #[test]
    fn speech_request_body_rejects_out_of_range_cosyvoice_pitch() {
        let payload = json!({ "pitch": 12 });
        let error = build_speech_request_body(
            &payload,
            "你好".to_string(),
            "voice_xxx".to_string(),
            "cosyvoice-v3.5-flash".to_string(),
            "mp3".to_string(),
        )
        .expect_err("CosyVoice pitch must stay on the 0.5-2.0 scale");

        assert!(error.contains("pitch"));
    }

    #[test]
    fn clone_model_target_tts_model_maps_current_models() {
        assert_eq!(
            clone_model_target_tts_model("doubao-voice-clone", ""),
            "doubao-seed-tts-2.0"
        );
        assert_eq!(
            clone_model_target_tts_model("cosyvoice-v3.5-flash-voice-clone", ""),
            "cosyvoice-v3.5-flash"
        );
    }

    #[test]
    fn bailian_custom_voices_append_from_settings() {
        let settings = json!({
            "bailian_custom_voices_json": r#"[
                {"id":"cosyvoice-v3.5-flash-bailian-abc123","name":"测试音色"},
                {"id":""},
                {"voiceId":"cosyvoice-v3.5-flash-bailian-def456"},
                {"id":"cosyvoice-v3.5-flash-bailian-abc123","name":"重复项"}
            ]"#,
        });
        let mut voices = Vec::new();
        let mut seen = HashSet::new();
        append_bailian_custom_voices(&mut voices, &mut seen, &settings);
        assert_eq!(voices.len(), 2, "empty id and duplicates must be skipped");
        assert_eq!(voices[0]["voice_id"], "cosyvoice-v3.5-flash-bailian-abc123");
        assert_eq!(voices[0]["name"], "测试音色");
        assert_eq!(voices[0]["provider"], "custom");
        // 缺省备注回落为 ID 尾段。
        assert_eq!(voices[1]["name"], "自定义复刻音色 · def456");
        // 无配置时不注入。
        let mut empty = Vec::new();
        let mut empty_seen = HashSet::new();
        append_bailian_custom_voices(&mut empty, &mut empty_seen, &json!({}));
        assert!(empty.is_empty());
    }

    #[test]
    fn adapter_system_voice_catalog_parses_default_entries() {
        let items = adapter_system_voice_list_items();
        let ids: Vec<String> = items
            .iter()
            .filter_map(|item| item.get("voice_id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        assert!(ids.contains(&"volc-tts-default".to_string()));
        assert!(ids.contains(&"cosyvoice-tts-default".to_string()));
        // 火山平台：豆包官方 2.0 音色（uranus 系列）21 个，逐音色实测透传可用。
        let volc_real = ids.iter().filter(|id| id.ends_with("_uranus_bigtts")).count();
        assert_eq!(volc_real, 21, "volc uranus 2.0 voices should be present");
        // 百炼平台：固定复刻音色 10 个，直连专属端点合成。
        let bailian = ids
            .iter()
            .filter(|id| id.starts_with("cosyvoice-v3.5-flash-bailian-"))
            .count();
        assert_eq!(bailian, 10, "bailian cloned voices should be present");
        for item in &items {
            assert_eq!(item.get("status").and_then(Value::as_str), Some("ready"));
            assert_eq!(item.get("provider").and_then(Value::as_str), Some("adapter"));
        }
    }

    #[test]
    fn speech_request_body_maps_cosyvoice_to_gateway_channel_model() {
        let payload = json!({});
        let body = build_speech_request_body(
            &payload,
            "你好".to_string(),
            "cosyvoice-tts-default".to_string(),
            "cosyvoice-v3.5-flash".to_string(),
            "mp3".to_string(),
        )
        .expect("adapter default voice should pass");

        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some(GATEWAY_COSYVOICE_CHANNEL_MODEL)
        );
        assert_eq!(body.get("voice_id").and_then(Value::as_str), Some(""));
    }

    #[test]
    fn speech_request_body_maps_adapter_default_voice_for_doubao() {
        let payload = json!({});
        let body = build_speech_request_body(
            &payload,
            "你好".to_string(),
            "volc-tts-default".to_string(),
            "doubao-seed-tts-2.0".to_string(),
            "mp3".to_string(),
        )
        .expect("adapter default voice should pass");

        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some(DOUBAO_TTS_MODEL)
        );
        assert_eq!(body.get("voice_id").and_then(Value::as_str), Some(""));

        let body_with_real_voice = build_speech_request_body(
            &payload,
            "你好".to_string(),
            "cloned_voice_01".to_string(),
            "doubao-seed-tts-2.0".to_string(),
            "mp3".to_string(),
        )
        .expect("cloned voice should pass");
        assert_eq!(
            body_with_real_voice
                .get("voice_id")
                .and_then(Value::as_str),
            Some("cloned_voice_01")
        );
    }

    #[test]
    fn speech_sequence_segment_payload_inherits_and_overrides_controls() {
        let parent = json!({
            "voiceId": "voice_parent",
            "model": "speech-2.8-hd",
            "prompt": "旁白语气平稳。",
            "language_hints": ["zh"],
            "speed": 0.95,
            "emotion": "calm",
            "audio_setting": {
                "sample_rate": 32000,
                "channel": 1
            },
            "voice_setting": {
                "pitch": 0,
                "add_silence": 0.2
            },
            "segments": [
                {
                    "input": "第一段",
                    "speed": 1.08,
                    "emotion": "whisper",
                    "voice_setting": {
                        "pitch": 2
                    }
                }
            ]
        });
        let segment = parent
            .get("segments")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .expect("segment");
        let payload = speech_sequence_segment_payload(&parent, segment, 0).expect("payload");

        assert_eq!(
            payload.get("voiceId").and_then(Value::as_str),
            Some("voice_parent")
        );
        assert_eq!(payload.get("speed").and_then(Value::as_f64), Some(1.08));
        assert_eq!(
            payload.get("emotion").and_then(Value::as_str),
            Some("whisper")
        );
        assert_eq!(
            payload
                .pointer("/audio_setting/sample_rate")
                .and_then(Value::as_i64),
            Some(32000)
        );
        assert_eq!(
            payload.get("prompt").and_then(Value::as_str),
            Some("旁白语气平稳。")
        );
        assert_eq!(
            payload
                .get("language_hints")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(Value::as_str),
            Some("zh")
        );
        assert_eq!(
            payload
                .pointer("/voice_setting/pitch")
                .and_then(Value::as_i64),
            Some(2)
        );
        assert_eq!(
            payload
                .pointer("/voice_setting/add_silence")
                .and_then(Value::as_f64),
            Some(0.2)
        );
        let body = build_speech_request_body(
            &payload,
            "第一段".to_string(),
            "voice_parent".to_string(),
            "doubao-seed-tts-2.0".to_string(),
            "mp3".to_string(),
        )
        .expect("body");
        assert_eq!(body.get("emotion").and_then(Value::as_str), Some("whipser"));
    }

    #[test]
    fn speech_sequence_detection_requires_multiple_segments() {
        assert!(!is_speech_sequence_payload(
            &json!({ "segments": [{ "input": "一段" }] })
        ));
        assert!(is_speech_sequence_payload(&json!({
            "segments": [
                { "input": "一段" },
                { "input": "二段" }
            ]
        })));
    }
}
