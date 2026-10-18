use anyhow::{bail, Result};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
pub const LIMIT: usize = 500;
pub fn load(path: &Path) -> Result<Vec<crate::Event>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    if fs::metadata(path)?.len() > 2_000_000 {
        bail!("history too large");
    }
    let mut events: Vec<crate::Event> = serde_json::from_slice(&fs::read(path)?)?;
    events.truncate(LIMIT);
    Ok(events)
}
pub fn save(path: &Path, events: &[crate::Event]) -> Result<()> {
    let temp = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(&serde_json::to_vec(events)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reload_and_corruption() {
        let dir = std::env::temp_dir().join(format!(
            "zhenan-history-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        assert!(load(&path).unwrap().is_empty());
        save(
            &path,
            &[crate::Event {
                date: "2026-10-18".into(),
                time: "09:00:00".into(),
                text: "多人暂停".into(),
            }],
        )
        .unwrap();
        assert_eq!(load(&path).unwrap()[0].text, "多人暂停");
        fs::write(&path, b"invalid").unwrap();
        assert!(load(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
