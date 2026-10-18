pub fn transport_error(error: &reqwest::Error) -> String {
    use std::error::Error;
    if error.is_timeout() {
        return "请求超时，结果尚未确认；未自动重试以免重复播报".into();
    }
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if cfg!(target_os = "macos") && io.raw_os_error() == Some(65) {
                return "局域网连接被系统拒绝或不可达（系统错误 65）；请在系统设置 → 隐私与安全性 → 本地网络中允许枕安，然后重新测试".into();
            }
            return format!(
                "网络连接失败（{:?}，系统错误 {}）；未自动重试",
                io.kind(),
                io.raw_os_error()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "未知".into())
            );
        }
        source = cause.source();
    }
    if error.is_connect() {
        "无法建立连接，请检查局域网访问权限、地址及网络；未自动重试".into()
    } else if error.is_request() {
        "请求未能发送，请检查地址与凭据格式；未自动重试".into()
    } else {
        "网络请求失败，未自动重试".into()
    }
}
use base64::Engine;
use hmac::{Hmac, Mac};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use sha2::Sha256;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Generic,
    Wecom,
    Dingtalk,
}
pub fn kind(channel: &str, url: &str) -> Kind {
    match channel {
        "wecom" => Kind::Wecom,
        "dingtalk" => Kind::Dingtalk,
        _ => match Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .as_deref()
        {
            Some("qyapi.weixin.qq.com") => Kind::Wecom,
            Some("oapi.dingtalk.com") => Kind::Dingtalk,
            _ => Kind::Generic,
        },
    }
}
pub fn valid_url(channel: &str, input: &str) -> bool {
    let Ok(u) = Url::parse(input) else {
        return false;
    };
    if !["http", "https"].contains(&u.scheme())
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || input.chars().any(char::is_whitespace)
    {
        return false;
    }
    match kind(channel, input) {
        Kind::Wecom => {
            u.scheme() == "https"
                && u.host_str() == Some("qyapi.weixin.qq.com")
                && u.path() == "/cgi-bin/webhook/send"
                && u.query_pairs().any(|(k, v)| k == "key" && !v.is_empty())
        }
        Kind::Dingtalk => {
            u.scheme() == "https"
                && u.host_str() == Some("oapi.dingtalk.com")
                && u.path() == "/robot/send"
                && u.query_pairs()
                    .any(|(k, v)| k == "access_token" && !v.is_empty())
        }
        Kind::Generic => true,
    }
}
pub fn payload(kind: Kind, message: &str) -> Value {
    match kind {
        Kind::Wecom | Kind::Dingtalk => json!({"msgtype":"text","text":{"content":message}}),
        Kind::Generic => {
            json!({"event":"rise_alert","title":"老人起身提醒","message":message,"time":chrono::Utc::now().to_rfc3339()})
        }
    }
}
pub fn signed_url(input: &str, secret: &str, timestamp: i64) -> Result<Url, ()> {
    let mut url = Url::parse(input).map_err(|_| ())?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| ())?;
    mac.update(format!("{timestamp}\n{secret}").as_bytes());
    let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(k, _)| k != "timestamp" && k != "sign")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair("timestamp", &timestamp.to_string())
        .append_pair("sign", &signature);
    Ok(url)
}
pub fn vendor_result(value: &Value) -> String {
    match value.get("errcode").and_then(Value::as_i64) {
        Some(0) => "平台已确认接收，请核实群内消息".into(),
        Some(code) => {
            format!("平台拒绝推送，错误码 {code}；请检查机器人地址、安全关键词、加签或 IP 白名单")
        }
        None => "平台响应缺少 errcode，无法确认推送成功".into(),
    }
}
pub async fn send(
    client: &Client,
    channel: &str,
    url: &str,
    secret: &str,
    message: &str,
    event: &str,
) -> String {
    if url.is_empty() {
        return "未配置机器人或 Webhook 地址".into();
    }
    let mode = kind(channel, url);
    let target = if mode == Kind::Dingtalk && !secret.is_empty() {
        match signed_url(url, secret, chrono::Utc::now().timestamp_millis()) {
            Ok(u) => u,
            Err(_) => return "机器人地址或签名配置无效".into(),
        }
    } else {
        match Url::parse(url) {
            Ok(u) => u,
            Err(_) => return "Webhook 地址无效".into(),
        }
    };
    let mut body = payload(mode, message);
    if mode == Kind::Generic {
        body["event"] = json!(event);
        if event == "notification_test" {
            body["title"] = json!("枕安通知测试");
        }
    }
    // No HA Authorization header is ever attached to robot/webhook requests.
    let mut response = match client.post(target).json(&body).send().await {
        Ok(r) => r,
        Err(e) => return transport_error(&e),
    };
    if !response.status().is_success() {
        return format!("失败 HTTP {}", response.status().as_u16());
    }
    if mode == Kind::Generic {
        return "请求已接受，请核实实际送达".into();
    }
    let mut bytes = vec![];
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() + chunk.len() > 65536 {
                    return "平台响应过大，无法确认结果".into();
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => return "读取平台响应失败，未自动重试".into(),
        }
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => vendor_result(&v),
        Err(_) => "平台响应不是有效 JSON，无法确认结果".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn http_200_platform_rejection_is_failure() {
        use axum::{http::HeaderMap, routing::post, Json, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new().route(
            "/",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert!(headers.get("authorization").is_none());
                assert_eq!(body["msgtype"], "text");
                assert_eq!(body["text"]["content"], "本地测试");
                Json(json!({"errcode":310000,"errmsg":"test rejection"}))
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let client = Client::builder().no_proxy().build().unwrap();
        let result = send(
            &client,
            "wecom",
            &format!("http://{addr}/"),
            "",
            "本地测试",
            "notification_test",
        )
        .await;
        assert!(result.contains("拒绝"));
        assert!(result.contains("310000"));
        assert!(!result.contains("确认接收"));
        server.abort();
    }
    #[test]
    fn formats() {
        let message = "注意，老人已坐起";
        for k in [Kind::Wecom, Kind::Dingtalk] {
            assert_eq!(
                payload(k, message),
                json!({"msgtype":"text","text":{"content":message}})
            );
        }
        assert_eq!(payload(Kind::Generic, message)["message"], message);
    }
    #[test]
    fn auto_detect() {
        assert_eq!(
            kind(
                "webhook",
                "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=test"
            ),
            Kind::Wecom
        );
        assert_eq!(
            kind(
                "webhook",
                "https://oapi.dingtalk.com/robot/send?access_token=test"
            ),
            Kind::Dingtalk
        );
        assert_eq!(
            kind("webhook", "https://qyapi.weixin.qq.com.evil.test/"),
            Kind::Generic
        );
    }
    #[test]
    fn vendor_errors_are_not_success() {
        assert!(vendor_result(&json!({"errcode":0})).contains("确认接收"));
        assert!(
            vendor_result(&json!({"errcode":310000,"errmsg":"secret-url-must-not-appear"}))
                .contains("310000")
        );
        assert!(
            !vendor_result(&json!({"errcode":310000,"errmsg":"secret-url-must-not-appear"}))
                .contains("secret-url")
        );
        assert!(vendor_result(&json!({})).contains("缺少"));
    }
    #[test]
    fn url_validation() {
        assert!(valid_url(
            "wecom",
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=test"
        ));
        assert!(!valid_url(
            "wecom",
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send"
        ));
        assert!(!valid_url(
            "dingtalk",
            "https://example.test/robot/send?access_token=test"
        ));
        assert!(valid_url(
            "dingtalk",
            "https://oapi.dingtalk.com/robot/send?access_token=test"
        ));
    }
    #[test]
    fn signing_replaces_existing_parameters() {
        let u = signed_url(
            "https://oapi.dingtalk.com/robot/send?access_token=test&timestamp=old&sign=old",
            "SECtest",
            1700000000000,
        )
        .unwrap();
        let p: Vec<_> = u.query_pairs().collect();
        assert_eq!(p.iter().filter(|(k, _)| k == "sign").count(), 1);
        assert!(p
            .iter()
            .any(|(k, v)| k == "timestamp" && v == "1700000000000"));
        assert!(p.iter().any(|(k, v)| k == "access_token" && v == "test"));
        assert_eq!(
            p.iter().find(|(k, _)| k == "sign").unwrap().1,
            "aZLLrriXgn05YbwaGR7knYsLeJADjr9NwLaNNKpxh4g="
        );
    }
}
