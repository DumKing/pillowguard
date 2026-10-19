//! Five alarm groups, each retaining a bounded sequence of analysis frames.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::VecDeque, fs, io::Write, path::PathBuf};

pub const MAX_FRAMES: usize = 20;
#[derive(Clone, Serialize, Deserialize)]
pub struct Frame {
    time: String,
    pose: String,
    jpeg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<Value>,
}
impl Frame {
    pub fn new(timestamp: f64, jpeg: &[u8], evidence: Option<Value>) -> Self {
        let time = chrono::DateTime::from_timestamp_millis((timestamp * 1000.) as i64)
            .map(|t| {
                t.with_timezone(&chrono_tz::Asia::Shanghai)
                    .format("%H:%M:%S%.3f")
                    .to_string()
            })
            .unwrap_or_else(|| "时间未知".into());
        Self {
            time,
            pose: evidence
                .as_ref()
                .and_then(|v| v["pose"].as_str())
                .unwrap_or("unknown")
                .into(),
            jpeg: if jpeg.len() <= 256_000 {
                STANDARD.encode(jpeg)
            } else {
                String::new()
            },
            evidence,
        }
    }
    fn metadata(&self) -> Value {
        json!({"time":self.time,"pose":self.pose,"has_image":!self.jpeg.is_empty()})
    }
}
#[derive(Default)]
pub struct Recent {
    frames: VecDeque<(f64, Frame)>,
}
impl Recent {
    pub fn clear(&mut self) {
        self.frames.clear();
    }
    pub fn push(&mut self, now: f64, frame: Frame, limit: usize) {
        if !now.is_finite() {
            self.clear();
            return;
        }
        if self
            .frames
            .back()
            .is_some_and(|(t, _)| now <= *t || now - t > 3.)
        {
            self.clear();
        }
        self.frames.push_back((now, frame));
        while self.frames.len() > limit.clamp(1, MAX_FRAMES) {
            self.frames.pop_front();
        }
    }
    pub fn snapshot(&self, now: f64) -> Vec<Frame> {
        if !self
            .frames
            .back()
            .is_some_and(|(t, _)| now.is_finite() && now >= *t && now - t <= 2.)
        {
            return vec![];
        }
        self.frames.iter().map(|(_, f)| f.clone()).collect()
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Capture {
    id: String,
    time: String,
    label: String,
    demo: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    jpeg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<Value>,
    #[serde(default)]
    frames: Vec<Frame>,
}
pub struct Store {
    path: PathBuf,
    entries: Vec<Capture>,
    pub status: String,
}
impl Store {
    pub fn load(path: PathBuf) -> Self {
        let result = if path.exists() {
            fs::metadata(&path)
                .ok()
                .filter(|m| m.len() <= 50_000_000)
                .and_then(|_| fs::read(&path).ok())
                .and_then(|b| serde_json::from_slice::<Vec<Capture>>(&b).ok())
        } else {
            Some(vec![])
        };
        let valid = result.is_some();
        let mut entries = result.unwrap_or_default();
        entries.truncate(5);
        for c in &mut entries {
            c.frames.truncate(MAX_FRAMES);
        }
        Self {
            path,
            entries,
            status: if valid {
                "告警画面保存在本机，保留最近 5 组"
            } else {
                "告警画面读取失败，原文件已保留；下次告警时重新保存"
            }
            .into(),
        }
    }
    pub fn metadata(&self) -> Value {
        json!(self.entries.iter().map(|c| {
            let frames: Vec<Value> = if c.frames.is_empty() && !c.jpeg.is_empty() {
                vec![json!({"time":c.time,"pose":c.evidence.as_ref().and_then(|v| v["pose"].as_str()).unwrap_or("unknown"),"has_image":true})]
            } else { c.frames.iter().map(Frame::metadata).collect() };
            json!({"id":c.id,"time":c.time,"label":c.label,"demo":c.demo,
                "has_image":!c.jpeg.is_empty() || c.frames.iter().any(|f| !f.jpeg.is_empty()),"frame_count":frames.len(),
                "legacy":c.frames.is_empty() && !c.jpeg.is_empty(),"frames":frames})
        }).collect::<Vec<_>>())
    }
    pub fn image(&self, id: &str) -> Option<Vec<u8>> {
        let c = self.entries.iter().find(|c| c.id == id)?;
        let jpeg = if c.frames.is_empty() {
            &c.jpeg
        } else {
            &c.frames.iter().rev().find(|f| !f.jpeg.is_empty())?.jpeg
        };
        if jpeg.is_empty() {
            return None;
        }
        STANDARD.decode(jpeg).ok()
    }
    pub fn frame_image(&self, id: &str, index: usize) -> Option<Vec<u8>> {
        let c = self.entries.iter().find(|c| c.id == id)?;
        let jpeg = if c.frames.is_empty() {
            if index != 0 {
                return None;
            }
            &c.jpeg
        } else {
            &c.frames.get(index)?.jpeg
        };
        if jpeg.is_empty() {
            return None;
        }
        STANDARD.decode(jpeg).ok()
    }
    pub fn record(
        &mut self,
        id: String,
        time: String,
        label: String,
        demo: bool,
        mut frames: Vec<Frame>,
    ) {
        if self.entries.iter().any(|c| c.id == id) {
            return;
        }
        if demo {
            frames.clear();
        }
        if frames.len() > MAX_FRAMES {
            frames.drain(..frames.len() - MAX_FRAMES);
        }
        self.entries.insert(
            0,
            Capture {
                id,
                time,
                label,
                demo,
                jpeg: String::new(),
                evidence: None,
                frames,
            },
        );
        self.entries.truncate(5);
        let temp = self.path.with_extension("tmp");
        let result = (|| -> anyhow::Result<()> {
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(&serde_json::to_vec(&self.entries)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, &self.path)?;
            Ok(())
        })();
        self.status = if result.is_ok() {
            "告警画面保存在本机，保留最近 5 组"
        } else {
            let _ = fs::remove_file(temp);
            "告警画面保存失败，当前仅在内存中可查看"
        }
        .into();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn frame(n: u8) -> Frame {
        Frame::new(
            100. + n as f64 / 2.,
            &[n],
            Some(json!({"pose":if n == 2 { "unknown" } else { "upright" }})),
        )
    }
    #[test]
    fn recent_keeps_order_unknown_and_trigger_frame_and_clears_gaps() {
        let mut r = Recent::default();
        for n in 0..7 {
            r.push(100. + n as f64 / 2., frame(n), 5);
        }
        let f = r.snapshot(103.);
        assert_eq!(f.len(), 5);
        assert_eq!(f[0].pose, "unknown");
        assert_eq!(STANDARD.decode(&f[4].jpeg).unwrap(), vec![6]);
        assert!(r.snapshot(106.).is_empty());
        r.push(110., frame(7), 5);
        assert_eq!(r.snapshot(110.).len(), 1);
        r.clear();
        assert!(r.snapshot(110.).is_empty());
        r.push(111., frame(0), 5);
        r.push(109., frame(1), 5);
        assert_eq!(r.snapshot(109.).len(), 1);
    }
    #[test]
    fn groups_restart_eviction_and_legacy_single_frame() {
        let dir = std::env::temp_dir().join(format!(
            "captures-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("captures.json");
        fs::write(&path, serde_json::to_vec(&json!([{"id":"old","time":"time","label":"坐起","demo":false,"jpeg":STANDARD.encode([99])}])).unwrap()).unwrap();
        let mut s = Store::load(path.clone());
        assert_eq!(s.metadata()[0]["frame_count"], 1);
        assert_eq!(s.frame_image("old", 0), Some(vec![99]));
        assert_eq!(s.metadata()[0]["legacy"], true);
        for n in 0..6 {
            s.record(
                n.to_string(),
                "time".into(),
                "坐起".into(),
                false,
                (0..5).map(frame).collect(),
            );
        }
        s.record(
            "5".into(),
            "time".into(),
            "重复".into(),
            false,
            vec![frame(9)],
        );
        let mut s = Store::load(path.clone());
        assert_eq!(s.metadata().as_array().unwrap().len(), 5);
        assert!(s.image("old").is_none());
        assert_eq!(s.metadata()[0]["frame_count"], 5);
        assert_eq!(s.frame_image("5", 2), Some(vec![2]));
        assert_eq!(s.metadata()[0]["frames"][2]["pose"], "unknown");
        assert_eq!(s.image("5"), Some(vec![4]));
        assert!(s.frame_image("5", 5).is_none());
        assert!(s.frame_image("../captures.json", 0).is_none());
        s.record(
            "demo".into(),
            "time".into(),
            "演示".into(),
            true,
            vec![frame(1)],
        );
        assert_eq!(s.metadata()[0]["frame_count"], 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
