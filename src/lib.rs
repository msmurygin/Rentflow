//! RentFlow: Telegram-бот для расчёта коммунальных платежей между арендатором и владельцем.

pub mod api;
pub mod app;
pub mod backup;
pub mod billing;
pub mod bills;
pub mod bot;
pub mod db;
pub mod demo;
pub mod metrics;
pub mod ocr;
pub mod ops;
pub mod readings;
pub mod reissue;
pub mod reminders;
pub mod setup;
pub mod tariffs;
pub mod util;
pub mod void;
