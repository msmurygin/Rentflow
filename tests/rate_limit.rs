//! Перебор приглашений и массовое создание упираются в 429; чужие пользователи лимитом не задеваются.
mod common;
use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

async fn accept(w: &World, tok: &str, invite: &str) -> StatusCode {
    w.call(
        Method::POST,
        "/api/invitations/accept",
        Some(tok),
        None,
        Some(json!({ "token": invite })),
    )
    .await
    .0
}

#[tokio::test]
async fn guessing_invitations_is_stopped_per_user() {
    let w = World::new().await;
    let guesser = w.login(TENANT_A).await;
    for i in 0..5 {
        let s = accept(&w, &guesser, &format!("guess{i}")).await;
        assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "попытка {i}");
        assert_ne!(s, StatusCode::OK);
    }
    assert_eq!(
        accept(&w, &guesser, "guess6").await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // настоящее приглашение тому, кого заблокировали, тоже не проходит в окне; другой пользователь не задет
    let owner = w.owner(OWNER_A).await;
    let prop = w.create_property(&owner, "Квартира").await;
    let (_, v) = w
        .call(
            Method::POST,
            "/api/invitations",
            Some(&owner),
            Some(prop),
            Some(json!({})),
        )
        .await;
    let invite = v["url"].as_str().unwrap().split("start=").nth(1).unwrap();
    assert_eq!(
        accept(&w, &guesser, invite).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let other = w.login(TENANT_B).await;
    assert_eq!(accept(&w, &other, invite).await, StatusCode::OK);
}

#[tokio::test]
async fn invitation_creation_is_capped_per_hour() {
    let w = World::new().await;
    let owner = w.owner(OWNER_A).await;
    let prop = w.create_property(&owner, "Квартира").await;
    for _ in 0..20 {
        let (s, _) = w
            .call(
                Method::POST,
                "/api/invitations",
                Some(&owner),
                Some(prop),
                Some(json!({})),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, _) = w
        .call(
            Method::POST,
            "/api/invitations",
            Some(&owner),
            Some(prop),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    // другой владелец не задет
    let b = w.owner(OWNER_B).await;
    let pb = w.create_property(&b, "Другая").await;
    let (s, _) = w
        .call(
            Method::POST,
            "/api/invitations",
            Some(&b),
            Some(pb),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}
