use crate::channels::Channel;
use anyhow::{bail, Result};
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub fn validate(c: &Channel) -> Result<()> {
    if c.corp_id.len() > 128
        || !c
            .corp_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("企业 ID 格式无效");
    }
    if !c.agent_id.is_empty() && c.agent_id.parse::<u32>().ok().filter(|v| *v > 0).is_none() {
        bail!("应用 ID 应为正整数");
    }
    for (value, numeric, max) in [
        (&c.touser, false, 1000),
        (&c.toparty, true, 100),
        (&c.totag, true, 100),
    ] {
        if value.is_empty() {
            continue;
        }
        if !numeric && value == "@all" {
            continue;
        }
        if value.len() > 65536
            || value.split('|').count() > max
            || value.split('|').any(|v| {
                v.is_empty()
                    || if numeric {
                        v.parse::<u32>().ok().filter(|n| *n > 0).is_none()
                    } else {
                        v.len() > 64
                            || !v
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_-@.".contains(&b))
                    }
            })
        {
            bail!("接收对象格式无效，请使用 | 分隔 ID");
        }
    }
    if c.secret.len() > 512 {
        bail!("应用 Secret 过长");
    }
    Ok(())
}
#[derive(Clone, Default)]
pub struct Sender {
    cache: Arc<Mutex<HashMap<String, (String, Instant)>>>,
}
fn key(c: &Channel) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", c.corp_id, c.secret).as_bytes())
    )
}
fn error(value: &Value) -> String {
    let Some(code) = value["errcode"].as_i64() else {
        return "企业微信响应缺少 errcode，无法确认结果".into();
    };
    let hint = match code {
        60020 => "请在应用后台配置本软件网络出口的企业可信 IP",
        40013 => "请检查企业 ID",
        40001 => "请检查该自建应用的 Secret",
        40014 | 42001 => "访问凭证失效，已清除缓存；下次通知重新获取，本次未重发",
        81013 | 60111 => "请检查接收成员及应用可见范围",
        _ => "请检查应用配置、接收对象、可见范围和企业可信 IP",
    };
    format!("企业微信拒绝请求，错误码 {code}；{hint}")
}
async fn response(mut r: reqwest::Response) -> std::result::Result<Value, String> {
    if !r.status().is_success() {
        return Err(format!("企业微信请求失败 HTTP {}", r.status().as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = r
        .chunk()
        .await
        .map_err(|_| "读取企业微信响应失败，未自动重试")?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("企业微信响应过大，无法确认结果".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "企业微信响应不是有效 JSON，无法确认结果".into())
}
impl Sender {
    pub async fn send(&self, client: &Client, c: &Channel, message: &str) -> String {
        self.send_at(client, c, message, "https://qyapi.weixin.qq.com")
            .await
    }
    async fn send_at(&self, client: &Client, c: &Channel, message: &str, base: &str) -> String {
        if validate(c).is_err()
            || c.corp_id.is_empty()
            || c.secret.is_empty()
            || c.agent_id.is_empty()
            || (c.touser.is_empty() && c.toparty.is_empty() && c.totag.is_empty())
        {
            return "企业微信应用配置不完整或无效，请检查企业 ID、应用 ID、Secret 和接收对象"
                .into();
        }
        let cache_key = key(c);
        let token = {
            // Serialize token refresh so channels sharing one application reuse it.
            let mut cache = self.cache.lock().await;
            if let Some((token, _)) = cache
                .get(&cache_key)
                .filter(|(_, until)| *until > Instant::now())
            {
                token.clone()
            } else {
                let r = match client
                    .get(format!("{base}/cgi-bin/gettoken"))
                    .query(&[
                        ("corpid", c.corp_id.as_str()),
                        ("corpsecret", c.secret.as_str()),
                    ])
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => return crate::notifications::transport_error(&e),
                };
                let v = match response(r).await {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                if v["errcode"].as_i64() != Some(0) {
                    return error(&v);
                }
                let Some(token) = v["access_token"]
                    .as_str()
                    .filter(|t| !t.is_empty() && t.len() < 8192)
                else {
                    return "企业微信未返回有效访问凭证".into();
                };
                let Some(expires) = v["expires_in"].as_u64().filter(|n| *n > 0 && *n <= 86400)
                else {
                    return "企业微信访问凭证有效期无效".into();
                };
                if cache.len() >= 64 {
                    cache.clear();
                }
                let until = Instant::now() + Duration::from_secs(expires.saturating_sub(60));
                cache.insert(cache_key.clone(), (token.to_owned(), until));
                token.to_owned()
            }
        };
        let mut body = json!({"agentid":c.agent_id.parse::<u32>().unwrap(),"msgtype":"text","text":{"content":message},"safe":0});
        for (field, value) in [
            ("touser", &c.touser),
            ("toparty", &c.toparty),
            ("totag", &c.totag),
        ] {
            if !value.is_empty() {
                body[field] = json!(value);
            }
        }
        let r = match client
            .post(format!("{base}/cgi-bin/message/send"))
            .query(&[("access_token", token)])
            .json(&body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return crate::notifications::transport_error(&e),
        };
        let v = match response(r).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        if matches!(v["errcode"].as_i64(), Some(40014 | 42001)) {
            self.cache.lock().await.remove(&cache_key);
        }
        if v["errcode"].as_i64() != Some(0) {
            return error(&v);
        }
        if [
            "invaliduser",
            "invalidparty",
            "invalidtag",
            "unlicenseduser",
        ]
        .iter()
        .any(|f| {
            v.get(f).is_some_and(|value| match value {
                Value::Null => false,
                Value::String(s) => !s.is_empty(),
                Value::Array(a) => !a.is_empty(),
                _ => true,
            })
        }) {
            return "企业微信已处理请求，但部分接收对象无效、无权限或未获许可；请检查接收对象和应用可见范围，无法确认全部送达".into();
        }
        "企业微信应用已接受消息，请在接收成员的应用消息中核实".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::HeaderMap,
        routing::{get, post},
        Json, Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn cache_payload_partial_failure_and_token_invalidation() {
        let gets = Arc::new(AtomicUsize::new(0));
        let sends = Arc::new(AtomicUsize::new(0));
        let router = Router::new()
            .route(
                "/cgi-bin/gettoken",
                get({
                    let gets = gets.clone();
                    move |headers: HeaderMap| {
                        let gets = gets.clone();
                        async move {
                            assert!(!headers.contains_key("authorization"));
                            gets.fetch_add(1, Ordering::SeqCst);
                            Json(json!({"errcode":0,"access_token":"mock-token","expires_in":7200}))
                        }
                    }
                }),
            )
            .route(
                "/cgi-bin/message/send",
                post(
                    |State(sends): State<Arc<AtomicUsize>>,
                     headers: HeaderMap,
                     Json(body): Json<Value>| async move {
                        assert!(!headers.contains_key("authorization"));
                        assert_eq!(body["agentid"], 1001);
                        assert_eq!(body["touser"], "alice|bob");
                        assert_eq!(body["toparty"], "2");
                        assert_eq!(body["totag"], "3");
                        assert_eq!(body["text"]["content"], "坐起提醒");
                        let n = sends.fetch_add(1, Ordering::SeqCst);
                        Json(match n {
                            0 => json!({"errcode":0}),
                            1 => json!({"errcode":0,"invaliduser":"bob"}),
                            2 => json!({"errcode":42001,"errmsg":"secret-must-not-leak"}),
                            _ => json!({"errcode":0}),
                        })
                    },
                ),
            )
            .with_state(sends.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
        let c = Channel {
            kind: "wecom_app".into(),
            enabled: true,
            corp_id: "wwtest".into(),
            agent_id: "1001".into(),
            secret: "mock-secret".into(),
            touser: "alice|bob".into(),
            toparty: "2".into(),
            totag: "3".into(),
            ..Default::default()
        };
        let sender = Sender::default();
        let client = Client::builder().no_proxy().build().unwrap();
        assert!(sender
            .send_at(&client, &c, "坐起提醒", &base)
            .await
            .contains("已接受"));
        assert!(sender
            .send_at(&client, &c, "坐起提醒", &base)
            .await
            .contains("部分接收对象"));
        let rejected = sender.send_at(&client, &c, "坐起提醒", &base).await;
        assert!(rejected.contains("42001"));
        assert!(!rejected.contains("secret-must-not-leak"));
        assert_eq!(gets.load(Ordering::SeqCst), 1);
        assert!(sender
            .send_at(&client, &c, "坐起提醒", &base)
            .await
            .contains("已接受"));
        assert_eq!(gets.load(Ordering::SeqCst), 2);
        assert_eq!(sends.load(Ordering::SeqCst), 4);
        server.abort();
    }
}
