//! Проверка `initData` Telegram Mini App (https://core.telegram.org/bots/webapps#validating-data-received-via-the-mini-app).
//! Идентификатору пользователя, присланному отдельно от подписанной строки, не доверяем никогда.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgUser {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitDataError {
    Malformed,
    BadSignature,
    Expired,
}

fn mac(key: &[u8], data: &[u8]) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC принимает ключ любой длины");
    m.update(data);
    m
}

/// Строка для подписи: все поля кроме `hash`, по ключам, `key=value`, через `\n`.
fn data_check_string(pairs: &[(String, String)]) -> String {
    let mut rest: Vec<&(String, String)> = pairs.iter().filter(|(k, _)| k != "hash").collect();
    rest.sort_by(|a, b| a.0.cmp(&b.0));
    rest.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn secret_key(bot_token: &str) -> Vec<u8> {
    mac(b"WebAppData", bot_token.as_bytes())
        .finalize()
        .into_bytes()
        .to_vec()
}

/// Проверяет подпись и свежесть. `now_unix` передаётся снаружи, чтобы проверку можно было тестировать.
pub fn verify_init_data(
    init_data: &str,
    bot_token: &str,
    now_unix: i64,
    max_age_secs: i64,
) -> Result<TgUser, InitDataError> {
    let pairs: Vec<(String, String)> = form_urlencoded::parse(init_data.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let hash = get("hash").ok_or(InitDataError::Malformed)?;
    let hash = hex::decode(hash).map_err(|_| InitDataError::Malformed)?;
    // verify_slice сравнивает за постоянное время
    mac(&secret_key(bot_token), data_check_string(&pairs).as_bytes())
        .verify_slice(&hash)
        .map_err(|_| InitDataError::BadSignature)?;

    let auth_date: i64 = get("auth_date")
        .and_then(|v| v.parse().ok())
        .ok_or(InitDataError::Malformed)?;
    if now_unix - auth_date > max_age_secs || auth_date - now_unix > 60 {
        return Err(InitDataError::Expired);
    }
    let user: serde_json::Value =
        serde_json::from_str(get("user").ok_or(InitDataError::Malformed)?)
            .map_err(|_| InitDataError::Malformed)?;
    let id = user["id"].as_i64().ok_or(InitDataError::Malformed)?;
    let name = [user["first_name"].as_str(), user["last_name"].as_str()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    Ok(TgUser { id, name })
}

/// Собирает подписанную строку `initData` так, как это делает Telegram. Нужна тестам (и только им).
pub fn sign_init_data(fields: &[(&str, &str)], bot_token: &str) -> String {
    let pairs: Vec<(String, String)> = fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let hash = hex::encode(
        mac(&secret_key(bot_token), data_check_string(&pairs).as_bytes())
            .finalize()
            .into_bytes(),
    );
    let mut ser = form_urlencoded::Serializer::new(String::new());
    for (k, v) in fields {
        ser.append_pair(k, v);
    }
    ser.append_pair("hash", &hash);
    ser.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "123456:TEST";
    const USER: &str = r#"{"id":42,"first_name":"Ivan","last_name":"P"}"#;

    fn signed(auth_date: &str) -> String {
        sign_init_data(
            &[("auth_date", auth_date), ("user", USER), ("query_id", "q")],
            TOKEN,
        )
    }

    #[test]
    fn valid_signature_is_accepted() {
        let u = verify_init_data(&signed("1000"), TOKEN, 1100, 3600).unwrap();
        assert_eq!(
            u,
            TgUser {
                id: 42,
                name: "Ivan P".into()
            }
        );
    }

    #[test]
    fn wrong_token_or_tampered_field_is_rejected() {
        let ok = signed("1000");
        assert_eq!(
            verify_init_data(&ok, "other:TOKEN", 1100, 3600),
            Err(InitDataError::BadSignature)
        );
        let tampered = ok.replace("%22id%22%3A42", "%22id%22%3A43");
        assert_ne!(ok, tampered);
        assert_eq!(
            verify_init_data(&tampered, TOKEN, 1100, 3600),
            Err(InitDataError::BadSignature)
        );
    }

    #[test]
    fn missing_hash_or_garbage_is_malformed() {
        assert_eq!(
            verify_init_data("auth_date=1", TOKEN, 1, 10),
            Err(InitDataError::Malformed)
        );
        assert_eq!(
            verify_init_data("hash=zz", TOKEN, 1, 10),
            Err(InitDataError::Malformed)
        );
        assert_eq!(
            verify_init_data("", TOKEN, 1, 10),
            Err(InitDataError::Malformed)
        );
    }

    #[test]
    fn stale_or_future_auth_date_is_rejected() {
        assert_eq!(
            verify_init_data(&signed("1000"), TOKEN, 1000 + 3601, 3600),
            Err(InitDataError::Expired)
        );
        assert_eq!(
            verify_init_data(&signed("5000"), TOKEN, 1000, 3600),
            Err(InitDataError::Expired)
        );
    }
}
