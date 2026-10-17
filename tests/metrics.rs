//! Метрики беты считают воронку первого цикла и не содержат персональных данных.
mod common;
use axum::http::Method;
use common::*;

#[tokio::test]
async fn the_funnel_follows_the_first_billing_cycle() {
    let w = World::new().await;
    let o = w.owner(OWNER_A).await;
    let m = w.app.metrics(now()).await.unwrap();
    assert_eq!((m.owners, m.funnel_with_property), (0, 0));

    let p = w.create_property(&o, "Кв").await;
    let m = w.app.metrics(now()).await.unwrap();
    assert_eq!(
        (m.properties, m.funnel_with_property, m.funnel_with_tenant),
        (1, 1, 0)
    );

    let t = w.join_tenant(&o, p, TENANT_A).await;
    let m = w.app.metrics(now()).await.unwrap();
    assert_eq!(
        (
            m.invites_created,
            m.invites_used,
            m.tenants_joined,
            m.funnel_with_tenant
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(m.funnel_with_approved_bill, 0);

    let bill = w.bill(&o, &t, p, ["2", "2", "10", "10"]).await;
    let (s, _) = w
        .call(
            Method::POST,
            &format!("/api/bills/{bill}/approve"),
            Some(&o),
            Some(p),
            None,
        )
        .await;
    assert_eq!(s, axum::http::StatusCode::OK);
    let m = w.app.metrics(now()).await.unwrap();
    assert_eq!(
        (
            m.bills_approved_or_paid,
            m.bills_paid,
            m.funnel_with_approved_bill
        ),
        (1, 0, 1)
    );
    assert_eq!(m.active_7d, 2, "владелец и арендатор действовали через API");
}
