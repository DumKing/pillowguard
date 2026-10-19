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

#[derive(Clone, Debug, Serialize)]
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
    (torso_pose(p, threshold), 1)
}

fn valid_joint(p: &[f32; 3], confidence: f32) -> bool {
    p.iter().all(|x| x.is_finite())
        && p[2] >= confidence
        && (0. ..=1.).contains(&p[0])
        && (0. ..=1.).contains(&p[1])
}
fn torso_pose(p: &Person, threshold: f32) -> Pose {
    // Measure directions in image space, not the unequal normalized x/y axes.
    let vector = |a: [f32; 2], b: [f32; 2]| [(b[0] - a[0]) * 16. / 9., b[1] - a[1]];
    let length = |v: [f32; 2]| v[0].hypot(v[1]);
    let classify = |v: [f32; 2]| {
        let n = length(v);
        if n < 0.06 {
            return Pose::Unknown;
        }
        let vertical = v[1] / n;
        if vertical >= threshold {
            Pose::Upright
        } else if vertical.abs() < threshold - 0.15 {
            Pose::Lying
        } else {
            Pose::Unknown
        }
    };
    let point = |k: usize| [p.points[k][0], p.points[k][1]];
    let sides: Vec<_> = [(5, 11), (6, 12)]
        .into_iter()
        .filter(|&(a, b)| valid_joint(&p.points[a], 0.45) && valid_joint(&p.points[b], 0.45))
        .map(|(a, b)| (a, b, vector(point(a), point(b))))
        .collect();
    if sides.len() == 2 {
        let (a, b) = (sides[0].2, sides[1].2);
        let (na, nb) = (length(a), length(b));
        if na < 0.06
            || nb < 0.06
            || na.max(nb) > na.min(nb) * 3.
            || (a[0] * b[0] + a[1] * b[1]) / (na * nb) < 0.75
        {
            return Pose::Unknown;
        }
        let v = [(a[0] + b[0]) / 2., (a[1] + b[1]) / 2.];
        let pose = classify(v);
        // A blanket lifted by knees can produce two convincing but invented
        // shoulder/hip pairs. Upright always needs a visible, aligned head.
        if pose != Pose::Upright {
            return pose;
        }
        let shoulder = [
            (p.points[5][0] + p.points[6][0]) / 2.,
            (p.points[5][1] + p.points[6][1]) / 2.,
        ];
        return if aligned_head(p, shoulder, v) {
            Pose::Upright
        } else {
            Pose::Unknown
        };
    }
    // An occluded side is allowed only with a strong complete side AND visible
    // head evidence pointing in the same body direction. Never invent hidden hips.
    let Some(&(shoulder, hip, v)) = sides.first() else {
        return Pose::Unknown;
    };
    if !valid_joint(&p.points[shoulder], 0.6)
        || !valid_joint(&p.points[hip], 0.6)
        || classify(v) != Pose::Upright
    {
        return Pose::Unknown;
    }
    if aligned_head(p, point(shoulder), v) {
        Pose::Upright
    } else {
        Pose::Unknown
    }
}
fn aligned_head(p: &Person, shoulder: [f32; 2], v: [f32; 2]) -> bool {
    let head: Vec<_> = p.points[..5]
        .iter()
        .filter(|q| valid_joint(q, 0.6))
        .collect();
    if head.len() < 2 {
        return false;
    }
    let n = head.len() as f32;
    let h = [
        head.iter().map(|q| q[0]).sum::<f32>() / n,
        head.iter().map(|q| q[1]).sum::<f32>() / n,
    ];
    let hs = [(shoulder[0] - h[0]) * 16. / 9., shoulder[1] - h[1]];
    let torso = v[0].hypot(v[1]);
    let projection = (hs[0] * v[0] + hs[1] * v[1]) / torso;
    let lateral = (hs[0] * v[1] - hs[1] * v[0]).abs() / torso;
    projection >= 0.02 && projection <= torso * 1.5 && lateral <= torso * 0.35 + 0.02
}

/// Independent strong head evidence may veto a false torso on a raised quilt.
/// It never confirms lying or creates a sitting event by itself.
pub fn head_contradicts_sitting(p: &Person, bbox: [f32; 4]) -> bool {
    let head: Vec<_> = p.points[..5]
        .iter()
        .filter(|q| valid_joint(q, 0.6))
        .collect();
    if head.len() < 2 {
        return false;
    }
    let n = head.len() as f32;
    let center = [
        head.iter().map(|q| q[0]).sum::<f32>() / n,
        head.iter().map(|q| q[1]).sum::<f32>() / n,
    ];
    let margin_x = (bbox[2] - bbox[0]) * 0.5 + 0.015;
    let margin_y = (bbox[3] - bbox[1]) * 0.5 + 0.015;
    center[0] < bbox[0] - margin_x
        || center[0] > bbox[2] + margin_x
        || center[1] < bbox[1] - margin_y
        || center[1] > bbox[3] + margin_y
}

/// A pose model can mistake raised hands for a face. Sitting needs positive
/// corroboration from a fresh independent head detection, not just the absence
/// of a high-confidence contradiction. This threshold does not count occupants.
pub fn head_supports_sitting(
    p: &Person,
    heads: &[crate::occupancy::Head],
    bed: &crate::region::Region,
) -> bool {
    heads.iter().any(|h| {
        let b = h.bbox;
        if !h.score.is_finite()
            || h.score < 0.7
            || !b.iter().all(|v| v.is_finite() && (0. ..=1.).contains(v))
            || b[2] - b[0] < 0.008
            || b[3] - b[1] < 0.008
            || region::overlap(bed, b) < 0.7
            || head_contradicts_sitting(p, b)
        {
            return false;
        }
        let mx = (b[2] - b[0]) * 0.15 + 0.005;
        let my = (b[3] - b[1]) * 0.15 + 0.005;
        p.points[..5]
            .iter()
            .filter(|q| {
                valid_joint(q, 0.6)
                    && q[0] >= b[0] - mx
                    && q[0] <= b[2] + mx
                    && q[1] >= b[1] - my
                    && q[1] <= b[3] + my
            })
            .count()
            >= 2
    })
}

#[derive(Default)]
pub struct Rule {
    votes: std::collections::VecDeque<Pose>,
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
        self.votes.clear();
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
    pub fn vote_progress(&self) -> (usize, usize) {
        (
            self.votes.len(),
            self.votes.iter().filter(|p| **p == Pose::Upright).count(),
        )
    }
    pub fn update_window_policy(
        &mut self,
        pose: Pose,
        now: f64,
        window: usize,
        required: usize,
        cooldown: f64,
        assisted: bool,
        from_unknown: bool,
    ) -> bool {
        if !now.is_finite() || !(3..=20).contains(&window) || !(2..=window).contains(&required) {
            self.reset_baseline();
            return false;
        }
        let was_ready = self.unknown_ready;
        let was_latched = self.latched;
        // Reuse baseline, cooldown and rearming policy, with duration firing
        // disabled. Unknown results stay in this window instead of erasing it.
        self.update_policy(pose, now, f64::INFINITY, cooldown, assisted, from_unknown);
        if pose == Pose::Lying || pose == Pose::Multiple {
            self.votes.clear();
            return false;
        }
        if (!was_ready && self.unknown_ready) || (was_latched && !self.latched) {
            self.votes.clear();
        }
        self.votes.push_back(pose);
        while self.votes.len() > window {
            self.votes.pop_front();
        }
        let (samples, upright) = self.vote_progress();
        if pose == Pose::Upright
            && samples == window
            && upright >= required
            && (self.armed || assisted || (from_unknown && self.unknown_ready))
            && !self.latched
            && self.last_alarm.is_none_or(|t| now - t >= cooldown)
        {
            self.armed = false;
            self.unknown_ready = false;
            self.latched = true;
            self.last_alarm = Some(now);
            self.votes.clear();
            return true;
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
        p.points[0] = [x + 0.07, 0.18, 0.9];
        p.points[1] = [x + 0.08, 0.18, 0.85];
        p
    }
    #[test]
    fn independent_head_rejects_a_fake_head_on_the_quilt() {
        let p = person(0.3);
        assert!(!head_contradicts_sitting(&p, [0.33, 0.12, 0.42, 0.25]));
        assert!(head_contradicts_sitting(&p, [0.62, 0.58, 0.72, 0.72]));
    }
    #[test]
    fn sitting_requires_independent_head_agreement_even_below_occupancy_threshold() {
        use crate::occupancy::Head;
        let bed = [0., 0., 1., 1.].into();
        let p = person(0.3);
        let actual = Head {
            bbox: [0.33, 0.12, 0.42, 0.25],
            score: 0.8,
        };
        assert!(head_supports_sitting(&p, &[actual.clone()], &bed));
        assert!(!head_supports_sitting(&p, &[], &bed));
        let wrong = Head {
            bbox: [0.62, 0.58, 0.72, 0.72],
            score: 0.84,
        };
        assert!(!head_supports_sitting(&p, &[wrong], &bed));
        let mut weak = actual.clone();
        weak.score = 0.69;
        assert!(!head_supports_sitting(&p, &[weak], &bed));
        // Old broad negative-check margins could accept a face just outside
        // the real head. Positive support must contain two visible landmarks.
        let mut hands = p.clone();
        hands.points[0][0] = 0.44;
        hands.points[1][0] = 0.45;
        assert!(!head_supports_sitting(&hands, &[actual.clone()], &bed));
        let mut invalid = actual.clone();
        invalid.bbox[0] = f32::NAN;
        assert!(!head_supports_sitting(&p, &[invalid], &bed));
        let outside = [0., 0.5, 1., 1.].into();
        assert!(!head_supports_sitting(&p, &[actual], &outside));
    }
    #[test]
    fn raised_hands_cannot_trigger_when_real_head_is_at_pillow() {
        use crate::occupancy::Head;
        let bed = [0., 0., 1., 1.].into();
        let mut p = person(0.53);
        for k in 0..5 {
            p.points[k] = [0.61, 0.44, 0.9];
        }
        p.points[5] = [0.635, 0.518, 0.98];
        p.points[6] = [0.585, 0.498, 0.98];
        p.points[11] = [0.605, 0.649, 0.86];
        p.points[12] = [0.572, 0.623, 0.87];
        assert_eq!(classify(&[p.clone()], &bed, 0.55).0, Pose::Upright);
        for score in [0.69, 0.7, 0.84, 0.87, 0.99] {
            let heads = [Head {
                bbox: [0.633, 0.528, 0.734, 0.673],
                score,
            }];
            assert!(!head_supports_sitting(&p, &heads, &bed));
            let mut rule = Rule::default();
            for t in 0..20 {
                assert!(!rule.update_policy(Pose::Unknown, t as f64, 1.5, 60., false, true));
            }
        }
    }
    #[test]
    fn raised_quilt_without_aligned_head_never_arms_sitting_alarm() {
        let bed = [0., 0., 1., 1.];
        let mut p = person(0.3);
        assert_eq!(classify(&[p.clone()], bed, 0.55).0, Pose::Upright);
        for head in [[0.37, 0.75, 0.95], [0.7, 0.18, 0.95], [0., 0., 0.]] {
            p.points[0] = head;
            p.points[1] = head;
            let pose = classify(&[p.clone()], bed, 0.55).0;
            assert_eq!(pose, Pose::Unknown);
            let mut rule = Rule::default();
            rule.update_policy(Pose::Lying, 0., 1., 60., false, true);
            rule.update_policy(Pose::Lying, 2., 1., 60., false, true);
            for t in 3..20 {
                assert!(!rule.update_policy(pose, t as f64, 1., 60., false, true));
            }
        }
    }
    #[test]
    fn occluded_side_can_sit_only_with_consistent_visible_head() {
        let mut p = person(0.3);
        p.points[0] = [0.; 3];
        p.points[1] = [0.; 3];
        p.points[6] = [0.; 3];
        p.points[12] = [0.; 3];
        let bed = [0., 0., 1., 1.];
        assert_eq!(classify(&[p.clone()], bed, 0.65).0, Pose::Unknown);
        p.points[0] = [0.37, 0.18, 0.9];
        p.points[1] = [0.38, 0.18, 0.85];
        assert_eq!(classify(&[p.clone()], bed, 0.65).0, Pose::Upright);
        p.points[0][1] = 0.5;
        p.points[1][1] = 0.5;
        assert_eq!(classify(&[p.clone()], bed, 0.65).0, Pose::Unknown);
        p.points[0][1] = 0.18;
        p.points[1][1] = 0.18;
        p.points[11][2] = 0.5;
        assert_eq!(classify(&[p], bed, 0.65).0, Pose::Unknown);
    }
    #[test]
    fn conflicting_sides_hidden_hips_and_invalid_joints_do_not_guess() {
        let mut p = person(0.3);
        let bed = [0., 0., 1., 1.];
        p.points[11][2] = 0.;
        p.points[12][2] = 0.;
        p.points[0] = [0.37, 0.18, 0.9];
        p.points[1] = [0.38, 0.18, 0.9];
        assert_eq!(classify(&[p], bed, 0.65).0, Pose::Unknown);
        let mut p = person(0.3);
        p.points[12][1] = 0.1;
        assert_eq!(classify(&[p], bed, 0.65).0, Pose::Unknown);
        let mut p = person(0.3);
        p.points[5][0] = f32::NAN;
        p.points[6][0] = 1.2;
        assert_eq!(classify(&[p], bed, 0.65).0, Pose::Unknown);
    }
    #[test]
    fn horizontal_lying_and_near_horizontal_one_side_do_not_sit() {
        let bed = [0., 0., 1., 1.];
        let mut p = person(0.3);
        for k in [11, 12] {
            p.points[k] = [0.7, 0.3, 0.9];
        }
        assert_eq!(classify(&[p.clone()], bed, 0.65).0, Pose::Lying);
        p.points[6] = [0.; 3];
        p.points[12] = [0.; 3];
        p.points[0] = [0.2, 0.3, 0.9];
        p.points[1] = [0.21, 0.3, 0.9];
        assert_eq!(classify(&[p], bed, 0.65).0, Pose::Unknown);
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

#[cfg(test)]
mod frame_window_tests {
    use super::*;
    fn tick(r: &mut Rule, p: Pose, t: f64) -> bool {
        r.update_window_policy(p, t, 5, 4, 60., false, false)
    }
    fn armed() -> Rule {
        let mut r = Rule::default();
        tick(&mut r, Pose::Lying, 0.);
        tick(&mut r, Pose::Lying, 2.);
        r
    }
    #[test]
    fn one_unknown_is_tolerated_and_full_window_is_required() {
        let mut r = armed();
        for (i, p) in [
            Pose::Upright,
            Pose::Upright,
            Pose::Unknown,
            Pose::Upright,
            Pose::Upright,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(tick(&mut r, p, 2.5 + i as f64 / 2.), i == 4);
        }
        for i in 0..10 {
            assert!(!tick(&mut r, Pose::Upright, 5. + i as f64 / 2.));
        }
    }
    #[test]
    fn latest_unknown_never_fires_and_window_slides() {
        let mut r = armed();
        for (i, p) in [
            Pose::Upright,
            Pose::Upright,
            Pose::Upright,
            Pose::Upright,
            Pose::Unknown,
        ]
        .into_iter()
        .enumerate()
        {
            assert!(!tick(&mut r, p, 2.5 + i as f64 / 2.));
        }
        assert_eq!(r.vote_progress(), (5, 4));
        assert!(tick(&mut r, Pose::Upright, 5.));
        let mut r = armed();
        for (i, p) in [
            Pose::Upright,
            Pose::Unknown,
            Pose::Upright,
            Pose::Unknown,
            Pose::Upright,
        ]
        .into_iter()
        .enumerate()
        {
            assert!(!tick(&mut r, p, 2.5 + i as f64 / 2.));
        }
        assert_eq!(r.vote_progress(), (5, 3));
    }
    #[test]
    fn lying_multiple_disconnect_and_nonfinite_clear_votes() {
        for reset in [Pose::Lying, Pose::Multiple] {
            let mut r = armed();
            tick(&mut r, Pose::Upright, 2.5);
            tick(&mut r, Pose::Upright, 3.);
            assert!(!tick(&mut r, reset, 3.5));
            assert_eq!(r.vote_progress(), (0, 0));
            for i in 0..4 {
                assert!(!tick(&mut r, Pose::Upright, 4. + i as f64 / 2.));
            }
        }
        let mut r = armed();
        tick(&mut r, Pose::Upright, 2.5);
        tick(&mut r, Pose::Upright, 3.);
        assert!(!tick(&mut r, Pose::Upright, 10.));
        assert_eq!(r.vote_progress(), (1, 1));
        assert!(!r.armed);
        assert!(!tick(&mut r, Pose::Upright, f64::NAN));
        assert_eq!(r.vote_progress(), (0, 0));
    }
    #[test]
    fn unknown_opt_in_rearming_and_cooldown_still_apply() {
        let mut r = Rule::default();
        for i in 0..5 {
            assert!(!r.update_window_policy(Pose::Unknown, i as f64 / 2., 5, 4, 60., false, true));
        }
        for i in 0..4 {
            assert_eq!(
                r.update_window_policy(Pose::Upright, 2.5 + i as f64 / 2., 5, 4, 60., false, true),
                i == 3
            );
        }
        for i in 0..5 {
            assert!(!r.update_window_policy(
                Pose::Unknown,
                4.5 + i as f64 / 2.,
                5,
                4,
                60.,
                false,
                true
            ));
        }
        for i in 0..8 {
            assert!(!r.update_window_policy(
                Pose::Upright,
                7. + i as f64 / 2.,
                5,
                4,
                60.,
                false,
                true
            ));
        }
        for i in 0..5 {
            assert!(!r.update_window_policy(
                Pose::Unknown,
                64. + i as f64 / 2.,
                5,
                4,
                60.,
                false,
                true
            ));
        }
        for i in 0..4 {
            assert_eq!(
                r.update_window_policy(Pose::Upright, 66.5 + i as f64 / 2., 5, 4, 60., false, true),
                i == 3
            );
        }
    }
}
