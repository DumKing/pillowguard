use crate::region::{self, RegionLike};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pose {
    Unknown,
    Lying,
    Upright,
    Multiple,
}

#[derive(Clone, Debug)]
pub struct Person {
    pub bbox: [f32; 4],
    pub score: f32,
    pub points: [[f32; 3]; 17],
}

pub fn in_bed(person: &Person, bed: impl RegionLike) -> bool {
    region::overlap(bed, person.bbox) >= 0.15
}

fn has_person_evidence(p: &Person) -> bool {
    let valid = |v: &&[f32; 3], confidence: f32| {
        v.iter().all(|x| x.is_finite())
            && v[2] >= confidence
            && (0. ..=1.).contains(&v[0])
            && (0. ..=1.).contains(&v[1])
    };
    let head = p.points[..5].iter().filter(|v| valid(v, 0.6)).count();
    let torso = [5, 6, 11, 12]
        .iter()
        .filter(|&&k| valid(&&p.points[k], 0.55))
        .count();
    head >= 2 || torso >= 3
}
/// Keep the primary person at normal sensitivity; apply a higher threshold
/// only to additional bed occupants, after model duplicate suppression.
pub fn monitor_people(
    people: &[Person],
    bed: impl RegionLike,
    single: f32,
    multiple: f32,
) -> Vec<Person> {
    let primary = people
        .iter()
        .enumerate()
        .filter(|(_, p)| p.score.is_finite() && p.score >= single && in_bed(p, &bed))
        .max_by(|(_, a), (_, b)| {
            // Reserve normal sensitivity for a supported weaker occupant; otherwise
            // a clear caregiver could displace a partly covered elder as the primary.
            (has_person_evidence(a) && a.score < multiple.max(single))
                .cmp(&(has_person_evidence(b) && b.score < multiple.max(single)))
                .then_with(|| has_person_evidence(a).cmp(&has_person_evidence(b)))
                .then_with(|| a.score.total_cmp(&b.score))
        })
        .map(|(k, _)| k);
    people
        .iter()
        .enumerate()
        .filter(|(k, p)| {
            p.score.is_finite()
                && p.score >= single
                && (!in_bed(p, &bed) || Some(*k) == primary || p.score >= multiple.max(single))
        })
        .map(|(_, p)| p.clone())
        .collect()
}
pub fn uncertain_people(people: &[Person], bed: impl RegionLike) -> bool {
    let selected: Vec<_> = people.iter().filter(|p| in_bed(p, &bed)).collect();
    selected.len() > 1 && selected.iter().any(|p| !has_person_evidence(p))
}
pub fn classify(people: &[Person], bed: impl RegionLike, threshold: f32) -> (Pose, usize) {
    let selected: Vec<_> = people.iter().filter(|p| in_bed(p, &bed)).collect();
    let count = selected.len();
    if count > 1 {
        return (Pose::Multiple, count);
    }
    let Some(p) = selected.first() else {
        return (Pose::Unknown, 0);
    };
    if [5, 6, 11, 12].iter().any(|&i| p.points[i][2] < 0.45) {
        return (Pose::Unknown, 1);
    }
    let a = [
        (p.points[5][0] + p.points[6][0]) / 2.,
        (p.points[5][1] + p.points[6][1]) / 2.,
    ];
    let b = [
        (p.points[11][0] + p.points[12][0]) / 2.,
        (p.points[11][1] + p.points[12][1]) / 2.,
    ];
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length = dx.hypot(dy);
    if length < 0.06 {
        return (Pose::Unknown, 1);
    }
    let vertical = dy / length;
    (
        if vertical >= threshold {
            Pose::Upright
        } else if vertical.abs() < threshold - 0.15 {
            Pose::Lying
        } else {
            Pose::Unknown
        },
        1,
    )
}

#[derive(Default)]
pub struct Rule {
    lying: Option<f64>,
    upright: Option<f64>,
    pub armed: bool,
    pub last_alarm: Option<f64>,
    last_seen: Option<f64>,
    unknown_since: Option<f64>,
    unknown_ready: bool,
    latched: bool,
}
impl Rule {
    pub fn reset_baseline(&mut self) {
        self.lying = None;
        self.upright = None;
        self.armed = false;
        self.last_seen = None;
        self.unknown_since = None;
        self.unknown_ready = false;
        self.latched = false;
    }
    #[cfg(test)]
    pub fn update(&mut self, pose: Pose, now: f64, hold: f64, cooldown: f64) -> bool {
        self.update_assisted(pose, now, hold, cooldown, false)
    }
    #[cfg(test)]
    pub fn update_assisted(
        &mut self,
        pose: Pose,
        now: f64,
        hold: f64,
        cooldown: f64,
        assisted: bool,
    ) -> bool {
        self.update_policy(pose, now, hold, cooldown, assisted, false)
    }
    pub fn update_policy(
        &mut self,
        pose: Pose,
        now: f64,
        hold: f64,
        cooldown: f64,
        assisted: bool,
        from_unknown: bool,
    ) -> bool {
        if self.last_seen.is_some_and(|t| now - t > 3. || now < t) {
            self.reset_baseline();
        }
        self.last_seen = Some(now);
        match pose {
            Pose::Lying => {
                self.unknown_ready = false;
                self.unknown_since = None;
                self.upright = None;
                let t = *self.lying.get_or_insert(now);
                if now - t >= 2. {
                    self.armed = true;
                    self.latched = false;
                }
            }
            Pose::Upright => {
                self.unknown_since = None;
                self.lying = None;
                let t = *self.upright.get_or_insert(now);
                if (self.armed || assisted || (from_unknown && self.unknown_ready))
                    && !self.latched
                    && now - t >= hold
                    && self.last_alarm.is_none_or(|a| now - a >= cooldown)
                {
                    self.armed = false;
                    self.unknown_ready = false;
                    self.latched = true;
                    self.last_alarm = Some(now);
                    return true;
                }
            }
            Pose::Unknown => {
                self.lying = None;
                self.upright = None; // never count hidden time toward the sitting hold
                let start = *self.unknown_since.get_or_insert(now);
                if from_unknown && now - start >= 2. {
                    self.unknown_ready = true;
                    self.latched = false;
                }
                if now - start > 120. {
                    self.armed = false;
                }
                // Default: only confirmed lying rearms. Opt-in: stable unknown also rearms.
            }
            Pose::Multiple => self.reset_baseline(),
        }
        false
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: String,
    pub end: String,
}
pub fn in_windows(mode: &str, windows: &[TimeWindow], clock: &str) -> bool {
    mode == "all_day"
        || windows
            .iter()
            .any(|w| in_window("custom", &w.start, &w.end, clock))
}
pub fn in_window(mode: &str, start: &str, end: &str, clock: &str) -> bool {
    mode == "all_day"
        || if start < end {
            start <= clock && clock < end
        } else {
            clock >= start || clock < end
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn person(x: f32) -> Person {
        let mut p = Person {
            bbox: [x, 0.2, x + 0.15, 0.9],
            score: 0.9,
            points: [[0.; 3]; 17],
        };
        for i in [5, 6] {
            p.points[i] = [x + 0.07, 0.3, 0.9];
        }
        for i in [11, 12] {
            p.points[i] = [x + 0.07, 0.7, 0.9];
        }
        p
    }
    #[test]
    fn polygon_excludes_a_person_in_its_empty_bounding_corner() {
        let bed =
            crate::region::Region::Polygon(vec![[0.4, 0.1], [0.6, 0.1], [0.9, 0.9], [0.1, 0.9]]);
        let elder = person(0.45);
        let mut outside = person(0.1);
        outside.bbox = [0.1, 0.1, 0.25, 0.3];
        assert!(!in_bed(&outside, &bed));
        let filtered = monitor_people(&[elder, outside], &bed, 0.5, 0.75);
        assert_eq!(classify(&filtered, &bed, 0.65), (Pose::Upright, 1));
        assert!(!uncertain_people(&filtered, &bed));
    }
    #[test]
    fn count_people_before_keypoint_validation() {
        let a = person(0.3);
        let mut b = person(0.5);
        b.points = [[0.; 3]; 17];
        assert_eq!(
            classify(&[a, b], [0.1, 0.1, 0.8, 0.8], 0.65),
            (Pose::Multiple, 2)
        );
    }
    #[test]
    fn occluded_extra_box_is_uncertain_but_still_pauses() {
        let a = person(0.2);
        let mut b = person(0.45);
        b.points = [[0.; 3]; 17];
        let bed = [0.1, 0.1, 0.8, 0.8];
        assert!(uncertain_people(&[a.clone(), b.clone()], bed));
        assert_eq!(classify(&[a, b], bed, 0.65).0, Pose::Multiple);
        assert!(!uncertain_people(&[person(0.2), person(0.45)], bed));
    }
    #[test]
    fn higher_threshold_only_filters_additional_people() {
        let bed = [0.1, 0.1, 0.8, 0.8];
        let mut elder = person(0.2);
        elder.score = 0.55;
        let mut object = person(0.45);
        object.score = 0.65;
        object.points = [[0.; 3]; 17];
        let filtered = monitor_people(&[elder.clone(), object.clone()], bed, 0.5, 0.75);
        assert_eq!(classify(&filtered, bed, 0.65), (Pose::Upright, 1));
        assert_eq!(filtered[0].score, 0.55);
        let mut caregiver = person(0.45);
        caregiver.score = 0.85;
        let filtered = monitor_people(&[elder.clone(), caregiver], bed, 0.5, 0.75);
        assert_eq!(classify(&filtered, bed, 0.65), (Pose::Multiple, 2));
        assert!(!uncertain_people(&filtered, bed));
        object.score = 0.9;
        let filtered = monitor_people(&[elder, object], bed, 0.5, 0.75);
        assert_eq!(filtered.len(), 2);
        assert!(uncertain_people(&filtered, bed));
    }
    #[test]
    fn secondary_threshold_is_configurable_and_never_bypasses_single_threshold() {
        let mut a = person(0.2);
        a.score = 0.7;
        let mut b = person(0.45);
        b.score = 0.65;
        let bed = [0.1, 0.1, 0.8, 0.8];
        assert_eq!(
            monitor_people(&[a.clone(), b.clone()], bed, 0.5, 0.6).len(),
            2
        );
        assert_eq!(
            monitor_people(&[a.clone(), b.clone()], bed, 0.5, 0.8).len(),
            1
        );
        assert_eq!(monitor_people(&[a.clone(), b], bed, 0.68, 0.6).len(), 1);
        assert_eq!(monitor_people(&[a], bed, 0.8, 0.8).len(), 0);
    }
    #[test]
    fn outside_person_does_not_pause() {
        assert_eq!(
            classify(&[person(0.2), person(0.8)], [0.1, 0.1, 0.4, 0.8], 0.65),
            (Pose::Upright, 1)
        );
    }
    #[test]
    fn multiple_clears_old_baseline() {
        let mut r = Rule::default();
        r.update(Pose::Lying, 0., 2., 60.);
        r.update(Pose::Lying, 2., 2., 60.);
        assert!(r.armed);
        r.update(Pose::Multiple, 3., 2., 60.);
        assert!(!r.armed);
        assert!(!r.update(Pose::Upright, 4., 2., 60.));
        assert!(!r.update(Pose::Upright, 6., 2., 60.));
        r.update(Pose::Lying, 7., 2., 60.);
        r.update(Pose::Lying, 9., 2., 60.);
        r.update(Pose::Upright, 10., 2., 60.);
        assert!(r.update(Pose::Upright, 12., 2., 60.));
    }
    #[test]
    fn cooldown_and_occlusion() {
        let mut r = Rule::default();
        for t in [0., 1., 2.] {
            r.update(Pose::Lying, t, 1., 60.);
        }
        r.update(Pose::Upright, 3., 1., 60.);
        assert!(r.update(Pose::Upright, 4., 1., 60.));
        r.update(Pose::Unknown, 5., 1., 60.);
        assert!(!r.update(Pose::Upright, 6., 1., 60.));
        for t in [7., 8., 9.] {
            r.update(Pose::Lying, t, 1., 60.);
        }
        r.update(Pose::Upright, 10., 1., 60.);
        assert!(!r.update(Pose::Upright, 11., 1., 60.));
    }
    #[test]
    fn schedule_boundaries() {
        for (t, e) in [
            ("21:59", false),
            ("22:00", true),
            ("00:00", true),
            ("06:59", true),
            ("07:00", false),
        ] {
            assert_eq!(in_window("custom", "22:00", "07:00", t), e);
        }
        assert!(in_window("all_day", "22:00", "07:00", "12:00"));
    }
}

/// Tracks inability to judge, independently of the last confirmed posture.
#[derive(Default)]
pub struct Watchdog {
    lost: Option<f64>,
    sent: Option<f64>,
    recovered: Option<f64>,
}
impl Watchdog {
    pub fn update(&mut self, unknown: bool, now: f64, delay: f64) -> (bool, bool) {
        if unknown {
            self.recovered = None;
            let start = *self.lost.get_or_insert(now);
            if now - start >= delay && self.sent.is_none_or(|t| now - t >= 300.) {
                self.sent = Some(now);
                return (true, false);
            }
        } else {
            let start = *self.recovered.get_or_insert(now);
            if now - start >= 2. {
                let recovery = self.sent.is_some();
                self.lost = None;
                self.sent = None;
                return (false, recovery);
            }
        }
        (false, false)
    }
}
#[cfg(test)]
mod night_tests {
    use super::*;
    #[test]
    fn short_cover_retains_baseline_but_never_counts_hidden_time() {
        let mut r = Rule::default();
        for t in 0..=2 {
            r.update(Pose::Lying, t as f64, 2., 5.);
        }
        for t in 3..=20 {
            r.update(Pose::Unknown, t as f64, 2., 5.);
        }
        assert!(r.armed);
        assert!(!r.update(Pose::Upright, 21., 2., 5.));
        assert!(r.update(Pose::Upright, 23., 2., 5.));
        for t in 24..=35 {
            r.update(Pose::Unknown, t as f64, 2., 5.);
        }
        assert!(!r.update_assisted(Pose::Upright, 36., 2., 5., true));
        assert!(!r.update_assisted(Pose::Upright, 38., 2., 5., true));
        r.update(Pose::Lying, 39., 2., 5.);
        r.update(Pose::Lying, 41., 2., 5.);
        r.update(Pose::Upright, 42., 2., 5.);
        assert!(r.update(Pose::Upright, 44., 2., 5.));
    }
    #[test]
    fn direct_rise_needs_calibration_and_multi_still_resets() {
        let mut r = Rule::default();
        r.update(Pose::Upright, 0., 2., 5.);
        assert!(!r.update(Pose::Upright, 2., 2., 5.));
        assert!(r.update_assisted(Pose::Upright, 3., 2., 5., true));
        r.update(Pose::Multiple, 4., 2., 5.);
        assert!(!r.armed);
        assert!(!r.update_assisted(Pose::Multiple, 6., 2., 5., true));
    }
    #[test]
    fn watchdog_repeats_and_requires_stable_recovery() {
        let mut w = Watchdog::default();
        assert_eq!(w.update(true, 0., 30.), (false, false));
        assert_eq!(w.update(true, 30., 30.), (true, false));
        assert_eq!(w.update(true, 31., 30.), (false, false));
        w.update(false, 32., 30.);
        w.update(true, 33., 30.);
        assert_eq!(w.update(true, 330., 30.), (true, false));
        w.update(false, 331., 30.);
        assert_eq!(w.update(false, 333., 30.), (false, true));
        assert_eq!(w.update(true, 334., 30.), (false, false));
    }
}

#[cfg(test)]
mod expiry_test {
    use super::*;
    #[test]
    fn prolonged_occlusion_expires_baseline_but_not_sitting_latch() {
        let mut r = Rule::default();
        r.update(Pose::Lying, 0., 2., 5.);
        r.update(Pose::Lying, 2., 2., 5.);
        for t in 3..=125 {
            r.update(Pose::Unknown, t as f64, 2., 5.);
        }
        assert!(!r.armed);
        r.update(Pose::Upright, 126., 2., 5.);
        assert!(!r.update(Pose::Upright, 128., 2., 5.));
    }
}

#[cfg(test)]
mod unknown_option_tests {
    use super::*;
    #[test]
    fn opt_in_unknown_to_upright_and_stable_rearming() {
        let mut r = Rule::default();
        for t in 0..=2 {
            assert!(!r.update_policy(Pose::Unknown, t as f64, 2., 5., false, true));
        }
        assert!(!r.update_policy(Pose::Upright, 3., 2., 5., false, true));
        assert!(r.update_policy(Pose::Upright, 5., 2., 5., false, true));
        for t in 6..=12 {
            assert!(!r.update_policy(Pose::Upright, t as f64, 2., 5., false, true));
        }
        r.update_policy(Pose::Unknown, 13., 2., 5., false, true);
        r.update_policy(Pose::Upright, 14., 2., 5., false, true);
        assert!(!r.update_policy(Pose::Upright, 16., 2., 5., false, true));
        for t in 17..=19 {
            r.update_policy(Pose::Unknown, t as f64, 2., 5., false, true);
        }
        r.update_policy(Pose::Upright, 20., 2., 5., false, true);
        assert!(r.update_policy(Pose::Upright, 22., 2., 5., false, true));
    }
    #[test]
    fn default_multiple_and_cooldown_are_respected() {
        for enabled in [false, true] {
            let mut r = Rule::default();
            for t in 0..=2 {
                r.update_policy(Pose::Unknown, t as f64, 2., 60., false, enabled);
            }
            r.update_policy(Pose::Upright, 3., 2., 60., false, enabled);
            assert_eq!(
                r.update_policy(Pose::Upright, 5., 2., 60., false, enabled),
                enabled
            );
            for t in 6..=8 {
                r.update_policy(Pose::Unknown, t as f64, 2., 60., false, enabled);
            }
            r.update_policy(Pose::Upright, 9., 2., 60., false, enabled);
            assert!(!r.update_policy(Pose::Upright, 11., 2., 60., false, enabled));
            r.update_policy(Pose::Multiple, 12., 2., 60., false, enabled);
            r.update_policy(Pose::Upright, 13., 2., 60., false, enabled);
            assert!(!r.update_policy(Pose::Upright, 15., 2., 60., false, enabled));
        }
    }
}

#[cfg(test)]
mod multi_window_tests {
    use super::*;
    #[test]
    fn union_midnight_overlap_and_boundaries() {
        let w = vec![
            TimeWindow {
                start: "13:00".into(),
                end: "14:00".into(),
            },
            TimeWindow {
                start: "22:00".into(),
                end: "07:00".into(),
            },
            TimeWindow {
                start: "06:30".into(),
                end: "08:00".into(),
            },
        ];
        for (clock, expected) in [
            ("00:00", true),
            ("06:30", true),
            ("07:00", true),
            ("07:59", true),
            ("08:00", false),
            ("12:59", false),
            ("13:00", true),
            ("13:59", true),
            ("14:00", false),
            ("21:59", false),
            ("22:00", true),
            ("23:59", true),
        ] {
            assert_eq!(in_windows("custom", &w, clock), expected, "{clock}");
        }
        assert!(in_windows("all_day", &[], "12:00"));
        assert!(!in_windows("custom", &[], "12:00"));
    }
}
