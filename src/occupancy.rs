//! Head evidence may pause multi-person scenes, but never confirms a single posture.
use crate::{
    region::{self, Region},
    rules::Pose,
};
pub const MIN_HEAD_CONFIDENCE: f32 = 0.85;
#[derive(Clone, Debug, serde::Serialize)]
pub struct Head {
    pub bbox: [f32; 4],
    pub score: f32,
}
/// A second head uses at least 0.85 confidence and the configured second-person
/// threshold. Merge alternate crops/rotations of the same head before counting.
pub fn distinct_heads(heads: &[Head], bed: &Region, threshold: f32) -> Vec<Head> {
    let mut candidates: Vec<_> = heads
        .iter()
        .filter(|h| {
            h.score.is_finite()
                && h.score >= threshold.max(MIN_HEAD_CONFIDENCE)
                && h.bbox
                    .iter()
                    .all(|v| v.is_finite() && (0. ..=1.).contains(v))
                && h.bbox[2] - h.bbox[0] >= 0.008
                && h.bbox[3] - h.bbox[1] >= 0.008
                && region::overlap(bed, h.bbox) >= 0.7
        })
        .cloned()
        .collect();
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Head> = vec![];
    for h in candidates {
        let duplicate = kept.iter().any(|k| {
            let area = |b: [f32; 4]| (b[2] - b[0]) * (b[3] - b[1]);
            let overlap = (h.bbox[2].min(k.bbox[2]) - h.bbox[0].max(k.bbox[0])).max(0.)
                * (h.bbox[3].min(k.bbox[3]) - h.bbox[1].max(k.bbox[1])).max(0.);
            let distance = (((h.bbox[0] + h.bbox[2] - k.bbox[0] - k.bbox[2]) / 2.) * 16. / 9.)
                .hypot((h.bbox[1] + h.bbox[3] - k.bbox[1] - k.bbox[3]) / 2.);
            let diagonal = |b: [f32; 4]| ((b[2] - b[0]) * 16. / 9.).hypot(b[3] - b[1]);
            overlap / area(h.bbox).min(area(k.bbox)) >= 0.5
                || distance <= diagonal(h.bbox).max(diagonal(k.bbox)) * 0.6
        });
        if !duplicate {
            kept.push(h);
        }
    }
    kept
}
pub fn head_safety(
    pose: Pose,
    people: usize,
    heads: &[Head],
    bed: &Region,
    threshold: f32,
) -> (Pose, usize, bool) {
    let count = distinct_heads(heads, bed, threshold).len();
    if count >= 2 {
        (Pose::Multiple, people.max(count), true)
    } else {
        (pose, people, false)
    }
}

/// Don't resume alarms during a brief miss of an already observed second person.
#[derive(Default)]
pub struct MultiPause {
    last: Option<f64>,
    clear_since: Option<f64>,
    count: usize,
}
impl MultiPause {
    pub fn update(&mut self, now: f64, pose: Pose, count: usize) -> (Pose, usize, bool) {
        if !now.is_finite() {
            return (Pose::Multiple, count.max(2), true);
        }
        if pose == Pose::Multiple {
            self.last = Some(now);
            self.clear_since = None;
            self.count = count.max(2);
            return (pose, self.count, false);
        }
        if let Some(last) = self.last {
            if now < last || now - last > 5. {
                self.clear_since = None;
            }
            self.last = Some(now);
            let start = *self.clear_since.get_or_insert(now);
            if now - start < 3. {
                return (Pose::Multiple, self.count.max(2), true);
            }
            *self = Self::default();
        }
        (pose, count, false)
    }
}

#[cfg(test)]
mod multi_safety_tests {
    use super::*;
    fn h(bbox: [f32; 4], score: f32) -> Head {
        Head { bbox, score }
    }
    #[test]
    fn covered_elder_and_caregiver_heads_override_one_upright_body() {
        let bed: Region = [0., 0., 1., 1.].into();
        // Geometry/scores from the locally inspected false-alarm screenshot;
        // no image or personal information is included in the test fixture.
        let heads = [
            h([0.4614, 0.6272, 0.5507, 0.8078], 0.923),
            h([0.3224, 0.2411, 0.4445, 0.4479], 0.884),
            h([0.3837, 0.7448, 0.5764, 0.9974], 0.737),
        ];
        let (pose, count, guard) = head_safety(Pose::Upright, 1, &heads, &bed, 0.8);
        assert_eq!((pose, count, guard), (Pose::Multiple, 2, true));
        let mut rule = crate::rules::Rule::default();
        for t in 0..3 {
            rule.update_policy(Pose::Unknown, t as f64, 1.5, 10., false, true);
        }
        for t in 3..20 {
            assert!(!rule.update_policy(pose, t as f64, 1.5, 10., false, true));
        }
    }
    #[test]
    fn duplicates_weak_objects_outside_bed_and_single_head_do_not_create_people() {
        let bed: Region = [0., 0., 0.8, 1.].into();
        let heads = [
            h([0.4, 0.4, 0.5, 0.6], 0.94),
            h([0.405, 0.405, 0.505, 0.605], 0.92),
            h([0.44, 0.46, 0.49, 0.57], 0.91),
            h([0.1, 0.1, 0.2, 0.3], 0.8),
            h([0.85, 0.1, 0.95, 0.3], 0.95),
            h([f32::NAN, 0.1, 0.3, 0.3], 0.99),
        ];
        assert_eq!(distinct_heads(&heads, &bed, 0.8).len(), 1);
        assert_eq!(
            head_safety(Pose::Unknown, 0, &heads, &bed, 0.8),
            (Pose::Unknown, 0, false)
        );
        assert_eq!(
            head_safety(Pose::Upright, 1, &heads, &bed, 0.8),
            (Pose::Upright, 1, false)
        );
        assert_eq!(
            head_safety(Pose::Multiple, 2, &[], &bed, 0.8),
            (Pose::Multiple, 2, false)
        );
    }
    #[test]
    fn configured_higher_second_person_threshold_still_applies() {
        let bed: Region = [0., 0., 1., 1.].into();
        let heads = [h([0.1, 0.1, 0.2, 0.3], 0.95), h([0.5, 0.5, 0.6, 0.7], 0.89)];
        assert_eq!(distinct_heads(&heads, &bed, 0.8).len(), 2);
        assert_eq!(distinct_heads(&heads, &bed, 0.9).len(), 1);
    }
    #[test]
    fn brief_second_person_miss_never_resumes_alarms_and_gaps_do_not_count() {
        let mut guard = MultiPause::default();
        assert_eq!(
            guard.update(0., Pose::Multiple, 2),
            (Pose::Multiple, 2, false)
        );
        for t in [0.5, 1., 1.5, 2., 2.5, 3.] {
            assert_eq!(guard.update(t, Pose::Upright, 1).0, Pose::Multiple);
        }
        assert_eq!(
            guard.update(3.5, Pose::Upright, 1),
            (Pose::Upright, 1, false)
        );
        guard.update(4., Pose::Multiple, 2);
        guard.update(4.5, Pose::Lying, 1);
        assert_eq!(guard.update(10., Pose::Upright, 1).0, Pose::Multiple);
        for t in [10.5, 11., 11.5, 12., 12.5] {
            assert_eq!(guard.update(t, Pose::Upright, 1).0, Pose::Multiple);
        }
        assert_eq!(guard.update(13., Pose::Upright, 1).0, Pose::Upright);
    }
}
#[derive(Default)]
pub struct Tracker {
    region: Option<Region>,
    bed: Option<Region>,
    anchor: Option<Head>,
    since: f64,
    last: Option<f64>,
}
impl Tracker {
    pub fn clear(&mut self) {
        self.anchor = None;
        self.last = None;
    }
    pub fn update(
        &mut self,
        now: f64,
        heads: &[Head],
        bed: &Region,
        rest: &Option<Region>,
        pose: Pose,
        people: usize,
    ) -> bool {
        if self.region != *rest || self.bed.as_ref() != Some(bed) {
            self.clear();
            self.region = rest.clone();
            self.bed = Some(bed.clone());
        }
        let Some(rest) = rest else {
            self.clear();
            return false;
        };
        if !now.is_finite() || pose != Pose::Unknown || people > 1 {
            self.clear();
            return false;
        }
        let heads: Vec<_> = heads
            .iter()
            .filter(|h| {
                h.score.is_finite()
                    && h.score >= MIN_HEAD_CONFIDENCE
                    && h.bbox
                        .iter()
                        .all(|v| v.is_finite() && (0. ..=1.).contains(v))
                    && h.bbox[2] - h.bbox[0] >= 0.008
                    && h.bbox[3] - h.bbox[1] >= 0.008
                    && region::overlap(bed, h.bbox) >= 0.7
            })
            .collect();
        if heads.len() != 1 || region::overlap(rest, heads[0].bbox) < 0.5 {
            self.clear();
            return false;
        }
        let head = heads[0];
        let center = |h: &Head| [(h.bbox[0] + h.bbox[2]) / 2., (h.bbox[1] + h.bbox[3]) / 2.];
        let continuous = self.last.is_some_and(|t| now >= t && now - t <= 0.8)
            && self.anchor.as_ref().is_some_and(|a| {
                let (a, b) = (center(a), center(head));
                (a[0] - b[0]).hypot(a[1] - b[1]) <= 0.04
            });
        if !continuous {
            self.since = now;
            self.anchor = Some(head.clone());
        }
        self.last = Some(now);
        now - self.since >= 3.
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn head(x: f32) -> Head {
        Head {
            bbox: [x, 0.3, x + 0.08, 0.4],
            score: 0.95,
        }
    }
    #[test]
    fn sustained_head_requires_region_and_never_confirms_posture() {
        let mut t = Tracker::default();
        let bed = [0., 0., 1., 1.].into();
        let r = Some([0.2, 0.2, 0.3, 0.3].into());
        for k in 0..12 {
            assert!(!t.update(k as f64 / 4., &[head(0.3)], &bed, &r, Pose::Unknown, 0));
        }
        assert!(t.update(3., &[head(0.3)], &bed, &r, Pose::Unknown, 0));
        assert!(!t.update(3.25, &[], &bed, &r, Pose::Unknown, 0));
        assert!(!t.update(3.5, &[head(0.3)], &bed, &r, Pose::Unknown, 0));
    }
    #[test]
    fn objects_multiple_upright_gaps_and_region_changes_clear() {
        let mut t = Tracker::default();
        let bed = [0., 0., 1., 1.].into();
        let r = Some([0.2, 0.2, 0.3, 0.3].into());
        for k in 0..16 {
            t.update(k as f64 / 4., &[head(0.3)], &bed, &r, Pose::Unknown, 0);
        }
        assert!(!t.update(4., &[head(0.3), head(0.4)], &bed, &r, Pose::Unknown, 0));
        assert!(!t.update(4.25, &[head(0.3)], &bed, &r, Pose::Upright, 1));
        assert!(!t.update(4.5, &[head(0.3)], &bed, &r, Pose::Multiple, 2));
        assert!(!t.update(4.75, &[head(0.3)], &bed, &None, Pose::Unknown, 0));
        assert!(!t.update(10., &[head(0.3)], &bed, &r, Pose::Unknown, 0));
        assert!(!t.update(9., &[head(0.3)], &bed, &r, Pose::Unknown, 0));
        let mut weak = head(0.3);
        weak.score = 0.6;
        assert!(!t.update(9.25, &[weak], &bed, &r, Pose::Unknown, 0));
        assert!(!t.update(9.5, &[head(0.7)], &bed, &r, Pose::Unknown, 0));
    }
    #[test]
    fn changing_bed_or_head_region_and_moving_head_restarts_evidence() {
        let bed: Region = [0., 0., 1., 1.].into();
        let r = Some([0.2, 0.2, 0.3, 0.3].into());
        let mut t = Tracker::default();
        for k in 0..16 {
            t.update(k as f64 / 4., &[head(0.3)], &bed, &r, Pose::Unknown, 0);
        }
        assert!(!t.update(4., &[head(0.38)], &bed, &r, Pose::Unknown, 0));
        for k in 17..32 {
            t.update(k as f64 / 4., &[head(0.3)], &bed, &r, Pose::Unknown, 0);
        }
        let changed = Some([0.19, 0.2, 0.32, 0.3].into());
        assert!(!t.update(8., &[head(0.3)], &bed, &changed, Pose::Unknown, 0));
        for k in 33..48 {
            t.update(
                k as f64 / 4.,
                &[head(0.3)],
                &bed,
                &changed,
                Pose::Unknown,
                0,
            );
        }
        let new_bed: Region = [0., 0., 0.99, 1.].into();
        assert!(!t.update(12., &[head(0.3)], &new_bed, &changed, Pose::Unknown, 0));
    }
}
