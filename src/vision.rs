use crate::rules::Person;
use anyhow::{bail, Result};
use ort::{session::Session, value::Tensor};
use std::path::Path;

pub struct Detector {
    session: Session,
    head: Option<crate::head::Detector>,
    head_region: Option<crate::region::Region>,
    head_bed: Option<crate::region::Region>,
    head_tick: u8,
    head_last: Option<std::time::Instant>,
    cached_heads: Vec<crate::occupancy::Head>,
}
impl Detector {
    pub fn new(model: &Path) -> Result<Self> {
        let head_path = model.with_file_name("head-detector.onnx");
        let head = if head_path.exists() {
            Some(crate::head::Detector::new(&head_path)?)
        } else {
            None
        };
        Ok(Self {
            head,
            head_region: None,
            head_bed: None,
            head_tick: 0,
            head_last: None,
            cached_heads: vec![],
            session: Session::builder()?
                .with_intra_threads(2)?
                .commit_from_file(model)?,
        })
    }
    /// Full-frame detections determine occupancy. A high-resolution crop may
    /// improve joints for one ambiguous occupant; it never adds/removes people.
    pub fn detect_detailed(
        &mut self,
        rgb: &[u8],
        bed: &crate::region::Region,
        single: f32,
        multiple: f32,
        threshold: f32,
        rest: Option<&crate::region::Region>,
    ) -> Result<(Vec<Person>, Vec<u8>, Vec<crate::occupancy::Head>, bool)> {
        let started = std::time::Instant::now();
        let (people, preview, mut heads) =
            self.detect_inner(rgb, bed, single, multiple, threshold, rest)?;
        let selected = crate::rules::monitor_people(&people, bed, single, multiple);
        let mut conflict = false;
        if crate::rules::classify(&selected, bed, threshold).0 == crate::rules::Pose::Upright {
            conflict = true;
            // A proposed rise must be checked against THIS frame, never a cached
            // lying head from before the movement.
            if self.head.is_some() {
                if self.head_last.is_none_or(|last| last < started) {
                    let frame = image::RgbImage::from_raw(1280, 720, rgb.to_vec()).unwrap();
                    let probe = rest.unwrap_or(bed);
                    self.head_last = Some(std::time::Instant::now());
                    heads = self.head.as_mut().unwrap().detect_region(&frame, probe)?;
                    self.cached_heads = heads.clone();
                }
                if let Some(person) = selected.iter().find(|p| crate::rules::in_bed(p, bed)) {
                    conflict = !crate::rules::head_supports_sitting(person, &heads, bed);
                }
            }
        }
        Ok((people, preview, heads, conflict))
    }
    fn detect_inner(
        &mut self,
        rgb: &[u8],
        bed: &crate::region::Region,
        single: f32,
        multiple: f32,
        threshold: f32,
        rest: Option<&crate::region::Region>,
    ) -> Result<(Vec<Person>, Vec<u8>, Vec<crate::occupancy::Head>)> {
        let frame = image::RgbImage::from_raw(1280, 720, rgb.to_vec())
            .ok_or_else(|| anyhow::anyhow!("视频帧尺寸不正确"))?;
        let preview =
            image::imageops::resize(&frame, 640, 360, image::imageops::FilterType::Triangle);
        let mut people = self.detect(preview.as_raw())?;
        let selected = crate::rules::monitor_people(&people, bed, single, multiple);
        let needs_heads =
            crate::rules::classify(&selected, bed, threshold).0 != crate::rules::Pose::Multiple;
        let probe_region = rest.unwrap_or(bed);
        if self.head_region.as_ref() != Some(probe_region)
            || self.head_bed.as_ref() != Some(bed)
            || !needs_heads
        {
            self.head_tick = 0;
            self.head_last = None;
            self.cached_heads.clear();
            self.head_region = Some(probe_region.clone());
            self.head_bed = Some(bed.clone());
        }
        if needs_heads {
            // Head evidence is sampled at most once per four analysis frames.
            // Cached evidence lasts less than one second at the 4 fps input rate.
            if self.head_tick == 0
                || self
                    .head_last
                    .is_none_or(|t| t.elapsed().as_secs_f64() >= 1.)
            {
                self.head_last = Some(std::time::Instant::now());
                self.cached_heads = match &mut self.head {
                    Some(head) => head.detect_region(&frame, probe_region)?,
                    _ => vec![],
                };
            }
            self.head_tick = (self.head_tick + 1) % 4;
        }
        let heads = self.cached_heads.clone();
        if crate::rules::classify(&selected, bed, threshold) != (crate::rules::Pose::Unknown, 1) {
            return Ok((people, preview.into_raw(), heads));
        }
        let target = selected
            .iter()
            .find(|p| crate::rules::in_bed(p, bed))
            .unwrap();
        let Some(index) = people
            .iter()
            .position(|p| p.bbox == target.bbox && p.score == target.score)
        else {
            return Ok((people, preview.into_raw(), heads));
        };
        let b = people[index].bbox;
        let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
        if bw < 0.04 || bh < 0.08 {
            return Ok((people, preview.into_raw(), heads));
        }
        let x = ((b[0] - bw * 0.25).max(0.) * 1280.).floor() as u32;
        let y = ((b[1] - bh * 0.25).max(0.) * 720.).floor() as u32;
        let right = ((b[2] + bw * 0.25).min(1.) * 1280.).ceil() as u32;
        let bottom = ((b[3] + bh * 0.25).min(1.) * 720.).ceil() as u32;
        let (w, h) = (right - x, bottom - y);
        let crop = image::imageops::crop_imm(&frame, x, y, w, h).to_image();
        let scale = (640. / w as f32).min(360. / h as f32);
        let (cw, ch) = (
            ((w as f32 * scale).round() as u32).clamp(1, 640),
            ((h as f32 * scale).round() as u32).clamp(1, 360),
        );
        let resized = image::imageops::resize(&crop, cw, ch, image::imageops::FilterType::Triangle);
        let mut padded = image::RgbImage::from_pixel(640, 360, image::Rgb([114, 114, 114]));
        let (ox, oy) = ((640 - cw) / 2, (360 - ch) / 2);
        image::imageops::overlay(&mut padded, &resized, ox as i64, oy as i64);
        let mut candidates = self.detect(padded.as_raw())?;
        if candidates.len() == 1 {
            let mut refined = candidates.remove(0);
            let map = |p: [f32; 2]| {
                [
                    (x as f32 + (p[0] * 640. - ox as f32) * w as f32 / cw as f32) / 1280.,
                    (y as f32 + (p[1] * 360. - oy as f32) * h as f32 / ch as f32) / 720.,
                ]
            };
            let a = map([refined.bbox[0], refined.bbox[1]]);
            let b = map([refined.bbox[2], refined.bbox[3]]);
            refined.bbox = [a[0], a[1], b[0], b[1]];
            for joint in &mut refined.points {
                let q = map([joint[0], joint[1]]);
                joint[0] = q[0];
                joint[1] = q[1];
            }
            refine_joints(&mut people[index], &refined);
        }
        Ok((people, preview.into_raw(), heads))
    }
    pub fn detect(&mut self, rgb: &[u8]) -> Result<Vec<Person>> {
        if rgb.len() != 640 * 360 * 3 {
            bail!("视频帧尺寸不正确");
        }
        let mut input = vec![114.0 / 255.0; 3 * 640 * 640];
        for y in 0..360 {
            for x in 0..640 {
                for c in 0..3 {
                    input[c * 640 * 640 + (y + 140) * 640 + x] =
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
                    ((cy - h / 2. - 140.) / 360.).clamp(0., 1.),
                    ((cx + w / 2.) / 640.).clamp(0., 1.),
                    ((cy + h / 2. - 140.) / 360.).clamp(0., 1.),
                ],
                score,
                points: [[0.; 3]; 17],
            };
            for k in 0..17 {
                p.points[k] = [
                    v(5 + 3 * k) / 640.,
                    (v(6 + 3 * k) - 140.) / 360.,
                    v(7 + 3 * k),
                ];
            }
            candidates.push(p);
        }
        Ok(deduplicate(candidates))
    }
}
/// Reject mismatched crops and conflicting visible joints. Preserve the original
/// confidence/box so refinement cannot weaken multiple-person suppression.
fn refine_joints(original: &mut Person, refined: &Person) -> bool {
    let good = |p: &&[f32; 3]| {
        p.iter().all(|v| v.is_finite())
            && p[2] >= 0.6
            && (0. ..=1.).contains(&p[0])
            && (0. ..=1.).contains(&p[1])
    };
    if !refined.score.is_finite()
        || refined.score < 0.5
        || refined.score < original.score * 0.7
        || iou(original.bbox, refined.bbox) < 0.45
    {
        return false;
    }
    let diagonal = (original.bbox[2] - original.bbox[0]).hypot(original.bbox[3] - original.bbox[1]);
    if original
        .points
        .iter()
        .zip(&refined.points)
        .any(|(a, b)| good(&a) && good(&b) && (a[0] - b[0]).hypot(a[1] - b[1]) > diagonal * 0.15)
    {
        return false;
    }
    let support = |p: &Person| {
        [5, 6, 11, 12]
            .iter()
            .filter(|&&k| good(&&p.points[k]))
            .count()
    };
    if support(refined) <= support(original) {
        return false;
    }
    original.points = refined.points;
    true
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
    fn detail_refinement_preserves_occupancy_and_rejects_other_person() {
        let mut base = person([0.2, 0.05, 0.8, 0.95], 0.);
        base.points[11][2] = 0.2;
        base.points[12][2] = 0.2;
        let detailed = person(base.bbox, 0.);
        let (bbox, score) = (base.bbox, base.score);
        assert!(refine_joints(&mut base, &detailed));
        assert_eq!(base.bbox, bbox);
        assert_eq!(base.score, score);
        let mut base = person(bbox, 0.);
        base.points[11][2] = 0.2;
        let unrelated = person(bbox, 0.2);
        assert!(!refine_joints(&mut base, &unrelated));
        let mut unrelated = person([0.7, 0.1, 0.95, 0.9], 0.);
        assert!(!refine_joints(&mut base, &unrelated));
        unrelated = person(bbox, 0.);
        unrelated.score = 0.4;
        assert!(!refine_joints(&mut base, &unrelated));
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
