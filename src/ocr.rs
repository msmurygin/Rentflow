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
