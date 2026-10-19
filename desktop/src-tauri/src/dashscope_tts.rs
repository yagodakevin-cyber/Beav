//! 阿里云百炼（DashScope MaaS 专属端点）TTS 直连通道。
//!
//! 官方网关的 cosyvoice 通道绑定固定默认音色，不透传百炼复刻音色；
//! 因此 `cosyvoice-v3.5-flash` 模型在配置了百炼专属源（baseURL 含
//! `maas.aliyuncs.com`）时改走 DashScope WebSocket 协议直联合成。

use futures_util::{SinkExt as _, StreamExt};
use serde_json::{json, Value};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header, HeaderValue};
use tokio_tungstenite::tungstenite::Message;

/// 默认音色占位符回落到的首个复刻音色。
pub const DEFAULT_BAILIAN_VOICE: &str =
    "cosyvoice-v3.5-flash-bailian-96383386bcaa435da39bc6bdb5e2824c";

const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const WS_TASK_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone)]
pub struct BailianTtsConfig {
    pub http_base: String,
    pub api_key: String,
}

impl BailianTtsConfig {
    /// `https://<host>/api/v1` → `wss://<host>/api-ws/v1/inference`。
    pub fn ws_url(&self) -> String {
        let rest = self
            .http_base
            .trim()
            .trim_end_matches('/')
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        let host = rest.split("/api/").next().unwrap_or(rest);
        format!("wss://{host}/api-ws/v1/inference")
    }
}

fn is_bailian_endpoint(base_url: &str) -> bool {
    let lowered = base_url.trim().to_ascii_lowercase();
    lowered.contains(".maas.aliyuncs.com") || lowered.contains("dashscope.aliyuncs.com")
}

/// 从设置 `ai_sources_json` 中解析百炼专属源（首个 maas/dashscope 端点）。
pub fn resolve_bailian_config(settings: &Value) -> Option<BailianTtsConfig> {
    let sources_json = settings.get("ai_sources_json").and_then(Value::as_str)?;
    let sources = serde_json::from_str::<Value>(sources_json).ok()?;
    let items = sources.as_array()?;
    for item in items {
        let base_url = item
            .get("baseURL")
            .or_else(|| item.get("base_url"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if base_url.is_empty() || !is_bailian_endpoint(&base_url) {
            continue;
        }
        let api_key = item
            .get("apiKey")
            .or_else(|| item.get("api_key"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if api_key.is_empty() {
            continue;
        }
        return Some(BailianTtsConfig {
            http_base: base_url,
            api_key,
        });
    }
    None
}

/// 音色 ID 解析：空值/默认占位符 → 首个复刻音色；其余原样透传。
pub fn resolve_bailian_voice(voice_id: &str) -> String {
    let trimmed = voice_id.trim();
    if trimmed.is_empty() || trimmed == "cosyvoice-tts-default" {
        DEFAULT_BAILIAN_VOICE.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 生成 uuid v4 格式任务 ID（仅用于 WS 帧关联，无需密码学随机）。
fn make_task_id() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default(),
    );
    let raw = hasher.finish() as u128 ^ (std::process::id() as u128) << 64;
    let bytes = raw.to_le_bytes();
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let expanded = format!("{hex}{hex}"); // 32 个 hex 字符
    format!(
        "{}-{}-4{}-8{}-{}",
        &expanded[0..8],
        &expanded[8..12],
        &expanded[13..16],
        &expanded[17..20],
        &expanded[20..32],
    )
}

fn header_frame(action: &str, task_id: &str) -> Value {
    json!({
        "header": {
            "action": action,
            "task_id": task_id,
            "streaming": "duplex",
        }
    })
}

fn run_task_request(task_id: &str, model: &str, voice: &str) -> Value {
    let mut frame = header_frame("run-task", task_id);
    frame["payload"] = json!({
        "model": model,
        "task_group": "audio",
        "task": "tts",
        "function": "SpeechSynthesizer",
        "input": {},
        "parameters": {
            "voice": voice,
            "volume": 50,
            "text_type": "PlainText",
            "sample_rate": 22050,
            "rate": 1.0,
            "format": "mp3",
            "pitch": 1.0,
            "seed": 0,
            "type": 0,
        },
    });
    frame
}

fn continue_task_request(task_id: &str, text: &str) -> Value {
    let mut frame = header_frame("continue-task", task_id);
    frame["payload"] = json!({
        "model": "cosyvoice-v3.5-flash",
        "task_group": "audio",
        "task": "tts",
        "function": "SpeechSynthesizer",
        "input": { "text": text },
    });
    frame
}

fn finish_task_request(task_id: &str) -> Value {
    let mut frame = header_frame("finish-task", task_id);
    frame["payload"] = json!({ "input": {} });
    frame
}

fn task_failed_error(event: &Value) -> Option<String> {
    let header = event.get("header")?;
    if header.get("event").and_then(Value::as_str) == Some("task-failed") {
        let code = header
            .get("error_code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = header
            .get("error_message")
            .and_then(Value::as_str)
            .unwrap_or("bailian tts task failed");
        return Some(format!("bailian tts task failed [{code}]: {message}"));
    }
    None
}

/// 通过百炼专属端点 WebSocket 合成一段文本，返回完整 MP3 字节。
pub fn synthesize(
    config: &BailianTtsConfig,
    model: &str,
    voice_id: &str,
    input: &str,
) -> Result<Vec<u8>, String> {
    let ws_url = config.ws_url();
    let api_key = config.api_key.clone();
    let model = model.trim().to_string();
    let voice_id = resolve_bailian_voice(voice_id);
    let input = input.to_string();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("bailian tts runtime init failed: {error}"))?;
    runtime.block_on(async move {
        let mut request = ws_url
            .as_str()
            .into_client_request()
            .map_err(|error| format!("bailian tts ws url invalid: {error}"))?;
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|error| format!("bailian tts auth header invalid: {error}"))?,
        );
        let connect = tokio::time::timeout(
            WS_CONNECT_TIMEOUT,
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                None,
                false,
                None,
            ),
        )
        .await
        .map_err(|_| "bailian tts ws connect timeout".to_string())?;
        let (mut stream, _) = connect
            .map_err(|error| format!("bailian tts ws connect failed: {error}"))?;

        let task_id = make_task_id();
        stream
            .send(Message::text(
                serde_json::to_string(&run_task_request(&task_id, &model, &voice_id))
                    .map_err(|error| error.to_string())?,
            ))
            .await
            .map_err(|error| format!("bailian tts ws send run-task failed: {error}"))?;

        let mut audio: Vec<u8> = Vec::new();
        let mut started = false;
        let mut sent_text = false;
        let mut finished = false;
        let overall = tokio::time::timeout(WS_TASK_TIMEOUT, async {
            while let Some(message) = stream.next().await {
                let message =
                    message.map_err(|error| format!("bailian tts ws stream error: {error}"))?;
                match message {
                    Message::Text(text) => {
                        let event: Value = serde_json::from_str(&text)
                            .map_err(|error| format!("bailian tts ws bad frame: {error}"))?;
                        if let Some(error) = task_failed_error(&event) {
                            return Err(error);
                        }
                        let action = event
                            .get("header")
                            .and_then(|header| header.get("event"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        match action.as_str() {
                            "task-started" => {
                                started = true;
                                let continue_frame = continue_task_request(&task_id, &input);
                                stream
                                    .send(Message::text(serde_json::to_string(&continue_frame).map_err(|e| e.to_string())?))
                                    .await
                                    .map_err(|error| format!("bailian tts ws send text failed: {error}"))?;
                                sent_text = true;
                                let finish_frame = finish_task_request(&task_id);
                                stream
                                    .send(Message::text(serde_json::to_string(&finish_frame).map_err(|e| e.to_string())?))
                                    .await
                                    .map_err(|error| format!("bailian tts ws send finish failed: {error}"))?;
                            }
                            "task-finished" => {
                                finished = true;
                                break;
                            }
                            _ => {}
                        }
                    }
                    Message::Binary(bytes) => {
                        audio.extend_from_slice(&bytes);
                    }
                    Message::Close(frame) => {
                        return Err(format!(
                            "bailian tts ws closed unexpectedly: {}",
                            frame
                                .map(|info| info.code.to_string())
                                .unwrap_or_default()
                        ));
                    }
                    _ => {}
                }
            }
            Ok(())
        })
        .await;

        match overall {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err("bailian tts task timeout".to_string()),
        }
        if !started || !sent_text {
            return Err("bailian tts task never started".to_string());
        }
        if !finished {
            return Err("bailian tts task finished event missing".to_string());
        }
        if audio.is_empty() {
            return Err("bailian tts returned no audio".to_string());
        }
        Ok(audio)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_derives_from_http_base() {
        let config = BailianTtsConfig {
            http_base: "https://llm-demo.cn-beijing.maas.aliyuncs.com/api/v1".to_string(),
            api_key: "sk-x".to_string(),
        };
        assert_eq!(
            config.ws_url(),
            "wss://llm-demo.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference"
        );
    }

    #[test]
    fn resolve_config_scans_ai_sources() {
        let settings = json!({
            "ai_sources_json": r#"[{"id":"x","baseURL":"https://api.ziz.hk/beav/v1","apiKey":"rbx_"},{"id":"b","baseURL":"https://llm-demo.cn-beijing.maas.aliyuncs.com/api/v1","apiKey":"sk-1"}]"#,
        });
        let config = resolve_bailian_config(&settings).expect("bailian config");
        assert_eq!(config.api_key, "sk-1");
        assert_eq!(
            config.ws_url(),
            "wss://llm-demo.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference"
        );
    }

    #[test]
    fn resolve_bailian_voice_defaults_placeholder() {
        assert_eq!(resolve_bailian_voice(""), DEFAULT_BAILIAN_VOICE);
        assert_eq!(
            resolve_bailian_voice("cosyvoice-tts-default"),
            DEFAULT_BAILIAN_VOICE
        );
        assert_eq!(
            resolve_bailian_voice("cosyvoice-v3.5-flash-bailian-abc"),
            "cosyvoice-v3.5-flash-bailian-abc"
        );
    }

    #[test]
    fn task_id_is_uuid_shaped() {
        let id = make_task_id();
        assert_eq!(id.len(), 36);
        assert_eq!(id.chars().filter(|c| *c == '-').count(), 4);
    }
}
