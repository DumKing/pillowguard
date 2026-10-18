use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub url: String,
    pub secret: String,
    pub corp_id: String,
    pub agent_id: String,
    pub touser: String,
    pub toparty: String,
    pub totag: String,
}
impl Channel {
    pub fn public(&self) -> Value {
        json!({"id":self.id,"name":self.name,"kind":self.kind,"enabled":self.enabled,
            "has_url":!self.url.is_empty(),"has_secret":!self.secret.is_empty(),
            "corp_id":self.corp_id,"agent_id":self.agent_id,"touser":self.touser,"toparty":self.toparty,"totag":self.totag})
    }
}
#[derive(Deserialize)]
#[serde(default)]
struct Edit {
    id: String,
    name: String,
    kind: String,
    enabled: bool,
    url: String,
    secret: String,
    corp_id: Option<String>,
    agent_id: Option<String>,
    touser: Option<String>,
    toparty: Option<String>,
    totag: Option<String>,

    clear_url: bool,
    clear_secret: bool,
}
impl Default for Edit {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: String::new(),
            enabled: true,
            url: String::new(),
            secret: String::new(),
            corp_id: None,
            agent_id: None,
            touser: None,
            toparty: None,
            totag: None,

            clear_url: false,
            clear_secret: false,
        }
    }
}
pub fn merge(value: &Value, previous: &[Channel]) -> Result<Vec<Channel>> {
    let edits: Vec<Edit> = serde_json::from_value(value.clone())?;
    if edits.len() > 32 {
        bail!("最多添加 32 个推送渠道");
    }
    let mut result = Vec::<Channel>::new();
    for edit in edits {
        if edit.id.is_empty()
            || edit.id.len() > 80
            || !edit
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || result.iter().any(|c| c.id == edit.id)
        {
            bail!("渠道标识无效或重复");
        }
        if !["speaker", "ha", "webhook", "wecom", "dingtalk", "wecom_app"]
            .contains(&edit.kind.as_str())
        {
            bail!("渠道类型无效");
        }
        let name = edit.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
            bail!("渠道名称应为 1 至 60 字");
        }
        let old = previous
            .iter()
            .find(|c| c.id == edit.id && c.kind == edit.kind);
        let mut channel = Channel {
            id: edit.id,
            name,
            kind: edit.kind,
            enabled: edit.enabled,
            url: old.map(|c| c.url.clone()).unwrap_or_default(),
            secret: old.map(|c| c.secret.clone()).unwrap_or_default(),
            corp_id: edit
                .corp_id
                .map(|s| s.trim().to_owned())
                .unwrap_or_else(|| old.map(|c| c.corp_id.clone()).unwrap_or_default()),
            agent_id: edit
                .agent_id
                .map(|s| s.trim().to_owned())
                .unwrap_or_else(|| old.map(|c| c.agent_id.clone()).unwrap_or_default()),
            touser: edit
                .touser
                .map(|s| s.trim().to_owned())
                .unwrap_or_else(|| old.map(|c| c.touser.clone()).unwrap_or_default()),
            toparty: edit
                .toparty
                .map(|s| s.trim().to_owned())
                .unwrap_or_else(|| old.map(|c| c.toparty.clone()).unwrap_or_default()),
            totag: edit
                .totag
                .map(|s| s.trim().to_owned())
                .unwrap_or_else(|| old.map(|c| c.totag.clone()).unwrap_or_default()),
        };
        if edit.clear_url {
            channel.url.clear();
        } else if !edit.url.trim().is_empty() {
            channel.url = edit.url.trim().into();
        }
        if edit.clear_secret {
            channel.secret.clear();
        } else if !edit.secret.trim().is_empty() {
            channel.secret = edit.secret.trim().into();
        }
        if channel.kind == "wecom_app" {
            channel.url.clear();
            crate::wecom_app::validate(&channel)?;
        }
        if !channel.url.is_empty() && !crate::notifications::valid_url(&channel.kind, &channel.url)
        {
            bail!("渠道地址格式不正确");
        }
        if channel.secret.chars().any(char::is_whitespace) {
            bail!("加签密钥不能含空白");
        }
        if ["speaker", "ha"].contains(&channel.kind.as_str()) {
            channel.url.clear();
            channel.secret.clear();
        }
        result.push(channel);
    }
    Ok(result)
}
pub fn migrate(saved: &crate::credentials::Credentials, selected: &[String]) -> Vec<Channel> {
    if let Some(channels) = &saved.push_channels {
        return channels.clone();
    }
    let names = [
        ("speaker", "小爱音箱", ""),
        ("ha", "HA 站内通知", ""),
        ("wecom", "企业微信群", saved.wecom.as_str()),
        ("dingtalk", "钉钉群", saved.dingtalk.as_str()),
        ("webhook", "自定义 Webhook", saved.webhook.as_str()),
    ];
    names
        .into_iter()
        .filter(|(kind, _, url)| selected.iter().any(|v| v == kind) || !url.is_empty())
        .map(|(kind, name, url)| Channel {
            id: format!("legacy-{kind}"),
            name: name.into(),
            kind: kind.into(),
            enabled: selected.iter().any(|v| v == kind),
            url: url.into(),
            secret: if kind == "dingtalk" || kind == "webhook" {
                saved.dingtalk_secret.clone()
            } else {
                String::new()
            },
            ..Default::default()
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiple_channels_keep_secrets_private_and_support_clear_and_remove() {
        let old=merge(&json!([{"id":"a","name":"家人群","kind":"webhook","url":"https://example.com/a","secret":"dummy"},{"id":"b","name":"照护群","kind":"webhook","url":"https://example.com/b"}]),&[]).unwrap();
        let public: Vec<_> = old.iter().map(Channel::public).collect();
        assert!(!serde_json::to_string(&public)
            .unwrap()
            .contains("example.com"));
        assert!(!serde_json::to_string(&public).unwrap().contains("dummy"));
        let kept = merge(&json!(public), &old).unwrap();
        assert_eq!(kept[0].secret, "dummy");
        assert_eq!(kept[1].url, old[1].url);
        let cleared=merge(&json!([{"id":"a","name":"家人群","kind":"webhook","clear_url":true,"clear_secret":true}]),&old).unwrap();
        assert_eq!(cleared.len(), 1);
        assert!(cleared[0].url.is_empty() && cleared[0].secret.is_empty());
        assert!(merge(&json!([public[0], public[0]]), &old).is_err());
        assert!(migrate(
            &crate::credentials::Credentials {
                push_channels: Some(vec![]),
                ..Default::default()
            },
            &["speaker".into()]
        )
        .is_empty());
    }
}
