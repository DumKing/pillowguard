use crate::rules::Person;
use anyhow::{bail, Result};
use ort::{session::Session, value::Tensor};
use std::path::Path;

pub struct Detector {
    session: Session,
}
impl Detector {
    pub fn new(model: &Path) -> Result<Self> {
        Ok(Self {
            session: Session::builder()?
                .with_intra_threads(2)?
                .commit_from_file(model)?,
        })
    }
    pub fn detect(&mut self, rgb: &[u8]) -> Result<Vec<Person>> {
        if rgb.len() != 640 * 480 * 3 {
            bail!("视频帧尺寸不正确");
        }
        let mut input = vec![114.0 / 255.0; 3 * 640 * 640];
        for y in 0..480 {
            for x in 0..640 {
                for c in 0..3 {
                    input[c * 640 * 640 + (y + 80) * 640 + x] =
                        rgb[(y * 640 + x) * 3 + c] as f32 / 255.;
                }
            }
        }
        let outputs = self.session.run(ort::inputs![Tensor::from_array((
            [1usize, 3, 640, 640],
            input
        ))?])?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        if shape.len() != 3 || shape[0] != 1 || shape[1] != 56 {
            bail!("模型输出不兼容：需要 [1,56,N]");
        }
        let n = shape[2] as usize;
        let mut candidates = Vec::new();
        for j in 0..n {
            let v = |i: usize| data[i * n + j];
            let score = v(4);
            if !score.is_finite() || score < 0.35 {
                continue;
            }
            let (cx, cy, w, h) = (v(0), v(1), v(2), v(3));
            if ![cx, cy, w, h].iter().all(|v| v.is_finite()) || w <= 0. || h <= 0. {
                continue;
            }
            let mut p = Person {
                bbox: [
                    ((cx - w / 2.) / 640.).clamp(0., 1.),
                    ((cy - h / 2. - 80.) / 480.).clamp(0., 1.),
                    ((cx + w / 2.) / 640.).clamp(0., 1.),
                    ((cy + h / 2. - 80.) / 480.).clamp(0., 1.),
                ],
                score,
                points: [[0.; 3]; 17],
            };
            for k in 0..17 {
                p.points[k] = [
                    v(5 + 3 * k) / 640.,
                    (v(6 + 3 * k) - 80.) / 480.,
                    v(7 + 3 * k),
                ];
            }
            candidates.push(p);
        }
        Ok(deduplicate(candidates))
    }
}
/// Suppress duplicate boxes using matching joints, while retaining overlapping people
/// whose visible joints are clearly different. Nested head/body boxes may have low IoU.
fn same_person(a: &Person, b: &Person) -> bool {
    let area = |p: &Person| ((p.bbox[2] - p.bbox[0]) * (p.bbox[3] - p.bbox[1])).max(0.00001);
    let overlap = (a.bbox[2].min(b.bbox[2]) - a.bbox[0].max(b.bbox[0])).max(0.)
        * (a.bbox[3].min(b.bbox[3]) - a.bbox[1].max(b.bbox[1])).max(0.);
    let union_iou = iou(a.bbox, b.bbox);
    if union_iou < 0.25 && overlap / area(a).min(area(b)) < 0.8 {
        return false;
    }
    let diagonal = |p: &Person| (p.bbox[2] - p.bbox[0]).hypot(p.bbox[3] - p.bbox[1]);
    let tolerance = (diagonal(a).min(diagonal(b)) * 0.035).clamp(0.006, 0.025);
    let mut common = 0;
    let mut aligned = 0;
    let mut head_common = 0;
    let mut head_aligned = 0;
    for (k, (p, q)) in a.points.iter().zip(b.points.iter()).enumerate() {
        if k < 5 && p.iter().chain(q).all(|v| v.is_finite()) && p[2] >= 0.65 && q[2] >= 0.65 {
            head_common += 1;
            if (p[0] - q[0]).hypot(p[1] - q[1]) <= tolerance {
                head_aligned += 1;
            }
        }
        if p.iter().chain(q).all(|v| v.is_finite()) && p[2] >= 0.5 && q[2] >= 0.5 {
            common += 1;
            if (p[0] - q[0]).hypot(p[1] - q[1]) <= tolerance {
                aligned += 1;
            }
        }
    }
    if head_common >= 2
        && head_aligned == head_common
        && (union_iou >= 0.4 || overlap / area(a).min(area(b)) >= 0.9)
    {
        return true;
    }
    if common >= 4 {
        return aligned >= 4 && aligned as f32 / common as f32 >= 0.8;
    }
    // Without reliable joints, only near-identical boxes can be safely treated as duplicates.
    union_iou >= 0.85
}
fn deduplicate(mut candidates: Vec<Person>) -> Vec<Person> {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept = Vec::<Person>::new();
    for p in candidates {
        if let Some(q) = kept.iter_mut().find(|q| same_person(&p, q)) {
            // Preserve the full body and the best visible joints when a head-only box
            // has higher confidence than a duplicate whole-body box.
            q.bbox = [
                q.bbox[0].min(p.bbox[0]),
                q.bbox[1].min(p.bbox[1]),
                q.bbox[2].max(p.bbox[2]),
                q.bbox[3].max(p.bbox[3]),
            ];
            q.score = q.score.max(p.score);
            for k in 0..17 {
                if p.points[k][2] > q.points[k][2] {
                    q.points[k] = p.points[k];
                }
            }
        } else {
            kept.push(p);
        }
        if kept.len() >= 30 {
            break;
        }
    }
    kept
}
fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let i = (a[2].min(b[2]) - a[0].max(b[0])).max(0.) * (a[3].min(b[3]) - a[1].max(b[1])).max(0.);
    i / (((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - i).max(0.00001))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn person(bbox: [f32; 4], offset: f32) -> Person {
        Person {
            bbox,
            score: 0.9,
            points: std::array::from_fn(|i| {
                [
                    0.4 + offset + (i % 2) as f32 * 0.04,
                    0.2 + i as f32 * 0.025,
                    0.9,
                ]
            }),
        }
    }
    #[test]
    fn nested_boxes_with_matching_joints_are_one_person() {
        let a = person([0.2, 0.05, 0.8, 0.95], 0.);
        let mut b = person([0.3, 0.1, 0.7, 0.5], 0.);
        b.score = 0.95;
        for k in 5..17 {
            b.points[k][2] = 0.;
        }
        assert!(iou(a.bbox, b.bbox) < 0.45);
        let result = deduplicate(vec![a, b]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].bbox, [0.2, 0.05, 0.8, 0.95]);
        assert!(result[0].points[11][2] >= 0.8);
    }
    #[test]
    fn covered_body_and_visible_head_are_deduplicated() {
        let mut a = person([0.1, 0.1, 0.9, 0.9], 0.);
        let mut b = person([0.3, 0.1, 0.6, 0.4], 0.);
        a.points = [[0.; 3]; 17];
        b.points = [[0.; 3]; 17];
        for (k, x) in [(0, 0.4), (1, 0.42)] {
            a.points[k] = [x, 0.22, 0.9];
            b.points[k] = a.points[k];
        }
        assert_eq!(deduplicate(vec![a, b]).len(), 1);
    }
    #[test]
    fn overlapping_distinct_people_are_not_merged() {
        let a = person([0.2, 0.1, 0.8, 0.95], 0.);
        let b = person([0.24, 0.12, 0.84, 0.97], 0.1);
        assert!(iou(a.bbox, b.bbox) > 0.45);
        assert_eq!(deduplicate(vec![a, b]).len(), 2);
    }
    #[test]
    fn occluded_second_person_is_kept() {
        let a = person([0.1, 0.1, 0.4, 0.9], 0.);
        let mut b = person([0.35, 0.1, 0.65, 0.9], 0.2);
        b.points = [[0.; 3]; 17];
        assert_eq!(deduplicate(vec![a, b]).len(), 2);
    }
}
