//! Ограничение частоты опасных действий (подбор приглашений, массовое создание объектов и приглашений).
//! Счётчики живут в памяти процесса: после перезапуска они обнуляются, этого достаточно против перебора вручную.

use std::{collections::HashMap, sync::Mutex};

#[derive(Default)]
pub struct Limiter {
    hits: Mutex<HashMap<(&'static str, i64), Vec<i64>>>,
}

impl Limiter {
    /// Сколько событий `kind` у `key` за последние `window_secs` секунд.
    pub fn count(&self, kind: &'static str, key: i64, window_secs: i64, now: i64) -> usize {
        let mut m = self.hits.lock().unwrap();
        let v = m.entry((kind, key)).or_default();
        v.retain(|t| *t > now - window_secs);
        v.len()
    }

    pub fn record(&self, kind: &'static str, key: i64, now: i64) {
        self.hits
            .lock()
            .unwrap()
            .entry((kind, key))
            .or_default()
            .push(now);
    }

    /// `true`, если лимит не исчерпан (событие при этом не записывается).
    pub fn allows(
        &self,
        kind: &'static str,
        key: i64,
        max: usize,
        window_secs: i64,
        now: i64,
    ) -> bool {
        self.count(kind, key, window_secs, now) < max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_slides_and_keys_are_independent() {
        let l = Limiter::default();
        for t in 0..3 {
            l.record("x", 1, 100 + t);
        }
        assert!(!l.allows("x", 1, 3, 60, 110));
        assert!(l.allows("x", 2, 3, 60, 110));
        assert!(l.allows("y", 1, 3, 60, 110));
        assert!(l.allows("x", 1, 3, 60, 170));
    }
}
