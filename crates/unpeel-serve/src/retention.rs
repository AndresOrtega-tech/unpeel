//! Stopped-Session retention: the sidebar's stopped preview
//! (`sidebar_stopped_limit`, Settings ▸ Advanced ▸ Cleanup, default five per
//! group) is also the keep limit. Stopped rows past it — never pinned or
//! unread ones — are removed by this Host-owned sweep, so the sidebar is the
//! whole list and stopped Sessions never pile up on disk. The provider's own
//! history still holds each conversation.
//!
//! One batch is in flight at a time, each removal re-checks under the
//! Session's lifecycle lock that it is still stopped (a Resume wins), and a
//! failed removal backs off for a minute. A row left alone — resumed, or
//! holding artifacts or Pi storage the sweep never deletes — is re-checked
//! every ten minutes rather than every tick.

use std::collections::HashMap;
use std::sync::mpsc;

const RETRY_DELAY_MS: u64 = 60_000;
const KEPT_RECHECK_MS: u64 = 600_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetentionEvent {
    Removed(String),
    Failed { session_id: String, error: String },
}

type Outcome = (String, Result<bool, String>);

#[derive(Default)]
pub struct Retention {
    in_flight: Option<mpsc::Receiver<Vec<Outcome>>>,
    retry_after_ms: HashMap<String, u64>,
}

impl Retention {
    /// One sweep over the model's expired rows (already past the keep limit).
    pub fn step(&mut self, expired: &[String], now_ms: u64) -> Vec<RetentionEvent> {
        let mut events = Vec::new();
        if let Some(outcomes) = self.in_flight.as_ref() {
            match outcomes.try_recv() {
                Ok(outcomes) => {
                    for (session_id, result) in outcomes {
                        match result {
                            Ok(true) => events.push(RetentionEvent::Removed(session_id)),
                            Ok(false) => {
                                self.retry_after_ms
                                    .insert(session_id, now_ms + KEPT_RECHECK_MS);
                            }
                            Err(error) => {
                                self.retry_after_ms
                                    .insert(session_id.clone(), now_ms + RETRY_DELAY_MS);
                                events.push(RetentionEvent::Failed { session_id, error });
                            }
                        }
                    }
                    self.in_flight = None;
                }
                Err(mpsc::TryRecvError::Empty) => return events,
                Err(mpsc::TryRecvError::Disconnected) => self.in_flight = None,
            }
        }
        self.retry_after_ms
            .retain(|id, _| expired.iter().any(|expired_id| expired_id == id));
        let due = due(expired, &self.retry_after_ms, now_ms);
        if due.is_empty() {
            return events;
        }
        let (outcomes_tx, outcomes_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("unpeel-retention".into())
            .spawn(move || {
                let outcomes = due
                    .into_iter()
                    .map(|id| {
                        let result = unpeel_core::session_ops::remove_stopped_session(&id);
                        (id, result)
                    })
                    .collect();
                let _ = outcomes_tx.send(outcomes);
            });
        if spawned.is_ok() {
            self.in_flight = Some(outcomes_rx);
        }
        events
    }
}

fn due(expired: &[String], retry_after_ms: &HashMap<String, u64>, now_ms: u64) -> Vec<String> {
    expired
        .iter()
        .filter(|id| {
            retry_after_ms
                .get(*id)
                .is_none_or(|retry_at| now_ms >= *retry_at)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_removal_waits_out_its_backoff() {
        let expired = vec!["a".to_string(), "b".to_string()];
        let retry = HashMap::from([("a".to_string(), 10_000)]);
        assert_eq!(due(&expired, &retry, 9_999), ["b"]);
        assert_eq!(due(&expired, &retry, 10_000), ["a", "b"]);
    }

    #[test]
    fn nothing_expired_spawns_nothing() {
        let mut retention = Retention::default();
        assert!(retention.step(&[], 0).is_empty());
        assert!(retention.in_flight.is_none());
    }
}
