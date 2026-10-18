//! Fixed-camera head/shoulder references; coordinates only, never stored images.
use crate::rules::{Person, Pose};
use serde::{Deserialize, Serialize};
use std::time::Instant;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Reference {
    pub head: Option<[f32; 2]>,
    pub shoulders: Option<[f32; 2]>,
}
impl Reference {
    pub fn valid(&self) -> bool {
        (self.head.is_some() || self.shoulders.is_some())
            && [self.head, self.shoulders]
                .into_iter()
                .flatten()
                .all(|p| p.iter().all(|v| v.is_finite() && (0. ..=1.).contains(v)))
    }
    pub fn extract(p: &Person) -> Self {
        let good = |v: &&[f32; 3]| {
            v.iter().all(|x| x.is_finite())
                && v[2] >= 0.65
                && (0. ..=1.).contains(&v[0])
                && (0. ..=1.).contains(&v[1])
        };
        let points: Vec<_> = p.points[..5].iter().filter(good).collect();
        // A fixed nose anchor avoids shifts caused by a changing centroid of visible joints.
        let midpoint = |a: usize, b: usize| {
            let (a, b) = (&p.points[a], &p.points[b]);
            if good(&a) && good(&b) {
                Some([(a[0] + b[0]) / 2., (a[1] + b[1]) / 2.])
            } else {
                None
            }
        };
        let head = if points.len() >= 2 && good(&&p.points[0]) {
            Some([p.points[0][0], p.points[0][1]])
        } else {
            None
        };
        Self {
            head,
            shoulders: midpoint(5, 6),
        }
    }
}
fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
pub fn ready(lying: &Option<Reference>, upright: &Option<Reference>) -> bool {
    match (lying, upright) {
        (Some(l), Some(u)) if l.valid() && u.valid() => {
            [(l.head, u.head), (l.shoulders, u.shoulders)]
                .into_iter()
                .any(|(a, b)| a.zip(b).is_some_and(|(a, b)| distance(a, b) >= 0.08))
        }
        _ => false,
    }
}
pub fn classify(l: &Reference, u: &Reference, p: &Reference) -> Pose {
    let mut votes = vec![];
    for ((a, b), v) in [(l.head, u.head), (l.shoulders, u.shoulders)]
        .into_iter()
        .zip([p.head, p.shoulders])
    {
        let Some(((a, b), v)) = a.zip(b).zip(v) else {
            continue;
        };
        let length = distance(a, b);
        if length < 0.08 {
            continue;
        }
        let t = ((v[0] - a[0]) * (b[0] - a[0]) + (v[1] - a[1]) * (b[1] - a[1])) / (length * length);
        votes.push(
            if (-0.25..=0.3).contains(&t) && distance(v, a) <= length * 0.4 + 0.01 {
                Pose::Lying
            } else if (0.7..=1.35).contains(&t) && distance(v, b) <= length * 0.4 + 0.01 {
                Pose::Upright
            } else {
                Pose::Unknown
            },
        );
    }
    if votes.is_empty() || votes.iter().any(|v| *v != votes[0]) {
        Pose::Unknown
    } else {
        votes[0]
    }
}
#[derive(Default)]
pub struct Samples {
    rows: Vec<(Instant, Reference)>,
}
impl Samples {
    pub fn clear(&mut self) {
        self.rows.clear()
    }
    pub fn push(&mut self, p: Option<Reference>) {
        let now = Instant::now();
        self.rows
            .retain(|(t, _)| now.duration_since(*t).as_secs_f64() < 2.5);
        if let Some(p) = p.filter(Reference::valid) {
            self.rows.push((now, p));
        } else {
            self.clear()
        }
        if self.rows.len() > 12 {
            self.rows.remove(0);
        }
    }
    pub fn capture(&self) -> Option<Reference> {
        if self.rows.len() < 5
            || self.rows.last()?.0.elapsed().as_secs_f64() > 1.5
            || self
                .rows
                .last()?
                .0
                .duration_since(self.rows.first()?.0)
                .as_secs_f64()
                < 1.
        {
            return None;
        }
        let mean = |head: bool| -> Option<[f32; 2]> {
            let points: Vec<_> = self
                .rows
                .iter()
                .filter_map(|(_, p)| if head { p.head } else { p.shoulders })
                .collect();
            if points.len() != self.rows.len() {
                return None;
            }
            let n = points.len() as f32;
            let center = [
                points.iter().map(|p| p[0]).sum::<f32>() / n,
                points.iter().map(|p| p[1]).sum::<f32>() / n,
            ];
            if points.iter().all(|p| distance(*p, center) <= 0.025) {
                Some(center)
            } else {
                None
            }
        };
        let r = Reference {
            head: mean(true),
            shoulders: mean(false),
        };
        r.valid().then_some(r)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn r(y: f32) -> Reference {
        Reference {
            head: Some([0.4, y]),
            shoulders: None,
        }
    }
    #[test]
    fn calibrated_head_can_classify_without_hips() {
        let (l, u) = (r(0.7), r(0.3));
        assert!(ready(&Some(l.clone()), &Some(u.clone())));
        assert_eq!(classify(&l, &u, &r(0.31)), Pose::Upright);
        assert_eq!(classify(&l, &u, &r(0.69)), Pose::Lying);
        assert_eq!(classify(&l, &u, &r(0.5)), Pose::Unknown);
        assert_eq!(
            classify(
                &l,
                &u,
                &Reference {
                    head: Some([0.8, 0.3]),
                    shoulders: None
                }
            ),
            Pose::Unknown
        );
        assert!(!ready(&Some(l), &Some(r(0.69))));
    }
    #[test]
    fn disagreeing_or_missing_anchors_do_not_guess() {
        let l = Reference {
            head: Some([0.4, 0.7]),
            shoulders: Some([0.4, 0.8]),
        };
        let u = Reference {
            head: Some([0.4, 0.3]),
            shoulders: Some([0.4, 0.4]),
        };
        assert_eq!(
            classify(
                &l,
                &u,
                &Reference {
                    head: u.head,
                    shoulders: l.shoulders
                }
            ),
            Pose::Unknown
        );
        assert_eq!(classify(&l, &u, &Reference::default()), Pose::Unknown);
        assert!(!r(f32::NAN).valid());
    }
}

#[cfg(test)]
mod sample_tests {
    use super::*;
    #[test]
    fn only_recent_stable_complete_samples_can_be_captured() {
        let reference = Reference {
            head: Some([0.4, 0.7]),
            shoulders: None,
        };
        let now = Instant::now();
        let mut samples = Samples {
            rows: (0..6)
                .map(|n| {
                    (
                        now - std::time::Duration::from_millis(1500 - n * 250),
                        reference.clone(),
                    )
                })
                .collect(),
        };
        assert!(samples.capture().is_some());
        samples.rows[0].1.head = Some([0.9, 0.9]);
        assert!(samples.capture().is_none());
        samples.push(None);
        assert!(samples.capture().is_none());
        samples.rows = (0..6)
            .map(|n| {
                (
                    now - std::time::Duration::from_millis(5000 - n * 250),
                    reference.clone(),
                )
            })
            .collect();
        assert!(samples.capture().is_none());
    }
}
