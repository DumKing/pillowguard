use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub start: String,
    pub end: String,
    pub count: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playback {
    pub default_count: u32,
    pub windows: Vec<Window>,
}
impl Default for Playback {
    fn default() -> Self {
        Self {
            default_count: 1,
            windows: vec![],
        }
    }
}
impl Playback {
    pub fn validate(&self) -> Result<()> {
        if !(1..=10).contains(&self.default_count) || self.windows.len() > 16 {
            bail!("音箱播报次数应为 1～10 次，最多设置 16 个时间区间");
        }
        for w in &self.windows {
            if !clock_valid(&w.start)
                || !clock_valid(&w.end)
                || w.start == w.end
                || !(1..=10).contains(&w.count)
            {
                bail!("音箱时间区间需填写不同的开始与结束时间，播报次数为 1～10 次");
            }
        }
        Ok(())
    }
    pub fn alert_count(&self, clock: &str, automatic: bool, escalation: bool) -> u32 {
        if automatic && !escalation {
            self.count_at(clock)
        } else {
            1
        }
    }
    pub fn count_at(&self, clock: &str) -> u32 {
        self.windows
            .iter()
            .find(|w| crate::rules::in_window("custom", &w.start, &w.end, clock))
            .map(|w| w.count)
            .unwrap_or(self.default_count)
            .clamp(1, 10)
    }
}
fn clock_valid(clock: &str) -> bool {
    crate::valid_time(clock)
        && clock
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 2 || b.is_ascii_digit())
}
pub fn pause_seconds(text: &str) -> f64 {
    // HA acknowledges the request but does not report speech completion.
    (text.chars().count().min(255) + 25) as f64 / 4. + 3.
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_and_cross_midnight_with_first_match_priority() {
        let p = Playback {
            default_count: 2,
            windows: vec![
                Window {
                    start: "22:00".into(),
                    end: "07:00".into(),
                    count: 4,
                },
                Window {
                    start: "06:00".into(),
                    end: "09:00".into(),
                    count: 3,
                },
            ],
        };
        p.validate().unwrap();
        for (time, count) in [
            ("21:59", 2),
            ("22:00", 4),
            ("23:59", 4),
            ("00:00", 4),
            ("06:59", 4),
            ("07:00", 3),
            ("09:00", 2),
        ] {
            assert_eq!(p.count_at(time), count, "{time}");
        }
        assert_eq!(p.alert_count("23:00", true, false), 4);
        assert_eq!(p.alert_count("23:00", true, true), 1);
        assert_eq!(p.alert_count("23:00", false, false), 1);
        let saved = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Playback>(&saved).unwrap(), p);
        assert_eq!(
            serde_json::from_str::<Playback>("{}").unwrap(),
            Playback::default()
        );
    }
    #[test]
    fn rejects_bad_counts_and_times() {
        for count in [0, 11, u32::MAX] {
            assert!(Playback {
                default_count: count,
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        for (start, end, count) in [
            ("24:00", "07:00", 1),
            ("+1:00", "07:00", 1),
            ("7:00", "09:00", 2),
            ("08:00", "08:00", 2),
            ("08:00", "09:00", 0),
            ("08:00", "09:00", 11),
        ] {
            assert!(Playback {
                default_count: 1,
                windows: vec![Window {
                    start: start.into(),
                    end: end.into(),
                    count
                }]
            }
            .validate()
            .is_err());
        }
        assert!(Playback {
            windows: vec![
                Window {
                    start: "01:00".into(),
                    end: "02:00".into(),
                    count: 1
                };
                17
            ],
            ..Default::default()
        }
        .validate()
        .is_err());
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Complete,
    Cancelled,
    Failed,
}
/// The initial request has already been accepted. Additional requests never
/// retry failures and recheck cancellation throughout the speech delay.
pub async fn repeat<V, F, Fut>(
    count: u32,
    pause: std::time::Duration,
    valid: V,
    mut send: F,
) -> Outcome
where
    V: Fn() -> bool,
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for play in 2..=count {
        let deadline = std::time::Instant::now() + pause;
        loop {
            if !valid() {
                return Outcome::Cancelled;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        if !send(play).await {
            return Outcome::Failed;
        }
    }
    Outcome::Complete
}
#[cfg(test)]
mod repeat_tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU32, Ordering},
        time::Duration,
    };
    #[tokio::test]
    async fn repeats_exact_remaining_count_and_stops_on_failure_or_cancel() {
        let sent = AtomicU32::new(0);
        assert_eq!(
            repeat(
                4,
                Duration::ZERO,
                || true,
                |_| async {
                    sent.fetch_add(1, Ordering::SeqCst);
                    true
                }
            )
            .await,
            Outcome::Complete
        );
        assert_eq!(sent.load(Ordering::SeqCst), 3);
        sent.store(0, Ordering::SeqCst);
        assert_eq!(
            repeat(
                4,
                Duration::ZERO,
                || true,
                |_| async {
                    sent.fetch_add(1, Ordering::SeqCst);
                    false
                }
            )
            .await,
            Outcome::Failed
        );
        assert_eq!(sent.load(Ordering::SeqCst), 1);
        sent.store(0, Ordering::SeqCst);
        assert_eq!(
            repeat(
                4,
                Duration::ZERO,
                || sent.load(Ordering::SeqCst) == 0,
                |_| async {
                    sent.fetch_add(1, Ordering::SeqCst);
                    true
                }
            )
            .await,
            Outcome::Cancelled
        );
        assert_eq!(sent.load(Ordering::SeqCst), 1);
        assert_eq!(
            repeat(
                4,
                Duration::from_secs(30),
                || false,
                |_| async { panic!("cancelled batch must not send") }
            )
            .await,
            Outcome::Cancelled
        );
    }
}
