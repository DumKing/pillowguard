//! Display-only evidence. Never changes posture, occupancy counts or alarm baselines.
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
