//! Local credentials and push-channel persistence, separate from state polling.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    pub push_channels: Option<Vec<crate::channels::Channel>>,
    pub token: String,
    pub webhook: String,
    pub wecom: String,
    pub dingtalk: String,
    pub dingtalk_secret: String,
    pub camera_source: String,
    pub replay_source: String,
}
pub fn load_all(dir: &Path) -> Result<Credentials> {
    let path = dir.join("credentials.json");
    if !path.exists() {
        return Ok(Credentials::default());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(serde_json::from_slice::<Credentials>(&fs::read(path)?)?)
}
#[cfg(test)]
pub fn load(dir: &Path) -> Result<String> {
    Ok(load_all(dir)?.token)
}
pub fn save(dir: &Path, token: &str) -> Result<()> {
    let mut saved = load_all(dir)?;
    saved.token = token.into();
    save_all(dir, &saved)
}
pub fn save_all(dir: &Path, saved: &Credentials) -> Result<()> {
    let path = dir.join("credentials.json");
    if [
        &saved.token,
        &saved.webhook,
        &saved.wecom,
        &saved.dingtalk,
        &saved.dingtalk_secret,
        &saved.camera_source,
        &saved.replay_source,
    ]
    .iter()
    .all(|v| v.is_empty())
        && saved.push_channels.is_none()
    {
        match fs::remove_file(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(
        ".credentials-{}-{}.tmp",
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
        file.write_all(&serde_json::to_vec(saved)?)?;
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
    fn survives_reload_replace_and_clear() {
        let path = std::env::temp_dir().join(format!(
            "pillowguard-credentials-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(load(&path).unwrap(), "");
        save(&path, "dummy-first-token").unwrap();
        assert_eq!(load(&path).unwrap(), "dummy-first-token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path.join("credentials.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        save(&path, "dummy-replacement-token").unwrap();
        assert_eq!(load(&path).unwrap(), "dummy-replacement-token");
        assert!(!path.join("config.json").exists());
        save(&path, "").unwrap();
        assert_eq!(load(&path).unwrap(), "");
        save(&path, "").unwrap();
        fs::write(
            path.join("credentials.json"),
            br#"{"token":"legacy-dummy"}"#,
        )
        .unwrap();
        let mut all = load_all(&path).unwrap();
        assert_eq!(all.token, "legacy-dummy");
        all.webhook = "https://example.com/hook".into();
        all.wecom = "https://example.com/wecom".into();
        all.dingtalk = "https://example.com/ding".into();
        all.dingtalk_secret = "dummy-secret".into();
        all.camera_source = "rtsp://dummy:pass@camera.local/video".into();
        all.replay_source = "/tmp/dummy.mp4".into();
        all.push_channels = Some(vec![crate::channels::Channel {
            id: "test-route".into(),
            name: "测试渠道".into(),
            kind: "webhook".into(),
            enabled: true,
            url: "https://example.com/channel".into(),
            secret: "dummy-route-secret".into(),
            ..Default::default()
        }]);
        all.push_channels
            .as_mut()
            .unwrap()
            .push(crate::channels::Channel {
                id: "test-speaker".into(),
                name: "音箱".into(),
                kind: "speaker".into(),
                enabled: true,
                speaker_playback: crate::speaker::Playback {
                    default_count: 2,
                    windows: vec![crate::speaker::Window {
                        start: "22:00".into(),
                        end: "07:00".into(),
                        count: 4,
                    }],
                },
                ..Default::default()
            });
        save_all(&path, &all).unwrap();
        let restored = load_all(&path).unwrap();
        assert_eq!(
            serde_json::to_value(&all).unwrap(),
            serde_json::to_value(&restored).unwrap()
        );
        save(&path, "").unwrap();
        let cleared = load_all(&path).unwrap();
        assert!(cleared.token.is_empty());
        assert_eq!(cleared.camera_source, all.camera_source);
        assert_eq!(cleared.dingtalk_secret, all.dingtalk_secret);
        assert_eq!(
            cleared.push_channels.unwrap()[0].secret,
            "dummy-route-secret"
        );
        save_all(&path, &Credentials::default()).unwrap();
        fs::remove_dir(path).unwrap();
    }
}
