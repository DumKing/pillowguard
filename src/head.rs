//! Head detector evidence only; never identifies a person or changes posture.
use crate::{
    occupancy::Head,
    region::{Region, RegionLike},
};
use anyhow::{bail, Result};
use ort::{session::Session, value::Tensor};
use std::path::Path;
pub struct Detector {
    session: Session,
}
impl Detector {
    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            session: Session::builder()?
                .with_intra_threads(1)?
                .commit_from_file(path)?,
        })
    }
    pub fn detect_region(&mut self, frame: &image::RgbImage, region: &Region) -> Result<Vec<Head>> {
        let mut heads = self.detect(frame)?;
        let v = region.vertices();
        let b = v.iter().fold([1f32, 1., 0., 0.], |a, p| {
            [
                a[0].min(p[0]),
                a[1].min(p[1]),
                a[2].max(p[0]),
                a[3].max(p[1]),
            ]
        });
        if (b[2] - b[0]) * (b[3] - b[1]) < 0.8 {
            let x = ((b[0] - (b[2] - b[0]) * 0.25).max(0.) * frame.width() as f32).floor() as u32;
            let y = ((b[1] - (b[3] - b[1]) * 0.25).max(0.) * frame.height() as f32).floor() as u32;
            let r = ((b[2] + (b[2] - b[0]) * 0.25).min(1.) * frame.width() as f32).ceil() as u32;
            let d = ((b[3] + (b[3] - b[1]) * 0.25).min(1.) * frame.height() as f32).ceil() as u32;
            if r > x && d > y {
                let crop = image::imageops::crop_imm(frame, x, y, r - x, d - y).to_image();
                for mut h in self.detect(&crop)? {
                    for k in [0, 2] {
                        h.bbox[k] = (x as f32 + h.bbox[k] * (r - x) as f32) / frame.width() as f32;
                    }
                    for k in [1, 3] {
                        h.bbox[k] = (y as f32 + h.bbox[k] * (d - y) as f32) / frame.height() as f32;
                    }
                    heads.push(h);
                }
            }
        }
        let mut heads = suppress(heads);
        // A weak, spatially located head may be too small in the full frame.
        // Re-run at native image detail, retaining the 5:4 model input aspect.
        // Never manufacture a stronger score by averaging repeated predictions.
        let seeds: Vec<_> = heads
            .iter()
            .filter(|h| {
                h.score >= 0.5
                    && h.score < crate::occupancy::MIN_HEAD_CONFIDENCE
                    && crate::region::overlap(region, h.bbox) >= 0.5
            })
            .take(2)
            .cloned()
            .collect();
        for seed in seeds {
            for scale in [2.5, 4.] {
                let Some(bounds) = context_bounds(seed.bbox, frame.width(), frame.height(), scale)
                else {
                    continue;
                };
                let [x, y, w, h] = bounds;
                let crop = image::imageops::crop_imm(frame, x, y, w, h).to_image();
                let refined: Vec<_> = self
                    .detect(&crop)?
                    .into_iter()
                    .filter(|h| h.score >= crate::occupancy::MIN_HEAD_CONFIDENCE)
                    .map(|h| from_crop(h, bounds, frame.width(), frame.height()))
                    .collect();
                if let Some(head) = consistent_refinement(&seed, &refined) {
                    heads.push(head);
                    break;
                }
            }
        }
        Ok(suppress(heads))
    }
    fn detect(&mut self, frame: &image::RgbImage) -> Result<Vec<Head>> {
        let mut heads = vec![];
        for turn in 0..4 {
            let rotated = match turn {
                1 => image::imageops::rotate90(frame),
                2 => image::imageops::rotate180(frame),
                3 => image::imageops::rotate270(frame),
                _ => frame.clone(),
            };
            let resized =
                image::imageops::resize(&rotated, 320, 256, image::imageops::FilterType::Triangle);
            let mut input = vec![0f32; 3 * 320 * 256];
            for (i, p) in resized.pixels().enumerate() {
                for c in 0..3 {
                    input[c * 320 * 256 + i] = p[2 - c] as f32;
                }
            }
            let outputs = self.session.run(ort::inputs![Tensor::from_array((
                [1usize, 3, 256, 320],
                input
            ))?])?;
            // ORT rc.10 cannot safely expose a null zero-length dynamic output.
            // An empty NMS result is normal on empty/rotated frames.
            if outputs[0]
                .dtype()
                .tensor_shape()
                .is_some_and(|s| s.contains(&0))
            {
                continue;
            }
            let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
            if shape.last() != Some(&7) || data.len() % 7 != 0 {
                bail!("头部模型输出不兼容");
            }
            for row in data.chunks_exact(7) {
                if row.iter().any(|v| !v.is_finite())
                    || row[0] != 0.
                    || row[1] != 1.
                    || row[2] < 0.25
                    || row[2] > 1.
                {
                    continue;
                }
                let a = unrotate([row[3] / 320., row[4] / 256.], turn);
                let b = unrotate([row[5] / 320., row[6] / 256.], turn);
                let bbox = [
                    a[0].min(b[0]).max(0.),
                    a[1].min(b[1]).max(0.),
                    a[0].max(b[0]).min(1.),
                    a[1].max(b[1]).min(1.),
                ];
                if bbox[2] > bbox[0] && bbox[3] > bbox[1] {
                    heads.push(Head {
                        bbox,
                        score: row[2],
                    });
                }
            }
        }
        Ok(suppress(heads))
    }
}
fn context_bounds(b: [f32; 4], width: u32, height: u32, scale: f32) -> Option<[u32; 4]> {
    if width < 8
        || height < 8
        || !scale.is_finite()
        || scale < 1.
        || b.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v))
        || b[2] <= b[0]
        || b[3] <= b[1]
    {
        return None;
    }
    let w = ((b[2] - b[0]) * width as f32 * scale)
        .max((b[3] - b[1]) * height as f32 * scale * 1.25)
        .min(width as f32)
        .min(height as f32 * 1.25)
        .floor() as u32;
    let h = (w as f32 / 1.25).floor() as u32;
    if w < 8 || h < 8 {
        return None;
    }
    let x = (((b[0] + b[2]) * width as f32 - w as f32) / 2.)
        .clamp(0., (width - w) as f32)
        .floor() as u32;
    let y = (((b[1] + b[3]) * height as f32 - h as f32) / 2.)
        .clamp(0., (height - h) as f32)
        .floor() as u32;
    Some([x, y, w, h])
}
fn from_crop(mut head: Head, b: [u32; 4], width: u32, height: u32) -> Head {
    for k in [0, 2] {
        head.bbox[k] = (b[0] as f32 + head.bbox[k] * b[2] as f32) / width as f32;
    }
    for k in [1, 3] {
        head.bbox[k] = (b[1] as f32 + head.bbox[k] * b[3] as f32) / height as f32;
    }
    head
}
fn consistent_refinement(seed: &Head, heads: &[Head]) -> Option<Head> {
    // Multiple strong heads in this crop are ambiguous, even if one overlaps.
    if heads.len() != 1 {
        return None;
    }
    let h = &heads[0];
    (h.score.is_finite()
        && h.score >= crate::occupancy::MIN_HEAD_CONFIDENCE
        && h.bbox
            .iter()
            .all(|v| v.is_finite() && (0. ..=1.).contains(v))
        && iou(seed.bbox, h.bbox) >= 0.45)
        .then(|| h.clone())
}
fn unrotate(p: [f32; 2], turn: usize) -> [f32; 2] {
    match turn {
        1 => [p[1], 1. - p[0]],
        2 => [1. - p[0], 1. - p[1]],
        3 => [1. - p[1], p[0]],
        _ => p,
    }
}
fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let area = |r: [f32; 4]| (r[2] - r[0]).max(0.) * (r[3] - r[1]).max(0.);
    let intersection =
        (a[2].min(b[2]) - a[0].max(b[0])).max(0.) * (a[3].min(b[3]) - a[1].max(b[1])).max(0.);
    intersection / (area(a) + area(b) - intersection).max(1e-6)
}
fn suppress(mut heads: Vec<Head>) -> Vec<Head> {
    heads.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept = Vec::<Head>::new();
    for h in heads {
        if !kept.iter().any(|q| iou(q.bbox, h.bbox) > 0.3) {
            kept.push(h);
        }
    }
    kept
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotations_and_duplicate_heads() {
        let p = [0.2, 0.4];
        for (turn, rotated) in [(1, [0.6, 0.2]), (2, [0.8, 0.6]), (3, [0.4, 0.8])] {
            let q = unrotate(rotated, turn);
            assert!((p[0] - q[0]).abs() < 1e-6 && (p[1] - q[1]).abs() < 1e-6);
        }
        let h = Head {
            bbox: [0.2, 0.2, 0.4, 0.4],
            score: 0.9,
        };
        assert_eq!(suppress(vec![h.clone(), h]).len(), 1);
    }
    #[test]
    fn context_crops_preserve_geometry_at_edges_and_map_back() {
        for b in [
            [0.62, 0.58, 0.72, 0.72],
            [0., 0., 0.1, 0.1],
            [0.9, 0.9, 1., 1.],
        ] {
            let c = context_bounds(b, 1280, 720, 2.5).unwrap();
            assert!(c[0] + c[2] <= 1280 && c[1] + c[3] <= 720);
            assert!((c[2] as f32 / c[3] as f32 - 1.25).abs() < 0.02);
            let mapped = from_crop(
                Head {
                    bbox: [0., 0., 1., 1.],
                    score: 0.9,
                },
                c,
                1280,
                720,
            );
            assert_eq!(mapped.bbox[0], c[0] as f32 / 1280.);
            assert_eq!(mapped.bbox[3], (c[1] + c[3]) as f32 / 720.);
        }
        assert!(context_bounds([0.5, 0.5, 0.4, 0.6], 1280, 720, 2.5).is_none());
    }
    #[test]
    fn refinement_rejects_a_different_head_multiple_or_weak_evidence() {
        let seed = Head {
            bbox: [0.6, 0.5, 0.7, 0.65],
            score: 0.7,
        };
        let strong = Head {
            bbox: seed.bbox,
            score: 0.92,
        };
        assert!(consistent_refinement(&seed, &[strong.clone()]).is_some());
        assert!(consistent_refinement(&seed, &[seed.clone()]).is_none());
        assert!(consistent_refinement(&seed, &[strong.clone(), strong.clone()]).is_none());
        assert!(consistent_refinement(
            &seed,
            &[Head {
                bbox: [0.2, 0.2, 0.3, 0.35],
                score: 0.99
            }]
        )
        .is_none());
        assert!(consistent_refinement(&seed, &[]).is_none());
    }
}
