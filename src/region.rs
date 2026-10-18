use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

// Existing rectangle configurations remain readable; new edits save vertices.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Region {
    Rect([f32; 4]),
    Polygon(Vec<[f32; 2]>),
}
pub trait RegionLike {
    fn vertices(&self) -> Cow<'_, [[f32; 2]]>;
}
impl RegionLike for [f32; 4] {
    fn vertices(&self) -> Cow<'_, [[f32; 2]]> {
        let [x, y, w, h] = *self;
        Cow::Owned(vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]])
    }
}
impl RegionLike for Region {
    fn vertices(&self) -> Cow<'_, [[f32; 2]]> {
        match self {
            Self::Rect(r) => r.vertices(),
            Self::Polygon(p) => Cow::Borrowed(p),
        }
    }
}
impl<T: RegionLike + ?Sized> RegionLike for &T {
    fn vertices(&self) -> Cow<'_, [[f32; 2]]> {
        (**self).vertices()
    }
}
impl From<[f32; 4]> for Region {
    fn from(r: [f32; 4]) -> Self {
        Self::Rect(r)
    }
}
impl PartialEq for Region {
    fn eq(&self, other: &Self) -> bool {
        let a = self.vertices();
        let b = other.vertices();
        a.len() == b.len()
            && a.iter()
                .zip(b.iter())
                .all(|(a, b)| (0..2).all(|k| (a[k] - b[k]).abs() < 1e-6))
    }
}
fn cross(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}
fn on(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> bool {
    cross(a, b, p).abs() < 1e-7
        && (0..2).all(|k| p[k] >= a[k].min(b[k]) - 1e-7 && p[k] <= a[k].max(b[k]) + 1e-7)
}
fn intersects(a: [f32; 2], b: [f32; 2], c: [f32; 2], d: [f32; 2]) -> bool {
    cross(a, b, c) * cross(a, b, d) < 0. && cross(c, d, a) * cross(c, d, b) < 0.
        || on(a, b, c)
        || on(a, b, d)
        || on(c, d, a)
        || on(c, d, b)
}
fn area(p: &[[f32; 2]]) -> f32 {
    if p.len() < 3 {
        return 0.;
    }
    (0..p.len())
        .map(|k| {
            let a = p[k];
            let b = p[(k + 1) % p.len()];
            a[0] * b[1] - a[1] * b[0]
        })
        .sum::<f32>()
        .abs()
        / 2.
}
impl Region {
    pub fn validate(&self) -> Result<()> {
        let p = self.vertices();
        let n = p.len();
        if !(3..=12).contains(&n)
            || p.iter()
                .flatten()
                .any(|v| !v.is_finite() || !(0. ..=1.001).contains(v))
        {
            bail!("区域需要画面内的 3 至 12 个顶点");
        }
        if let Self::Rect([_, _, w, h]) = self {
            if *w < 0.05 || *h < 0.05 {
                bail!("区域过小");
            }
        }
        let (minx, maxx, miny, maxy) =
            p.iter().fold((1f32, 0f32, 1f32, 0f32), |(a, b, c, d), v| {
                (a.min(v[0]), b.max(v[0]), c.min(v[1]), d.max(v[1]))
            });
        if maxx - minx < 0.05 || maxy - miny < 0.05 || area(&p) < 0.0025 {
            bail!("区域面积过小");
        }
        for i in 0..n {
            let a = p[i];
            let b = p[(i + 1) % n];
            if (a[0] - b[0]).hypot(a[1] - b[1]) < 0.001 {
                bail!("区域顶点过近或重复");
            }
            // Adjacent collinear edges may continue, but must not double back.
            let c = p[(i + 2) % n];
            if cross(a, b, c).abs() < 1e-7
                && ((a[0] - b[0]) * (c[0] - b[0]) + (a[1] - b[1]) * (c[1] - b[1])) > 0.
            {
                bail!("区域边界不能重叠");
            }
            for j in i + 1..n {
                if j == i + 1 || (i == 0 && j == n - 1) {
                    continue;
                }
                if intersects(a, b, p[j], p[(j + 1) % n]) {
                    bail!("区域边界不能交叉或重叠");
                }
            }
        }
        Ok(())
    }
}
// Clip the polygon against the actual person box. Do not substitute the
// polygon's bounding rectangle: its empty corners can contain another person.
pub fn overlap(region: impl RegionLike, b: [f32; 4]) -> f32 {
    if b.iter().any(|v| !v.is_finite()) || b[2] <= b[0] || b[3] <= b[1] {
        return 0.;
    }
    let mut p = region.vertices().into_owned();
    for (axis, bound, greater) in [
        (0, b[0], true),
        (0, b[2], false),
        (1, b[1], true),
        (1, b[3], false),
    ] {
        if p.is_empty() {
            return 0.;
        }
        let mut out = Vec::new();
        let mut a = *p.last().unwrap();
        let inside = |v: [f32; 2]| {
            if greater {
                v[axis] >= bound
            } else {
                v[axis] <= bound
            }
        };
        for &v in &p {
            let ia = inside(a);
            let iv = inside(v);
            if ia != iv {
                let t = (bound - a[axis]) / (v[axis] - a[axis]);
                let mut q = [a[0] + t * (v[0] - a[0]), a[1] + t * (v[1] - a[1])];
                q[axis] = bound;
                out.push(q);
            }
            if iv {
                out.push(v)
            }
            a = v;
        }
        p = out;
    }
    area(&p) / ((b[2] - b[0]) * (b[3] - b[1]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn perspective_corner_and_concave_intersections() {
        let r = Region::Polygon(vec![[0.4, 0.1], [0.6, 0.1], [0.9, 0.9], [0.1, 0.9]]);
        assert!(r.validate().is_ok());
        assert_eq!(overlap(&r, [0.1, 0.1, 0.25, 0.3]), 0.);
        assert!((overlap(&r, [0.45, 0.3, 0.55, 0.8]) - 1.).abs() < 1e-5);
        let l = Region::Polygon(vec![
            [0., 0.],
            [1., 0.],
            [1., 0.3],
            [0.3, 0.3],
            [0.3, 1.],
            [0., 1.],
        ]);
        assert!(l.validate().is_ok());
        assert!((overlap(&l, [0., 0., 1., 1.]) - 0.51).abs() < 1e-5);
        assert_eq!(overlap(&l, [0.4, 0.4, 0.9, 0.9]), 0.);
        let reverse = Region::Polygon(l.vertices().iter().rev().copied().collect());
        assert!((overlap(&reverse, [0., 0., 1., 1.]) - 0.51).abs() < 1e-5);
    }
    #[test]
    fn legacy_and_invalid_geometry() {
        let old: Region = serde_json::from_str("[0.1,0.2,0.8,0.7]").unwrap();
        assert!(old.validate().is_ok());
        let new = Region::Polygon(old.vertices().into_owned());
        assert_eq!(old, new);
        for p in [
            vec![[0., 0.], [1., 1.], [0., 1.], [1., 0.]],
            vec![[0., 0.], [1., 0.], [1., 0.]],
            vec![[0., 0.], [0.001, 0.], [0., 0.001]],
            vec![[0., 0.], [1.1, 0.], [1., 1.]],
        ] {
            assert!(Region::Polygon(p).validate().is_err());
        }
    }
}
