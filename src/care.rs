use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Rising,
    Sitting,
    Edge,
    Left,
    Fall,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rising => "开始坐起",
            Self::Sitting => "已经坐起",
            Self::Edge => "准备下床",
            Self::Left => "已经离床",
            Self::Fall => "疑似跌倒",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Rising => "rising",
            Self::Sitting => "sitting",
            Self::Edge => "edge",
            Self::Left => "left",
            Self::Fall => "fall",
        }
    }
    pub fn level(self) -> u8 {
        match self {
            Self::Rising => 0,
            Self::Sitting => 1,
            Self::Edge => 2,
            Self::Left => 3,
            Self::Fall => 4,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub confirmation: bool,
    pub rising: bool,
    pub edge: bool,
    pub left: bool,
    pub fall: bool,
    pub rising_text: String,
    pub edge_text: String,
    pub left_text: String,
    pub fall_text: String,
    pub edge_region: Option<crate::region::Region>,
    pub floor_region: Option<crate::region::Region>,
    pub repeat: bool,
    pub repeat_secs: f64,
    pub repeat_count: u32,
    pub escalate: bool,
    pub escalate_secs: f64,
    pub escalation_channels: Vec<String>, // Legacy destination selection, no longer used.
    pub escalate_plays: u32,
    pub escalate_gap: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            confirmation: false,
            rising: false,
            edge: false,
            left: false,
            fall: false,
            rising_text: "注意，老人正在起身".into(),
            edge_text: "紧急提醒，老人准备下床".into(),
            left_text: "警告，老人已经离开床铺".into(),
            fall_text: "紧急警报，疑似跌倒，请立即检查老人情况".into(),
            edge_region: None,
            floor_region: None,
            repeat: false,
            repeat_secs: 60.,
            repeat_count: 3,
            escalate: false,
            escalate_secs: 120.,
            escalation_channels: vec![],
            escalate_plays: 3,
            escalate_gap: 3.,
        }
    }
}
impl Settings {
    pub fn focus_sitting(&mut self) {
        self.escalation_channels.clear();
        self.rising = false;
        self.edge = false;
        self.left = false;
        self.fall = false;
    }
    pub fn enabled(&self, k: Kind) -> bool {
        k == Kind::Sitting
    }
    pub fn template<'a>(&'a self, k: Kind, sitting: &'a str) -> &'a str {
        match k {
            Kind::Rising => &self.rising_text,
            Kind::Sitting => sitting,
            Kind::Edge => &self.edge_text,
            Kind::Left => &self.left_text,
            Kind::Fall => &self.fall_text,
        }
    }
    pub fn validate(&self, _bed: impl crate::region::RegionLike) -> Result<()> {
        if !self.repeat_secs.is_finite()
            || !(5. ..=3600.).contains(&self.repeat_secs)
            || !(1..=100).contains(&self.repeat_count)
            || !self.escalate_secs.is_finite()
            || !(5. ..=3600.).contains(&self.escalate_secs)
            || !(1..=10).contains(&self.escalate_plays)
            || !self.escalate_gap.is_finite()
            || !(1. ..=60.).contains(&self.escalate_gap)
        {
            bail!("重复或升级时间无效");
        }
        if self.escalation_channels.len() > 32
            || self
                .escalation_channels
                .iter()
                .any(|s| s.is_empty() || s.len() > 80)
        {
            bail!("升级通知渠道无效");
        }
        for region in [&self.edge_region, &self.floor_region]
            .into_iter()
            .flatten()
        {
            region.validate()?;
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    pub kind: Kind,
    pub message: String,
    pub created: f64,
    pub last_sent: f64,
    pub sends: u32,
    pub acknowledged: bool,
    pub resolved: bool,
    pub escalated: bool,
    pub demo: bool,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Book {
    next: u64,
    pub alerts: Vec<Alert>,
}
impl Book {
    pub fn create(&mut self, kind: Kind, message: String, now: f64, demo: bool) -> String {
        for a in &mut self.alerts {
            if !a.acknowledged && !a.resolved && a.demo == demo && a.kind.level() <= kind.level() {
                a.resolved = true;
            }
        }
        self.next = self.next.wrapping_add(1);
        let id = format!("{}-{}", (now * 1000.) as u64, self.next);
        self.alerts.insert(
            0,
            Alert {
                id: id.clone(),
                kind,
                message,
                created: now,
                last_sent: now,
                sends: 1,
                acknowledged: false,
                resolved: false,
                escalated: false,
                demo,
            },
        );
        self.alerts.truncate(100);
        id
    }
    pub fn active(&self, id: &str) -> bool {
        self.alerts
            .iter()
            .any(|a| a.id == id && !a.acknowledged && !a.resolved)
    }
    pub fn acknowledge(&mut self, id: &str) -> bool {
        if let Some(a) = self.alerts.iter_mut().find(|a| a.id == id) {
            a.acknowledged = true;
            true
        } else {
            false
        }
    }
    pub fn resolve(&mut self) {
        for a in &mut self.alerts {
            a.resolved = true;
        }
    }
    pub fn due(
        &mut self,
        settings: &Settings,
        now: f64,
        demo: bool,
    ) -> Vec<(String, String, bool, Kind)> {
        let mut out = vec![];
        for a in &mut self.alerts {
            if a.demo != demo
                || a.acknowledged
                || a.resolved
                || !settings.enabled(a.kind)
                || now - a.created > 86400.
            {
                continue;
            }
            let escalation =
                settings.escalate && !a.escalated && now - a.created >= settings.escalate_secs;
            let repeat = settings.repeat
                && a.sends < settings.repeat_count
                && now - a.last_sent >= settings.repeat_secs;
            if escalation || repeat {
                a.last_sent = now;
                if escalation {
                    a.escalated = true;
                } else {
                    a.sends += 1;
                }
                out.push((a.id.clone(), a.message.clone(), escalation, a.kind));
            }
        }
        out
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acknowledgement_dedup_repeat_escalation_and_recovery() {
        let mut b = Book::default();
        let mut s = Settings::default();
        s.repeat = true;
        s.repeat_secs = 5.;
        s.repeat_count = 2;
        s.escalate = true;
        s.escalate_secs = 10.;
        s.escalation_channels = vec!["family".into()];
        let id = b.create(Kind::Sitting, "坐起".into(), 0., false);
        assert!(b.due(&s, 4., false).is_empty());
        assert_eq!(b.due(&s, 5., false).len(), 1);
        assert!(b.due(&s, 9., false).is_empty());
        assert!(b.due(&s, 10., false)[0].2);
        assert!(b.due(&s, 20., false).is_empty());
        b.acknowledge(&id);
        assert!(!b.active(&id));
        assert!(b.due(&s, 30., false).is_empty());
        let id = b.create(Kind::Sitting, "坐起".into(), 31., false);
        b.resolve();
        assert!(!b.active(&id));
    }
    #[test]
    fn stages_replace_lower_severity_and_demo_is_separate() {
        let mut b = Book::default();
        let a = b.create(Kind::Rising, "开始".into(), 0., false);
        b.create(Kind::Sitting, "坐起".into(), 1., false);
        assert!(!b.active(&a));
        let mut s = Settings::default();
        s.repeat = true;
        s.repeat_secs = 5.;
        assert!(b.due(&s, 10., true).is_empty());
    }
}

#[cfg(test)]
mod focused_tests {
    use super::*;
    #[test]
    fn only_sitting_can_notify_even_with_legacy_settings() {
        let mut s = Settings::default();
        s.rising = true;
        s.edge = true;
        s.left = true;
        s.fall = true;
        for kind in [Kind::Rising, Kind::Edge, Kind::Left, Kind::Fall] {
            assert!(!s.enabled(kind));
        }
        assert!(s.enabled(Kind::Sitting));
        s.focus_sitting();
        assert!(!s.rising && !s.edge && !s.left && !s.fall);
        assert!(s.edge_region.is_none());
    }
    #[test]
    fn record_ids_survive_restart() {
        let mut b = Book::default();
        let id = b.create(Kind::Sitting, "坐起".into(), 1., false);
        let mut b: Book = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_ne!(id, b.create(Kind::Sitting, "坐起".into(), 1., false));
    }
}

#[cfg(test)]
mod burst_settings_tests {
    use super::*;
    #[test]
    fn migrated_defaults_and_burst_limits() {
        let mut s: Settings = serde_json::from_str(r#"{"escalate":true}"#).unwrap();
        assert_eq!(s.escalate_plays, 3);
        assert_eq!(s.escalate_gap, 3.);
        assert!(s.validate([0., 0., 1., 1.]).is_ok());
        s.escalate_plays = 0;
        assert!(s.validate([0., 0., 1., 1.]).is_err());
        s.escalate_plays = 11;
        assert!(s.validate([0., 0., 1., 1.]).is_err());
        s.escalate_plays = 3;
        s.escalate_gap = f64::NAN;
        assert!(s.validate([0., 0., 1., 1.]).is_err());
        s.escalate_gap = 0.;
        assert!(s.validate([0., 0., 1., 1.]).is_err());
    }
}
