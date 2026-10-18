//! Five most recent alarm events, persisted atomically with their local JPEGs.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fs, io::Write, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
struct Capture {
    id: String,
    time: String,
    label: String,
    demo: bool,
    jpeg: String,
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
                .filter(|m| m.len() <= 10_000_000)
                .and_then(|_| fs::read(&path).ok())
                .and_then(|b| serde_json::from_slice::<Vec<Capture>>(&b).ok())
        } else {
            Some(vec![])
        };
        let valid = result.is_some();
        let mut entries = result.unwrap_or_default();
        entries.truncate(5);
        Self {
            path,
            entries,
            status: if valid {
                "告警画面保存在本机，保留最近 5 次"
            } else {
                "告警画面读取失败，原文件已保留；下次告警时重新保存"
            }
            .into(),
        }
    }
    pub fn metadata(&self) -> Value {
        json!(self.entries.iter().map(|c| json!({"id":c.id,"time":c.time,"label":c.label,"demo":c.demo,"has_image":!c.jpeg.is_empty()})).collect::<Vec<_>>())
    }
    pub fn image(&self, id: &str) -> Option<Vec<u8>> {
        let c = self
            .entries
            .iter()
            .find(|c| c.id == id && !c.jpeg.is_empty())?;
        STANDARD.decode(&c.jpeg).ok()
    }
    pub fn record(&mut self, id: String, time: String, label: String, demo: bool, jpeg: &[u8]) {
        if self.entries.iter().any(|c| c.id == id) {
            return;
        }
        self.entries.insert(
            0,
            Capture {
                id,
                time,
                label,
                demo,
                jpeg: if jpeg.len() <= 1_000_000 {
                    STANDARD.encode(jpeg)
                } else {
                    String::new()
                },
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
            "告警画面保存在本机，保留最近 5 次"
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
    #[test]
    fn retain_five_restart_eviction_missing_and_duplicate() {
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
        let mut store = Store::load(path.clone());
        for n in 0..7 {
            store.record(n.to_string(), "time".into(), "坐起".into(), false, &[n]);
        }
        store.record("6".into(), "time".into(), "repeat".into(), false, &[99]);
        let mut store = Store::load(path.clone());
        assert_eq!(store.metadata().as_array().unwrap().len(), 5);
        assert!(store.image("1").is_none());
        assert_eq!(store.image("6"), Some(vec![6]));
        store.record("lost".into(), "time".into(), "断流".into(), false, &[]);
        assert_eq!(store.metadata()[0]["has_image"], false);
        assert!(store.image("../captures.json").is_none());
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
