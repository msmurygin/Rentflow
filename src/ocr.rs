//! Распознавание показаний по фото. ИИ только предлагает значение: оно никогда не пишется в базу
//! без подтверждения человеком и никогда не участвует в расчёте денег.

use async_trait::async_trait;

/// Предложение распознавателя по одному фото.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// `cold` | `hot` | `t1` | `t2`; `None`, если регистр определить не удалось.
    pub register: Option<String>,
    /// Показание строкой (с дробной частью, если видна); `None`, если не прочитано.
    pub value: Option<String>,
    /// `low` | `medium` | `high`.
    pub confidence: String,
}

#[async_trait]
pub trait Extractor: Send + Sync {
    /// `Err` — сервис недоступен или ответ не разобран: бот переходит на ручной ввод.
    async fn extract(&self, image: &[u8]) -> Result<Vec<Candidate>, String>;
}

/// Распознавание отключено (нет ключа): всё вводится вручную.
pub struct NoExtractor;

#[async_trait]
impl Extractor for NoExtractor {
    async fn extract(&self, _image: &[u8]) -> Result<Vec<Candidate>, String> {
        Err("распознавание фото не настроено".into())
    }
}

// ---------------------------------------------------------------------------------------------
// Claude (vision) через Messages API. Официального Rust SDK нет — обычный HTTPS-запрос.
// ---------------------------------------------------------------------------------------------

use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

const SYSTEM_PROMPT: &str = "Ты читаешь показания коммунальных счётчиков по фотографии. \
Отвечай ТОЛЬКО JSON без пояснений и без markdown.\n\
Формат: {\"readings\":[{\"register\":\"cold|hot|t1|t2|unknown\",\"value\":\"строка или null\",\"confidence\":\"low|medium|high\"}]}\n\
Правила:\n\
- cold — счётчик холодной воды (обычно синий), hot — горячей воды (обычно красный), t1/t2 — тарифные регистры электросчётчика.\n\
- У водяных счётчиков чёрные цифры — целая часть (м³), красные — дробная. Верни значение как «целая.дробная», например 00761.412 → \"761.412\". \
Если дробные цифры не видны, верни только целую часть.\n\
- У электросчётчика регистр определяется по индикатору тарифа на дисплее (T1 или T2). Если индикатора нет, виден только суммарный счётчик или тариф неясен — register=\"unknown\". \
Никогда не выбирай T1/T2 наугад.\n\
- Если цифру нельзя прочитать уверенно, поставь confidence=\"low\"; если значение прочитать нельзя совсем — value=null. Не додумывай цифры.\n\
- Не складывай и не вычисляй ничего. Только то, что написано на приборе.\n\
- Если на фото нет счётчика — {\"readings\":[]}.";

pub struct ClaudeExtractor {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl ClaudeExtractor {
    pub fn new(api_key: String, model: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("http client");
        Self {
            client,
            api_key,
            model,
            base_url: "https://api.anthropic.com".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn request_body(&self, image: &[u8]) -> Value {
        let data = base64::engine::general_purpose::STANDARD.encode(image);
        json!({
            "model": self.model,
            "max_tokens": 4096,
            // thinking на этой модели всегда включён; для чтения цифр хватает минимального усилия
            "output_config": { "effort": "low" },
            "system": SYSTEM_PROMPT,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": data } },
                    { "type": "text", "text": "Прочитай показание на этом фото." }
                ]
            }]
        })
    }
}

#[derive(Deserialize)]
struct RawReadings {
    #[serde(default)]
    readings: Vec<RawReading>,
}

#[derive(Deserialize)]
struct RawReading {
    register: Option<String>,
    value: Option<Value>,
    confidence: Option<String>,
}

/// Достаёт кандидатов из текста ответа модели. Терпимо к обрамлению (```json ...```), строго к содержанию:
/// неизвестный регистр → `None`, нечисловое значение → `None`, неизвестная уверенность → `low`.
pub fn parse_candidates(text: &str) -> Result<Vec<Candidate>, String> {
    let (start, end) = (text.find('{'), text.rfind('}'));
    let (Some(s), Some(e)) = (start, end) else {
        return Err("в ответе нет JSON".into());
    };
    if e < s {
        return Err("в ответе нет JSON".into());
    }
    let raw: RawReadings = serde_json::from_str(&text[s..=e])
        .map_err(|e| format!("не удалось разобрать JSON: {e}"))?;
    Ok(raw
        .readings
        .into_iter()
        .map(|r| {
            let register = r
                .register
                .map(|x| x.trim().to_lowercase())
                .filter(|x| matches!(x.as_str(), "cold" | "hot" | "t1" | "t2"));
            let value = match r.value {
                Some(Value::String(s)) => Some(s),
                Some(Value::Number(n)) => Some(n.to_string()),
                _ => None,
            }
            .filter(|s| !s.trim().is_empty());
            let confidence = match r.confidence.as_deref() {
                Some("high") => "high",
                Some("medium") => "medium",
                _ => "low",
            };
            Candidate {
                register,
                value,
                confidence: confidence.into(),
            }
        })
        .collect())
}

#[async_trait]
impl Extractor for ClaudeExtractor {
    async fn extract(&self, image: &[u8]) -> Result<Vec<Candidate>, String> {
        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&self.request_body(image))
            .send()
            .await
            .map_err(|e| format!("сеть: {e}"))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| format!("ответ не JSON: {e}"))?;
        if !status.is_success() {
            let msg = body["error"]["message"].as_str().unwrap_or("без описания");
            return Err(format!("API {status}: {msg}"));
        }
        if body["stop_reason"] == "refusal" {
            return Err("модель отказалась отвечать".into());
        }
        let text: String = body["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        parse_candidates(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_fenced_json() {
        let plain = r#"{"readings":[{"register":"cold","value":"761.412","confidence":"high"}]}"#;
        let c = parse_candidates(plain).unwrap();
        assert_eq!(
            c,
            vec![Candidate {
                register: Some("cold".into()),
                value: Some("761.412".into()),
                confidence: "high".into()
            }]
        );
        let fenced = format!("```json\n{plain}\n```");
        assert_eq!(parse_candidates(&fenced).unwrap(), c);
    }

    #[test]
    fn unknown_register_and_garbage_values_become_none() {
        let c = parse_candidates(r#"{"readings":[{"register":"unknown","value":null,"confidence":"???"},{"register":"T2","value":1179,"confidence":"medium"}]}"#).unwrap();
        assert_eq!(
            c[0],
            Candidate {
                register: None,
                value: None,
                confidence: "low".into()
            }
        );
        assert_eq!(
            c[1].register.as_deref(),
            Some("t2"),
            "регистр нормализуется"
        );
        assert_eq!(
            c[1].value.as_deref(),
            Some("1179"),
            "число принимается и как число"
        );
    }

    #[test]
    fn non_json_is_an_error_not_a_guess() {
        assert!(parse_candidates("не вижу счётчика").is_err());
        assert!(parse_candidates("{ broken").is_err());
        assert_eq!(parse_candidates(r#"{"readings":[]}"#).unwrap(), vec![]);
    }

    #[test]
    fn request_carries_image_and_never_asks_for_money() {
        let e = ClaudeExtractor::new("k".into(), "claude-opus-5-5".into());
        let body = e.request_body(&[0xff, 0xd8, 0xff]);
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["messages"][0]["content"][0]["type"], "image");
        assert_eq!(
            body["messages"][0]["content"][0]["source"]["media_type"],
            "image/jpeg"
        );
        assert!(
            body.get("thinking").is_none(),
            "thinking не задаём: на этой модели он всегда включён"
        );
        let system = body["system"].as_str().unwrap();
        assert!(system.contains("Не складывай и не вычисляй"));
    }
}
