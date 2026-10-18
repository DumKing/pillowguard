//! Stable upgrade paths and read-only aliases for pre-枕安 installations.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub fn env(name: &str) -> Option<OsString> {
    std::env::var_os(format!("ZHENAN_{name}"))
        .or_else(|| std::env::var_os(format!("LAOREN_{name}")))
}

/// Move the entire data directory together (including credentials and captures).
/// Never merge/overwrite a newer installation. If the move is unavailable,
/// continue using the old directory instead of silently starting from scratch.
pub fn desktop_data(parent: &Path, legacy_name: &str, name: &str) -> PathBuf {
    let current = parent.join(name);
    let legacy = parent.join(legacy_name);
    if !current.exists() && legacy.is_dir() {
        if std::fs::rename(&legacy, &current).is_err() {
            eprintln!("枕安：旧配置目录暂时无法迁移，继续使用原有设置");
            return legacy;
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_environment_names_take_precedence_with_legacy_fallback() {
        let suffix = format!("BRAND_TEST_{}", std::process::id());
        let new = format!("ZHENAN_{suffix}");
        let old = format!("LAOREN_{suffix}");
        assert!(env(&suffix).is_none());
        std::env::set_var(&old, "legacy");
        assert_eq!(env(&suffix), Some("legacy".into()));
        std::env::set_var(&new, "current");
        assert_eq!(env(&suffix), Some("current".into()));
        std::env::remove_var(new);
        std::env::remove_var(old);
    }
    #[test]
    fn migrate_complete_data_without_overwriting_newer_settings() {
        let parent = std::env::temp_dir().join(format!(
            "zhenan-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let old = parent.join("Shouwang");
        std::fs::create_dir_all(&old).unwrap();
        for name in [
            "config.json",
            "credentials.json",
            "history.json",
            "care-alerts.json",
            "alert-captures.json",
        ] {
            std::fs::write(old.join(name), b"existing private data").unwrap();
        }
        let current = desktop_data(&parent, "Shouwang", "枕安");
        assert!(!old.exists());
        for name in [
            "config.json",
            "credentials.json",
            "history.json",
            "care-alerts.json",
            "alert-captures.json",
        ] {
            assert_eq!(
                std::fs::read(current.join(name)).unwrap(),
                b"existing private data"
            );
        }
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.json"), b"older settings").unwrap();
        assert_eq!(desktop_data(&parent, "Shouwang", "枕安"), current);
        assert_eq!(
            std::fs::read(current.join("config.json")).unwrap(),
            b"existing private data"
        );
        assert_eq!(
            std::fs::read(old.join("config.json")).unwrap(),
            b"older settings"
        );
        std::fs::remove_dir_all(parent).unwrap();
    }
}
