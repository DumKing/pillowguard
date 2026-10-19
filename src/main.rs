mod assist;
mod brand;
mod captures;
mod care;
mod channels;
mod credentials;
mod head;
mod history;
mod notifications;
mod occupancy;
mod region;
mod rules;
mod stream;
use region::{Region, RegionLike};
mod vision;
mod wecom_app;
use anyhow::{bail, Result};
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use chrono::Utc;
use chrono_tz::Asia::Shanghai;
use rules::{Pose, Rule};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncReadExt, process::Command};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    care: care::Settings,
    alert_from_unknown: bool,
    assist_enabled: bool,
    assist_lying: Option<assist::Reference>,
    assist_upright: Option<assist::Reference>,
    lost_alert_enabled: bool,
    lost_after: f64,
    autostart: bool,
    source_mode: String,
    ha_url: String,
    text_entity: String,
    alert_text: String,
    test_text: String,
    channels: Vec<String>,
    bed: Region,
    rest_head_region: Option<Region>,
    hold: f64,
    analysis_fps: u8,
    realtime_preview: bool,
    cooldown: f64,
    threshold: f32,
    person_confidence: f32,
    multiple_confidence: f32,
    schedule_mode: String,
    schedule_start: String,
    schedule_end: String,
    schedule_windows: Option<Vec<rules::TimeWindow>>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            care: care::Settings::default(),
            alert_from_unknown: false,
            assist_enabled: false,
            assist_lying: None,
            assist_upright: None,
            lost_alert_enabled: true,
            lost_after: 30.,
            autostart: false,
            source_mode: "demo".into(),
            ha_url: "http://homeassistant.local:8123".into(),
            text_entity: "text.xiaoai_play_text".into(),
            alert_text: "注意，{time} 检测到老人{status}，请及时查看。".into(),
            test_text: "枕安通知测试：当前状态为{status}，发送时间{time}。".into(),
            channels: vec!["speaker".into()],
            bed: [0.1, 0.2, 0.8, 0.7].into(),
            rest_head_region: None,
            hold: 2.,
            analysis_fps: 4,
            realtime_preview: false,
            cooldown: 60.,
            threshold: 0.65,
            person_confidence: 0.5,
            multiple_confidence: 0.75,
            schedule_mode: "all_day".into(),
            schedule_start: "22:00".into(),
            schedule_end: "07:00".into(),
            schedule_windows: None,
        }
    }
}
impl Config {
    fn validate(&self) -> Result<()> {
        if !(1..=4).contains(&self.analysis_fps) {
            bail!("识别频率应为每秒 1 至 4 帧");
        }
        self.care.validate(&self.bed)?;
        if let Some(r) = &self.rest_head_region {
            r.validate()?;
        }
        if !self.lost_after.is_finite() || !(10. ..=300.).contains(&self.lost_after) {
            bail!("无法判断提醒时间应为 10 至 300 秒");
        }
        if self.assist_enabled && !assist::ready(&self.assist_lying, &self.assist_upright) {
            bail!("请先采集有效的卧床和坐起参考");
        }
        if self
            .assist_lying
            .iter()
            .chain(self.assist_upright.iter())
            .any(|r| !r.valid())
        {
            bail!("校准参考无效");
        }
        if !["demo", "camera", "replay"].contains(&self.source_mode.as_str()) {
            bail!("模式不正确");
        }
        if !self.person_confidence.is_finite() || !(0.35..=0.9).contains(&self.person_confidence) {
            bail!("人体检测置信度应为 0.35 至 0.9");
        }
        if !self.multiple_confidence.is_finite()
            || !(0.5..=0.95).contains(&self.multiple_confidence)
        {
            bail!("第二人确认阈值应为 0.5 至 0.95");
        }
        let u = reqwest::Url::parse(&self.ha_url)?;
        if !["http", "https"].contains(&u.scheme())
            || u.host_str().is_none()
            || !u.username().is_empty()
            || u.password().is_some()
            || u.query().is_some()
            || u.fragment().is_some()
            || !["", "/"].contains(&u.path())
        {
            bail!("Home Assistant 地址不正确");
        }
        if !self.text_entity.starts_with("text.")
            || self.text_entity.len() < 6
            || !self.text_entity[5..]
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        {
            bail!("播放文本实体应以 text. 开头");
        }
        if self.alert_text.trim().is_empty() || self.alert_text.chars().count() > 255 {
            bail!("通知内容应为 1 至 255 字");
        }
        if self.test_text.trim().is_empty() || self.test_text.chars().count() > 200 {
            bail!("测试通知模板应为 1 至 200 字");
        }
        if self.channels.is_empty()
            || self
                .channels
                .iter()
                .any(|c| !["speaker", "ha", "webhook", "wecom", "dingtalk"].contains(&c.as_str()))
        {
            bail!("请选择通知渠道");
        }
        let windows = self.time_windows();
        if !["all_day", "custom"].contains(&self.schedule_mode.as_str())
            || windows.len() > 16
            || (self.schedule_mode == "custom" && windows.is_empty())
            || windows
                .iter()
                .any(|w| !valid_time(&w.start) || !valid_time(&w.end) || w.start == w.end)
        {
            bail!("请设置 1 至 16 个有效时间区间，开始与结束时间不能相同");
        }
        if !self.hold.is_finite()
            || !(0.5..=30.).contains(&self.hold)
            || !self.cooldown.is_finite()
            || !(5.0..=3600.).contains(&self.cooldown)
            || !self.threshold.is_finite()
            || !(0.4..=0.95).contains(&self.threshold)
        {
            bail!("判断参数超出允许范围");
        }
        self.bed.validate()?;
        Ok(())
    }
    fn time_windows(&self) -> Vec<rules::TimeWindow> {
        self.schedule_windows.clone().unwrap_or_else(|| {
            vec![rules::TimeWindow {
                start: self.schedule_start.clone(),
                end: self.schedule_end.clone(),
            }]
        })
    }
    fn active(&self) -> bool {
        rules::in_windows(
            &self.schedule_mode,
            &self.time_windows(),
            &Utc::now()
                .with_timezone(&Shanghai)
                .format("%H:%M")
                .to_string(),
        )
    }
}
fn valid_time(t: &str) -> bool {
    t.is_ascii()
        && t.len() == 5
        && t.as_bytes()[2] == b':'
        && t[..2].parse::<u8>().is_ok_and(|v| v < 24)
        && t[3..].parse::<u8>().is_ok_and(|v| v < 60)
}
#[derive(Clone, Serialize, Deserialize)]
struct Event {
    #[serde(default)]
    date: String,
    time: String,
    text: String,
}
#[derive(Serialize)]
struct View {
    suspected_lying: bool,
    assisted: bool,
    people_uncertain: bool,
    history_status: String,
    running: bool,
    mode: Option<String>,
    pose: Pose,
    message: String,
    armed: bool,
    events: Vec<Event>,
    last_frame: Option<f64>,
    last_preview: Option<f64>,
    alert: String,
    fps: f64,
    people: usize,
    video_position: f64,
    video_duration: f64,
}
impl Default for View {
    fn default() -> Self {
        Self {
            suspected_lying: false,
            assisted: false,
            people_uncertain: false,
            history_status: "历史记录已保存".into(),
            running: false,
            mode: None,
            pose: Pose::Unknown,
            message: "准备就绪，等待开始监测".into(),
            armed: false,
            events: vec![],
            last_frame: None,
            last_preview: None,
            alert: "尚未测试".into(),
            fps: 0.,
            people: 0,
            video_position: 0.,
            video_duration: 0.,
        }
    }
}
struct Inner {
    frame_evidence: Option<serde_json::Value>,
    captures: captures::Store,
    care_book: care::Book,
    escalation_id: Option<String>,
    care_book_path: Option<PathBuf>,
    samples: assist::Samples,
    history_path: Option<PathBuf>,
    push_channels: Vec<channels::Channel>,
    config: Config,
    view: View,
    token: String,
    webhook: String,
    wecom: String,
    dingtalk: String,
    dingtalk_secret: String,
    camera_source: String,
    replay_source: String,
    frame: Vec<u8>,
    preview: Vec<u8>,
    rule: Rule,
    generation: u64,
    task: Option<tokio::task::JoinHandle<()>>,
    busy: bool,
    demo_pose: Pose,
}
#[derive(Clone)]
struct App {
    inner: Arc<Mutex<Inner>>,
    root: PathBuf,
    data: PathBuf,
    password: Option<String>,
    client: reqwest::Client,
    wecom_sender: wecom_app::Sender,
}
impl App {
    fn event(i: &mut Inner, text: impl Into<String>) {
        i.view.events.insert(
            0,
            Event {
                date: Utc::now()
                    .with_timezone(&Shanghai)
                    .format("%Y-%m-%d")
                    .to_string(),
                time: Utc::now()
                    .with_timezone(&Shanghai)
                    .format("%H:%M:%S")
                    .to_string(),
                text: text.into(),
            },
        );
        i.view.events.truncate(history::LIMIT);
        if let Some(path) = &i.history_path {
            if history::save(path, &i.view.events).is_err() {
                i.view.history_status = "历史记录写入失败，请检查磁盘空间和目录权限".into();
            } else {
                i.view.history_status = "历史记录已保存".into();
            }
        }
    }
}
fn error(message: impl ToString) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":message.to_string()})),
    )
        .into_response()
}
async fn guard(State(app): State<App>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if app.password.is_none() && !["127.0.0.1:8765", "localhost:8765"].contains(&host) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(p) = &app.password {
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("admin:{p}"))
        );
        if req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            != Some(expected.as_str())
        {
            return (
                StatusCode::UNAUTHORIZED,
                [(
                    header::WWW_AUTHENTICATE,
                    "Basic realm=\"Laoren\", charset=\"UTF-8\"",
                )],
                "需要登录",
            )
                .into_response();
        }
    }
    if let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        if origin != format!("http://{host}") && origin != format!("https://{host}") {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let mut out = next.run(req).await;
    out.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    out.headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    out.headers_mut().insert("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'".parse().unwrap());
    out
}
async fn state(State(app): State<App>) -> Json<Value> {
    let i = app.inner.lock().unwrap();
    let mut v = serde_json::to_value(&i.view).unwrap();
    v["care_alerts"] = json!(i.care_book.alerts);
    v["alert_captures"] = i.captures.metadata();
    v["capture_status"] = json!(i.captures.status);
    v["config"] = json!(i.config);
    v["calibration"] = json!({"ready":assist::ready(&i.config.assist_lying,&i.config.assist_upright),"sample_ready":i.samples.capture().is_some()});
    v["push_channels"] = json!(i
        .push_channels
        .iter()
        .map(channels::Channel::public)
        .collect::<Vec<_>>());
    v["camera_is_local"] = json!(i.camera_source == "0");
    v["has_camera_source"] = json!(!i.camera_source.is_empty());
    v["has_replay_source"] = json!(!i.replay_source.is_empty());
    v["has_token"] = json!(!i.token.is_empty());
    v["has_webhook"] = json!(!i.webhook.is_empty());
    v["has_wecom"] = json!(!i.wecom.is_empty());
    v["has_dingtalk"] = json!(!i.dingtalk.is_empty());
    v["has_dingtalk_secret"] = json!(!i.dingtalk_secret.is_empty());
    v["head_detection_available"] = json!(app.root.join("models/head-detector.onnx").exists());
    v["engine"] = json!("Rust / ONNX Runtime");
    v["schedule"] = json!({"active":i.config.active(),"clock":Utc::now().with_timezone(&Shanghai).format("%H:%M").to_string(),"hold":i.config.hold,"cooldown":i.config.cooldown,"timezone":"Asia/Shanghai"});
    Json(v)
}
fn write_config(data: &Path, c: &Config) -> Result<()> {
    let path = data.join("config.json");
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(c)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(temp, path)?;
    Ok(())
}
async fn calibrate(State(app): State<App>, Json(data): Json<Value>) -> Response {
    let result = (|| -> Result<()> {
        let mut i = app.inner.lock().unwrap();
        let mut c = i.config.clone();
        match data.get("step").and_then(Value::as_str).unwrap_or("") {
            "clear" => {
                c.assist_enabled = false;
                c.assist_lying = None;
                c.assist_upright = None;
            }
            step @ ("lying" | "upright") => {
                if !i.view.running
                    || i.view.mode.as_deref() != Some("camera")
                    || !c.active()
                    || i.view.people != 1
                    || i.view.people_uncertain
                {
                    bail!("请先开启实时摄像头，保持床位内只有一人");
                }
                let reference = i.samples.capture().ok_or_else(|| {
                    anyhow::anyhow!("请露出面部或双肩，并保持同一姿态稳定至少 2 秒后重试")
                })?;
                if step == "lying" {
                    c.assist_lying = Some(reference)
                } else {
                    c.assist_upright = Some(reference)
                }
                if c.assist_lying.is_some()
                    && c.assist_upright.is_some()
                    && !assist::ready(&c.assist_lying, &c.assist_upright)
                {
                    bail!("两种姿态的头肩位置过于接近，请调整侧面机位并重新采集");
                }
                c.assist_enabled = false;
            }
            _ => bail!("校准操作无效"),
        }
        c.validate()?;
        write_config(&app.data, &c)?;
        i.config = c;
        i.rule.reset_baseline();
        i.view.armed = false;
        App::event(
            &mut i,
            "辅助参考已永久保存；完成两种姿态采集后可开启辅助判断",
        );
        Ok(())
    })();
    match result {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => error(&e.to_string()),
    }
}
fn persist_care(i: &Inner) {
    let Some(path) = &i.care_book_path else {
        return;
    };
    let result = (|| -> Result<()> {
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, serde_json::to_vec(&i.care_book)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        eprintln!("告警确认记录保存失败");
    }
}
async fn acknowledge(State(app): State<App>, Json(data): Json<Value>) -> Response {
    let mut i = app.inner.lock().unwrap();
    if !i.config.care.confirmation {
        return error("当前为自动处理模式，无需人工确认");
    }
    let id = data["id"].as_str().unwrap_or("");
    if !i.care_book.acknowledge(id) {
        return error("告警不存在，请刷新后重试");
    }
    persist_care(&i);
    App::event(
        &mut i,
        format!("告警 {} 已在控制台确认，停止该事件的重复与升级通知", id),
    );
    Json(json!({"ok":true})).into_response()
}
#[derive(Clone)]
struct Dispatch {
    id: String,
    text: String,
    escalation: bool,
    kind: care::Kind,
}
fn trigger_care(app: &App, kind: care::Kind, reason: Option<&str>, generation: u64, replay: bool) {
    let dispatch = {
        let mut i = app.inner.lock().unwrap();
        if i.generation != generation
            || !i.view.running
            || i.view.pose == Pose::Multiple
            || !i.config.care.enabled(kind)
        {
            return;
        }
        let demo = i.view.mode.as_deref() == Some("demo");
        if !i.config.active() && !demo {
            return;
        }
        if replay {
            App::event(&mut i, format!("回放：{}，不发送通知", kind.label()));
            return;
        }
        let clock = Utc::now()
            .with_timezone(&Shanghai)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let template = i.config.care.template(kind, &i.config.alert_text);
        let mut text = template
            .replace("{status}", kind.label())
            .replace("{time}", &clock)
            .replace("{people}", &i.view.people.to_string());
        if let Some(prefix) = reason {
            text = format!("{}：{}", prefix, text)
        }
        let now = Utc::now().timestamp_millis() as f64 / 1000.;
        if i.care_book.alerts.iter().any(|a| {
            a.kind == kind
                && a.demo == demo
                && ((kind != care::Kind::Sitting && !a.acknowledged && !a.resolved)
                    || now - a.created < i.config.cooldown)
        }) {
            return;
        }
        let id = i.care_book.create(kind, text.clone(), now, demo);
        persist_care(&i);
        let fresh = !demo && i.view.last_frame.is_some_and(|t| now >= t && now - t <= 2.);
        let image = if fresh { i.frame.clone() } else { vec![] };
        let evidence = if fresh {
            i.frame_evidence.clone()
        } else {
            None
        };
        i.captures.record(
            id.clone(),
            clock,
            kind.label().into(),
            demo,
            &image,
            evidence,
        );
        App::event(
            &mut i,
            format!(
                "{}{}，告警编号 {}",
                if demo { "演示：" } else { "" },
                kind.label(),
                id
            ),
        );
        Dispatch {
            id,
            text,
            escalation: false,
            kind,
        }
    };
    tokio::spawn(notify_dispatch(
        app.clone(),
        true,
        generation,
        None,
        None,
        Some(dispatch),
    ));
}
async fn demo_event(State(app): State<App>, Json(data): Json<Value>) -> Response {
    let kind: care::Kind = match serde_json::from_value(data["kind"].clone()) {
        Ok(k) => k,
        Err(_) => return error("告警类型无效"),
    };
    let generation = {
        let i = app.inner.lock().unwrap();
        if !i.view.running || i.view.mode.as_deref() != Some("demo") {
            return error("请先启动演示模式");
        }
        if !i.config.care.enabled(kind) {
            return error("请先在监测规则中启用此类告警并保存");
        };
        i.generation
    };
    trigger_care(&app, kind, None, generation, false);
    Json(json!({"ok":true})).into_response()
}
async fn watch_care(app: App) {
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    loop {
        timer.tick().await;
        let (due, generation) = {
            let mut i = app.inner.lock().unwrap();
            let demo = i.view.mode.as_deref() == Some("demo");
            if !i.view.running
                || i.view.mode.as_deref() == Some("replay")
                || (!i.config.active() && !demo)
                || i.view.pose == Pose::Multiple
                || i.busy
                || i.escalation_id.is_some()
            {
                continue;
            }
            let settings = i.config.care.clone();
            let due = i.care_book.due(
                &settings,
                Utc::now().timestamp_millis() as f64 / 1000.,
                demo,
            );
            if !due.is_empty() {
                persist_care(&i);
            }
            (due, i.generation)
        };
        for (id, text, escalation, kind) in due {
            let dispatch = Dispatch {
                id,
                text,
                escalation,
                kind,
            };
            if escalation {
                app.inner.lock().unwrap().escalation_id = Some(dispatch.id.clone());
                tokio::spawn(escalation_burst(app.clone(), generation, dispatch));
            } else {
                tokio::spawn(notify_dispatch(
                    app.clone(),
                    true,
                    generation,
                    None,
                    None,
                    Some(dispatch),
                ));
            }
        }
    }
}
// The speaker reports request acceptance, not playback completion. Reserve an
// estimated speech duration plus the configured gap before the next request.
async fn escalation_burst(app: App, generation: u64, dispatch: Dispatch) {
    let settings = app.inner.lock().unwrap().config.care.clone();
    for play in 1..=settings.escalate_plays {
        let valid = {
            let i = app.inner.lock().unwrap();
            i.generation == generation
                && i.config.care.escalate
                && i.care_book.active(&dispatch.id)
                && i.view.running
                && i.view.pose != Pose::Multiple
                && (i.config.active() || i.view.mode.as_deref() == Some("demo"))
        };
        if !valid {
            break;
        }
        let mut next = dispatch.clone();
        next.text = format!("第{}次提醒：{}", play, dispatch.text);
        if !notify_dispatch(app.clone(), true, generation, None, None, Some(next)).await {
            break;
        }
        if play < settings.escalate_plays {
            let seconds =
                (dispatch.text.chars().count().min(180) + 25) as f64 / 4. + settings.escalate_gap;
            let deadline = Instant::now() + Duration::from_secs_f64(seconds);
            // Check cancellation while waiting, including a stop/restart generation.
            while Instant::now() < deadline {
                let valid = {
                    let i = app.inner.lock().unwrap();
                    i.generation == generation
                        && i.config.care.escalate
                        && i.care_book.active(&dispatch.id)
                        && i.view.running
                        && i.view.pose != Pose::Multiple
                        && (i.config.active() || i.view.mode.as_deref() == Some("demo"))
                };
                if !valid {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    let mut i = app.inner.lock().unwrap();
    if i.escalation_id.as_deref() == Some(dispatch.id.as_str()) {
        i.escalation_id = None;
    }
}
async fn alert_capture(
    State(app): State<App>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match app.inner.lock().unwrap().captures.image(&id) {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn frame(State(app): State<App>) -> Response {
    let bytes = app.inner.lock().unwrap().frame.clone();
    if bytes.is_empty() {
        StatusCode::NOT_FOUND.into_response()
    } else {
        ([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response()
    }
}
async fn preview(State(app): State<App>) -> Response {
    let i = app.inner.lock().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    if !i.config.realtime_preview
        || !i.view.running
        || i.view.mode.as_deref() != Some("camera")
        || !i.config.active()
        || !i
            .view
            .last_preview
            .is_some_and(|t| now >= t && now - t < 2.)
        || i.preview.is_empty()
    {
        return ([(header::CACHE_CONTROL, "no-store")], StatusCode::NOT_FOUND).into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        i.preview.clone(),
    )
        .into_response()
}
async fn save(State(app): State<App>, Json(data): Json<Value>) -> Response {
    let result = (|| -> Result<()> {
        let mut i = app.inner.lock().unwrap();
        let mut merged = serde_json::to_value(&i.config)?;
        for (k, v) in data
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("配置格式不正确"))?
        {
            if merged.get(k).is_some() && !["assist_lying", "assist_upright"].contains(&k.as_str())
            {
                merged[k] = v.clone();
            }
        }
        if data.get("schedule_windows").is_none()
            && (data.get("schedule_start").is_some() || data.get("schedule_end").is_some())
        {
            merged["schedule_windows"] = Value::Null;
        }
        let mut c: Config = serde_json::from_value(merged)?;
        if let Some(first) = c.schedule_windows.as_ref().and_then(|w| w.first()) {
            c.schedule_start = first.start.clone();
            c.schedule_end = first.end.clone();
        }
        c.care.focus_sitting();
        c.alert_text = c.alert_text.trim().into();
        c.ha_url = c.ha_url.trim_end_matches('/').into();
        c.validate()?;
        let new_token = data
            .get("token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if new_token.chars().any(char::is_whitespace) {
            bail!("令牌不能含空白");
        }
        let hook = data
            .get("webhook_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let wecom = data
            .get("wecom_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let dingtalk = data
            .get("dingtalk_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let ding_secret = data
            .get("dingtalk_secret")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        for (channel, url) in [("webhook", hook), ("wecom", wecom), ("dingtalk", dingtalk)] {
            if !url.is_empty() && !notifications::valid_url(channel, url) {
                bail!("机器人地址不正确");
            }
        }
        if ding_secret.chars().any(char::is_whitespace) {
            bail!("加签密钥不能含空白");
        }
        if data.get("dingtalk_signing").and_then(Value::as_bool) == Some(true)
            && ding_secret.is_empty()
            && i.dingtalk_secret.is_empty()
        {
            bail!("请填写钉钉加签密钥");
        }
        let routes = match data.get("push_channels") {
            Some(value) => channels::merge(value, &i.push_channels)?,
            None => i.push_channels.clone(),
        };
        if c.care.escalate && !routes.iter().any(|r| r.enabled && r.kind == "speaker") {
            bail!("升级连续播报需要至少一个已启用的小爱音箱渠道");
        }
        let mut saved = credentials::Credentials {
            push_channels: Some(routes),
            token: i.token.clone(),
            webhook: i.webhook.clone(),
            wecom: i.wecom.clone(),
            dingtalk: i.dingtalk.clone(),
            dingtalk_secret: i.dingtalk_secret.clone(),
            camera_source: i.camera_source.clone(),
            replay_source: i.replay_source.clone(),
        };
        for (target, value) in [
            (&mut saved.token, new_token),
            (&mut saved.webhook, hook),
            (&mut saved.wecom, wecom),
            (&mut saved.dingtalk, dingtalk),
        ] {
            if !value.is_empty() {
                *target = value.into();
            }
        }
        if data.get("dingtalk_signing").and_then(Value::as_bool) == Some(false) {
            saved.dingtalk_secret.clear();
        } else if !ding_secret.is_empty() {
            saved.dingtalk_secret = ding_secret.into();
        }
        let source = data
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if data.get("clear_source").and_then(Value::as_bool) == Some(true) {
            if c.source_mode == "replay" {
                saved.replay_source.clear();
            } else if c.source_mode == "camera" {
                saved.camera_source.clear();
            }
        } else if !source.is_empty() {
            match c.source_mode.as_str() {
                "camera" => saved.camera_source = source.into(),
                "replay" => saved.replay_source = source.into(),
                _ => (),
            }
        }
        let geometry_changed = c.bed != i.config.bed || saved.camera_source != i.camera_source;
        if saved.camera_source != i.camera_source
            && i.view.running
            && i.view.mode.as_deref() == Some("camera")
        {
            bail!("更换视频来源前请先停止监测");
        }
        if geometry_changed {
            if saved.camera_source != i.camera_source {
                c.rest_head_region = None;
            }
            c.assist_enabled = false;
            c.assist_lying = None;
            c.assist_upright = None;
            c.care.edge = false;
            c.care.fall = false;
        }
        write_config(&app.data, &c)?;
        credentials::save_all(&app.data, &saved)?;
        if geometry_changed {
            i.care_book.resolve();
            persist_care(&i);
            i.samples.clear();
            App::event(
                &mut i,
                "床位或视频来源已更改，辅助校准已清除，请重新采集参考",
            );
        }
        let care_changed = serde_json::to_value(&c.care)? != serde_json::to_value(&i.config.care)?;
        let rule_changed = care_changed
            || c.alert_from_unknown != i.config.alert_from_unknown
            || geometry_changed
            || c.hold != i.config.hold
            || c.threshold != i.config.threshold
            || c.person_confidence != i.config.person_confidence
            || c.multiple_confidence != i.config.multiple_confidence
            || c.assist_enabled != i.config.assist_enabled
            || c.schedule_mode != i.config.schedule_mode
            || c.schedule_start != i.config.schedule_start
            || c.schedule_end != i.config.schedule_end
            || c.schedule_windows != i.config.schedule_windows;
        if c.person_confidence != i.config.person_confidence
            || c.multiple_confidence != i.config.multiple_confidence
            || c.analysis_fps != i.config.analysis_fps
        {
            i.samples.clear();
        }
        i.push_channels = saved.push_channels.unwrap_or_default();
        if i.config.rest_head_region != c.rest_head_region {
            i.view.suspected_lying = false;
        }
        i.config = c;
        i.token = saved.token;
        i.webhook = saved.webhook;
        i.wecom = saved.wecom;
        i.dingtalk = saved.dingtalk;
        i.dingtalk_secret = saved.dingtalk_secret;
        i.camera_source = saved.camera_source;
        i.replay_source = saved.replay_source;
        if rule_changed {
            i.rule.reset_baseline();
            i.view.armed = false;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => {
            let message = if data.get("push_channels").is_some() {
                "保存失败，请检查渠道名称、地址、参数及本机写入权限"
            } else {
                "配置无效，请检查时间、通知内容和参数范围"
            };
            let _ = e;
            error(message)
        }
    }
}
async fn source_reveal(State(app): State<App>, Json(data): Json<Value>) -> Response {
    let i = app.inner.lock().unwrap();
    match data["mode"].as_str() {
        Some("camera") => Json(json!({"value":i.camera_source})).into_response(),
        Some("replay") => Json(json!({"value":i.replay_source})).into_response(),
        _ => error("请选择摄像头或回放来源"),
    }
}
async fn ha_check(State(app): State<App>, Json(_): Json<Value>) -> Json<Value> {
    let (config, token) = {
        let i = app.inner.lock().unwrap();
        (i.config.clone(), i.token.clone())
    };
    if token.is_empty() {
        return Json(json!({"ok":false,"message":"未配置 Home Assistant 令牌"}));
    }
    let started = Instant::now();
    let mut checks = vec![];
    for path in [
        "/api/".to_string(),
        format!("/api/states/{}", config.text_entity),
    ] {
        match app
            .client
            .get(format!("{}{}", config.ha_url, path))
            .bearer_auth(&token)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => checks.push(json!({"ok":true})),
            Ok(r) => {
                return Json(
                    json!({"ok":false,"message":format!("Home Assistant 返回 HTTP {}",r.status().as_u16())}),
                )
            }
            Err(e) => {
                return Json(
                    json!({"ok":false,"message":notifications::transport_error(&e),"ms":started.elapsed().as_millis()}),
                )
            }
        }
    }
    Json(
        json!({"ok":true,"message":"Home Assistant 连接、令牌和播放文本实体正常","ms":started.elapsed().as_millis()}),
    )
}
async fn channel_reveal(State(app): State<App>, Json(data): Json<Value>) -> Response {
    let i = app.inner.lock().unwrap();
    let Some(channel) = i
        .push_channels
        .iter()
        .find(|c| Some(c.id.as_str()) == data["id"].as_str())
    else {
        return error("渠道不存在");
    };
    let value = match data["field"].as_str() {
        Some("url") => &channel.url,
        Some("secret") => &channel.secret,
        _ => return error("字段无效"),
    };
    Json(json!({"value":value})).into_response()
}
async fn token_action(
    State(app): State<App>,
    axum::extract::Path(action): axum::extract::Path<String>,
    Json(_): Json<Value>,
) -> Response {
    let mut i = app.inner.lock().unwrap();
    match action.as_str() {
        "reveal" => Json(json!({"token": i.token})).into_response(),
        "clear" => match credentials::save(&app.data, "") {
            Ok(()) => {
                i.token.clear();
                Json(json!({"ok":true})).into_response()
            }
            Err(_) => error("无法清除已保存的令牌"),
        },
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn notification_text(
    config: &Config,
    automatic: bool,
    pose: Pose,
    people: usize,
    clock: &str,
    people_uncertain: bool,
) -> String {
    let status = if !automatic && people_uncertain {
        "遮挡，人数待确认"
    } else {
        match pose {
            Pose::Lying => "躺卧",
            Pose::Upright => "坐起",
            Pose::Multiple => "多人暂停",
            Pose::Unknown => "姿态未确认",
        }
    };
    let people_label = if !automatic && people_uncertain {
        "待确认".into()
    } else {
        people.to_string()
    };
    let template = if automatic {
        &config.alert_text
    } else {
        &config.test_text
    };
    template
        .replace("{status}", status)
        .replace("{people}", &people_label)
        .replace("{time}", clock)
        .chars()
        .take(255)
        .collect()
}

async fn notify(app: App, automatic: bool, generation: u64) {
    notify_routes(app, automatic, generation, None).await;
}
async fn notify_routes(app: App, automatic: bool, generation: u64, only: Option<String>) {
    notify_reason(app, automatic, generation, only, None).await;
}
async fn notify_reason(
    app: App,
    automatic: bool,
    generation: u64,
    only: Option<String>,
    reason: Option<&'static str>,
) {
    notify_dispatch(app, automatic, generation, only, reason, None).await;
}
async fn notify_dispatch(
    app: App,
    automatic: bool,
    generation: u64,
    only: Option<String>,
    reason: Option<&'static str>,
    dispatch: Option<Dispatch>,
) -> bool {
    // Preserve a rise event while an earlier channel request is in flight.
    // Individual requests are bounded; no network request itself is retried.
    if automatic {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let busy = {
                let i = app.inner.lock().unwrap();
                if i.generation != generation {
                    return false;
                }
                i.busy
            };
            if !busy {
                break;
            }
            if Instant::now() >= deadline {
                let mut i = app.inner.lock().unwrap();
                App::event(&mut i, "告警未发送：上一条通知仍未结束，请检查通知渠道");
                return false;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    let (config, token, routes) = {
        let mut i = app.inner.lock().unwrap();
        if dispatch.as_ref().is_some_and(|d| {
            !i.care_book.active(&d.id)
                || !i.config.care.enabled(d.kind)
                || (d.escalation && !i.config.care.escalate)
        }) {
            return false;
        }
        if (!i.config.lost_alert_enabled && matches!(reason, Some("lost" | "failure")))
            || i.busy
            || (automatic
                && ((!i.view.running && reason != Some("failure"))
                    || i.generation != generation
                    || (!i.config.active() && i.view.mode.as_deref() != Some("demo"))
                    || i.view.pose == Pose::Multiple))
        {
            return false;
        }
        i.busy = true;
        i.view.alert = "正在发送所选渠道…".into();
        let mut settings = i.config.clone();
        settings.alert_text = notification_text(
            &settings,
            automatic,
            if automatic {
                Pose::Upright
            } else {
                i.view.pose
            },
            i.view.people,
            &Utc::now()
                .with_timezone(&Shanghai)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
            i.view.people_uncertain,
        );
        if let Some(reason) = reason {
            settings.alert_text = if reason == "assisted" {
                format!("疑似起身（遮挡辅助）：{}", settings.alert_text)
            } else if reason == "from_unknown" {
                format!("无法识别后检测到坐起：{}", settings.alert_text)
            } else if reason == "failure" {
                "监测异常：摄像头监测启动失败或意外停止，当前无法判断老人状态，请及时查看。"
                    .to_string()
            } else {
                "监测异常：床位画面持续无法判断，可能被被子遮挡、人员离床或摄像头断流，请及时查看。恢复前每 5 分钟提醒一次。".to_string()
            };
        }
        if let Some(d) = &dispatch {
            let prefix = if d.escalation {
                if settings.care.confirmation {
                    "未确认，升级通知："
                } else {
                    "告警尚未解除，升级通知："
                }
            } else {
                ""
            };
            settings.alert_text = format!(
                "{}{}{}",
                prefix,
                d.text.chars().take(180).collect::<String>(),
                if settings.care.confirmation {
                    format!(" [告警 {}]", d.id)
                } else {
                    String::new()
                }
            );
        }
        settings.alert_text = settings.alert_text.chars().take(255).collect();
        if automatic && i.view.mode.as_deref() == Some("demo") {
            settings.alert_text = format!("演示告警：{}", settings.alert_text)
                .chars()
                .take(255)
                .collect();
        }
        (
            settings,
            i.token.clone(),
            i.push_channels
                .iter()
                .filter(|c| match &only {
                    Some(id) => &c.id == id,
                    None => {
                        c.enabled
                            && (!dispatch.as_ref().is_some_and(|d| d.escalation)
                                || c.kind == "speaker")
                    }
                })
                .cloned()
                .collect::<Vec<_>>(),
        )
    };
    let title = dispatch
        .as_ref()
        .map(|d| d.kind.label())
        .unwrap_or(if automatic {
            "老人起身提醒"
        } else {
            "枕安通知测试"
        });
    let event_type = dispatch
        .as_ref()
        .map(|d| d.kind.key())
        .unwrap_or(if automatic {
            "rise_alert"
        } else {
            "notification_test"
        });
    let mut tasks = tokio::task::JoinSet::new();
    for route in routes {
        let channel = route.kind.clone();
        let c = config.clone();
        let t = token.clone();
        let client = app.client.clone();
        let enterprise = route.clone();
        let wecom_sender = app.wecom_sender.clone();
        let secret = route.secret;
        let url = route.url;
        let live_app = app.clone();
        let live_dispatch = dispatch.clone();
        let route_id = route.id.clone();
        tasks.spawn(async move {
            let name = route.name;
            let mut accepted = false;
            let result = if channel == "wecom_app" {
                wecom_sender.send(&client, &enterprise, &c.alert_text).await
            } else if ["webhook", "wecom", "dingtalk"].contains(&channel.as_str()) {
                notifications::send(
                    &client,
                    &channel,
                    &url,
                    &secret,
                    &c.alert_text,
                    event_type,
                )
                .await
            } else if t.is_empty() {
                "未配置 HA 令牌".into()
            } else {
                if channel=="speaker" {
                    let check=client.get(format!("{}/api/states/{}",c.ha_url,c.text_entity)).bearer_auth(&t).timeout(Duration::from_secs(10)).send().await;
                    let valid=match check {Ok(r)if r.status().is_success()=>r.json::<Value>().await.ok().is_some_and(|v|v["entity_id"].as_str()==Some(c.text_entity.as_str())&&v["state"].as_str()!=Some("unavailable")),_=>false};
                    if !valid {return (format!("{name}：播放文本实体检查失败，未执行播报；请检查 HA 连接、实体与音箱动作映射"), false);}
                }
                if let Some(d) = &live_dispatch {
                    let i = live_app.inner.lock().unwrap();
                    if i.generation != generation || !i.view.running || !i.care_book.active(&d.id)
                        || i.view.pose == Pose::Multiple
                        || (!i.config.active() && i.view.mode.as_deref() != Some("demo"))
                        || (d.escalation && (!i.config.care.escalate || !i.push_channels.iter().any(|r| r.id == route_id && r.enabled))) {
                        return (format!("{name}：状态已变化，取消尚未提交的播报"), false);
                    }
                }
                let (url, body) = if channel == "speaker" {
                    (
                        format!("{}/api/services/text/set_value", c.ha_url),
                        json!({"entity_id":c.text_entity,"value":c.alert_text}),
                    )
                } else {
                    (
                        format!("{}/api/services/persistent_notification/create", c.ha_url),
                        json!({"title":title,"message":c.alert_text}),
                    )
                };
                match client.post(url).bearer_auth(t).timeout(Duration::from_secs(45)).json(&body).send().await {
                    Ok(r) if r.status().is_success() => { accepted = true; "请求已接受，请核实实际送达".into() },
                    Ok(r) => format!("失败 HTTP {}", r.status().as_u16()),
                    Err(e) => notifications::transport_error(&e),
                }
            };
            (format!("{name}：{result}"), accepted)
        });
    }
    let mut results = vec![];
    let mut accepted = true;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((message, ok)) => {
                accepted &= ok;
                results.push(message);
            }
            Err(_) => {
                accepted = false;
                results.push("通知任务未完成，停止连续播报".into());
            }
        }
    }
    accepted &= !results.is_empty();
    let mut i = app.inner.lock().unwrap();
    for r in &results {
        App::event(&mut i, r);
    }
    i.view.alert = if results.is_empty() {
        "未启用推送渠道，请添加并启用渠道".into()
    } else {
        results.join("；")
    };
    i.busy = false;
    accepted
}
fn demo_count(pose: Pose) -> usize {
    match pose {
        Pose::Multiple => 2,
        Pose::Unknown => 0,
        _ => 1,
    }
}
fn observe(app: &App, pose: Pose, count: usize, now: f64, simulated: bool, generation: u64) {
    observe_frame(app, pose, count, now, simulated, generation, false, false);
}
fn observe_frame(
    app: &App,
    pose: Pose,
    count: usize,
    now: f64,
    simulated: bool,
    generation: u64,
    uncertain: bool,
    assisted: bool,
) {
    let (fired, from_unknown) = {
        let mut i = app.inner.lock().unwrap();
        if !i.view.running || i.generation != generation {
            return;
        }
        let (pose, count) = if simulated && i.view.mode.as_deref() == Some("demo") {
            (i.demo_pose, demo_count(i.demo_pose))
        } else {
            (pose, count)
        };
        let old = i.view.pose;
        let old_uncertain = i.view.people_uncertain;
        i.view.people = count;
        let active = i.config.active() || (simulated && i.view.mode.as_deref() == Some("demo"));
        let pose = if active { pose } else { Pose::Unknown };
        i.view.pose = pose;
        i.view.suspected_lying = false;
        i.view.assisted = active && assisted;
        i.view.people_uncertain = pose == Pose::Multiple && uncertain;
        if pose == Pose::Multiple && old != Pose::Multiple {
            App::event(
                &mut i,
                if uncertain {
                    "被子或其他遮挡影响人数判断，暂停自动告警"
                } else {
                    "床位区域内出现多人，暂停判断和自动告警"
                },
            );
        }
        if old == Pose::Multiple && pose != Pose::Multiple {
            App::event(
                &mut i,
                if old_uncertain {
                    "遮挡或疑似多人状态结束，等待重新确认躺卧"
                } else {
                    "多人状态结束，等待重新确认单人躺卧"
                },
            );
        }
        let c = i.config.clone();
        let from_unknown = c.alert_from_unknown && !i.rule.armed && !assisted;
        let fired = i.rule.update_policy(
            pose,
            now,
            c.hold,
            c.cooldown,
            active && assisted,
            active && c.alert_from_unknown,
        );
        i.view.armed = i.rule.armed;
        if (pose == Pose::Lying && i.rule.armed) || pose == Pose::Multiple {
            if i.care_book
                .alerts
                .iter()
                .any(|a| !a.resolved && !a.acknowledged)
            {
                i.care_book.resolve();
                persist_care(&i);
                App::event(
                    &mut i,
                    "姿态恢复卧床或照护人进入，已停止当前事件的重复与升级通知",
                );
            }
        }
        let demo = i.view.mode.as_deref() == Some("demo");
        if fired {
            App::event(
                &mut i,
                if simulated && demo {
                    "演示：检测到起身，正在发送所选渠道通知"
                } else if simulated {
                    "回放：检测到起身，不发送通知"
                } else if assisted {
                    "疑似起身（遮挡辅助），正在发送告警"
                } else if from_unknown {
                    "无法识别后检测到持续坐起，正在发送告警"
                } else {
                    "检测到卧床后坐起，正在发送告警"
                },
            );
        }
        (
            fired && (!simulated || i.view.mode.as_deref() == Some("demo")),
            from_unknown,
        )
    };
    let replay = simulated && app.inner.lock().unwrap().view.mode.as_deref() != Some("demo");
    if fired {
        trigger_care(
            app,
            care::Kind::Sitting,
            if assisted {
                Some("疑似起身（遮挡辅助）")
            } else if from_unknown {
                Some("无法识别后检测到坐起")
            } else {
                None
            },
            generation,
            replay,
        );
    }
}
async fn watch_visibility(app: App) {
    let begin = Instant::now();
    let mut watchdog = rules::Watchdog::default();
    let mut generation = u64::MAX;
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    loop {
        timer.tick().await;
        let trigger = {
            let mut i = app.inner.lock().unwrap();
            if generation != i.generation {
                generation = i.generation;
                watchdog = rules::Watchdog::default();
            }
            let fresh = i.view.running
                && i.config.active()
                && i.view
                    .last_frame
                    .is_some_and(|t| Utc::now().timestamp_millis() as f64 / 1000. - t <= 2.);
            if !fresh && i.view.suspected_lying {
                i.view.suspected_lying = false;
                if i.view.message.starts_with("疑似卧床") {
                    i.view.message = "画面未更新，疑似卧床线索已失效".into();
                }
            }
            if !i.config.lost_alert_enabled
                || !i.view.running
                || i.view.mode.as_deref() != Some("camera")
                || !i.config.active()
                || i.view.pose == Pose::Multiple
            {
                watchdog = rules::Watchdog::default();
                None
            } else if i.busy {
                None
            } else {
                let stale = i
                    .view
                    .last_frame
                    .is_none_or(|t| Utc::now().timestamp_millis() as f64 / 1000. - t > 5.);
                let (alarm, recovered) = watchdog.update(
                    stale || i.view.pose == Pose::Unknown,
                    begin.elapsed().as_secs_f64(),
                    i.config.lost_after,
                );
                if recovered {
                    App::event(&mut i, "床位姿态已恢复可判断，监测异常提醒已解除");
                }
                if alarm {
                    App::event(
                        &mut i,
                        "持续无法判断床位状态，正在发送监测异常提醒（恢复前每 5 分钟重复）",
                    );
                    Some(i.generation)
                } else {
                    None
                }
            }
        };
        if let Some(generation) = trigger {
            tokio::spawn(notify_reason(
                app.clone(),
                true,
                generation,
                None,
                Some("lost"),
            ));
        }
    }
}
fn asset(root: &Path, name: &str) -> PathBuf {
    root.join("bin").join(name)
}
fn ffmpeg(root: &Path) -> PathBuf {
    brand::env("FFMPEG").map(PathBuf::from).unwrap_or_else(|| {
        asset(
            root,
            if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            },
        )
    })
}
#[cfg(any(target_os = "windows", test))]
fn first_windows_camera(text: &str) -> Option<String> {
    text.lines()
        .filter(|line| line.contains("(video)"))
        .find_map(|line| {
            let (_, rest) = line.split_once('"')?;
            let (name, _) = rest.split_once('"')?;
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
}
fn media_duration(text: &str) -> f64 {
    let Some((_, rest)) = text.split_once("Duration: ") else {
        return 0.;
    };
    let parts: Vec<_> = rest
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .split(':')
        .collect();
    if parts.len() != 3 {
        return 0.;
    }
    let values: Option<Vec<f64>> = parts.iter().map(|v| v.parse::<f64>().ok()).collect();
    match values {
        Some(v) if v.iter().all(|n| n.is_finite() && *n >= 0.) => v[0] * 3600. + v[1] * 60. + v[2],
        _ => 0.,
    }
}
async fn monitor(app: App, source: String, replay: bool, generation: u64) -> Result<()> {
    if replay {
        let mut probe = Command::new(ffmpeg(&app.root));
        probe
            .args(["-nostdin", "-hide_banner", "-i", &source])
            .kill_on_drop(true);
        if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(8), probe.output()).await {
            let duration = media_duration(&String::from_utf8_lossy(&output.stderr));
            let mut i = app.inner.lock().unwrap();
            if i.generation == generation {
                i.view.video_duration = duration;
            }
        }
    }
    #[cfg(target_os = "windows")]
    let local_camera = if source == "0" {
        let output = tokio::time::timeout(
            Duration::from_secs(8),
            Command::new(ffmpeg(&app.root))
                .args([
                    "-hide_banner",
                    "-list_devices",
                    "true",
                    "-f",
                    "dshow",
                    "-i",
                    "dummy",
                ])
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        first_windows_camera(&String::from_utf8_lossy(&output.stderr))
            .ok_or_else(|| anyhow::anyhow!("没有可用的本机摄像头"))?
    } else {
        String::new()
    };
    let model = app.root.join("models/yolov8n-pose.onnx");
    let detector = tokio::task::spawn_blocking(move || vision::Detector::new(&model)).await??;
    let detector = Arc::new(Mutex::new(detector));
    let begin = Instant::now();
    let mut index = 0u64;
    let mut occupancy = occupancy::Tracker::default();
    let mut multi_pause = occupancy::MultiPause::default();
    loop {
        {
            let i = app.inner.lock().unwrap();
            if i.generation != generation || !i.view.running {
                return Ok(());
            }
        }
        while !app.inner.lock().unwrap().config.active() {
            occupancy.clear();
            multi_pause = occupancy::MultiPause::default();
            {
                let mut i = app.inner.lock().unwrap();
                if i.generation != generation {
                    return Ok(());
                }
                i.rule.reset_baseline();
                i.view.armed = false;
                i.view.pose = Pose::Unknown;
                i.view.people_uncertain = false;
                i.samples.clear();
                i.view.assisted = false;
                i.view.last_frame = None;
                i.view.suspected_lying = false;
                i.view.message = "时段外，等待监测时间".into();
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let mut cmd = Command::new(ffmpeg(&app.root));
        cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
        if source.starts_with("rtsp://") {
            cmd.args(["-rtsp_transport", "tcp"]);
        }
        if source == "0" {
            #[cfg(target_os = "macos")]
            cmd.args([
                "-f",
                "avfoundation",
                "-framerate",
                "25",
                "-video_size",
                "640x480",
                "-pixel_format",
                "nv12",
                "-i",
                "0:none",
            ]);
            #[cfg(target_os = "linux")]
            cmd.args(["-f", "v4l2", "-i", "/dev/video0"]);
            #[cfg(target_os = "windows")]
            cmd.args([
                "-f",
                "dshow",
                "-rtbufsize",
                "32M",
                "-i",
                &format!("video={local_camera}"),
            ]);
        } else {
            cmd.args(["-i", &source]);
        }
        let video_filter = format!("fps={},scale=1280:720:force_original_aspect_ratio=decrease:force_divisible_by=2,pad=1280:720:(ow-iw)/2:(oh-ih)/2,setsar=1", if replay { 4 } else { 10 });
        cmd.args([
            "-vf",
            &video_filter,
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let decoder_diagnostics = stream::DecoderDiagnostics::new(child.stderr.take().unwrap());
        let mut output = Some(child.stdout.take().unwrap());
        let mut live_frames = if replay {
            None
        } else {
            Some(stream::LatestFrames::new(
                output.take().unwrap(),
                1280 * 720 * 3,
                Duration::from_secs(12),
            ))
        };
        if let Some(frames) = &mut live_frames {
            let preview_app = app.clone();
            let enabled_app = app.clone();
            frames.start_preview(
                move || enabled_app.inner.lock().unwrap().config.realtime_preview,
                move |jpeg| {
                    let mut i = preview_app.inner.lock().unwrap();
                    if i.generation != generation || !i.view.running {
                        return false;
                    }
                    i.view.last_preview = if jpeg.is_empty() {
                        None
                    } else {
                        Some(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs_f64(),
                        )
                    };
                    i.preview = jpeg;
                    true
                },
            );
        }
        let connection_started = Instant::now();
        let mut last_analysis = Instant::now();
        let mut last_analysis_start: Option<tokio::time::Instant> = None;
        loop {
            if !replay {
                let fps = app.inner.lock().unwrap().config.analysis_fps;
                if let Some(last) = last_analysis_start {
                    tokio::time::sleep_until(last + stream::analysis_interval(fps)).await;
                }
            }
            let read = if let Some(frames) = &mut live_frames {
                frames.next().await
            } else {
                let mut bytes = vec![0u8; 1280 * 720 * 3];
                match tokio::time::timeout(
                    Duration::from_secs(12),
                    output.as_mut().unwrap().read_exact(&mut bytes),
                )
                .await
                {
                    Ok(Ok(_)) => Ok(bytes),
                    Err(_) => Err("连续 12 秒未收到完整视频帧"),
                    Ok(Err(_)) => Err("视频解码进程输出结束"),
                }
            };
            {
                let i = app.inner.lock().unwrap();
                if i.generation != generation || !i.view.running {
                    return Ok(());
                }
            }
            let active = app.inner.lock().unwrap().config.active();
            if !active {
                let _ = child.kill().await;
                break;
            }
            if let Err(reason) = read {
                occupancy.clear();
                multi_pause = occupancy::MultiPause::default();
                let status = child.try_wait()?;
                let _ = child.kill().await;
                observe(
                    &app,
                    Pose::Unknown,
                    0,
                    begin.elapsed().as_secs_f64(),
                    true,
                    generation,
                );
                if replay {
                    if status.is_some_and(|s| s.success()) {
                        let mut i = app.inner.lock().unwrap();
                        i.view.message = "视频分析完成".into();
                        App::event(&mut i, "视频分析完成，未发送真实通知");
                        return Ok(());
                    }
                    bail!("视频解码中断");
                }
                {
                    let mut i = app.inner.lock().unwrap();
                    i.view.last_frame = None;
                    i.view.suspected_lying = false;
                    if i.view.message != "视频流中断，5 秒后重新连接" {
                        App::event(&mut i, "摄像头断流，当前无法判断姿态");
                        App::event(
                            &mut i,
                            format!(
                                "取流诊断：{reason}；{}；本次连接 {} 秒；解码进程{}",
                                decoder_diagnostics.summary(),
                                connection_started.elapsed().as_secs(),
                                if status.is_some() {
                                    "已退出"
                                } else {
                                    "仍在运行"
                                }
                            ),
                        );
                    }
                    i.view.message = "视频流中断，5 秒后重新连接".into();
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                break;
            }
            let bytes = read.unwrap();
            last_analysis_start = Some(tokio::time::Instant::now());
            index += 1;
            let detector = detector.clone();
            let c = app.inner.lock().unwrap().config.clone();
            let (people, mut bytes, heads, head_conflict) =
                tokio::task::spawn_blocking(move || {
                    detector.lock().unwrap().detect_detailed(
                        &bytes,
                        &c.bed,
                        c.person_confidence,
                        c.multiple_confidence,
                        c.threshold,
                        c.rest_head_region.as_ref(),
                    )
                })
                .await??;
            let (c, valid) = {
                let i = app.inner.lock().unwrap();
                (
                    i.config.clone(),
                    i.generation == generation && i.view.running,
                )
            };
            if !valid {
                return Ok(());
            }
            let people =
                rules::monitor_people(&people, &c.bed, c.person_confidence, c.multiple_confidence);
            occupancy.set_analysis_fps(if replay { 4 } else { c.analysis_fps });
            let (pose, count) = rules::classify(&people, &c.bed, c.threshold);
            let (pose, count, head_multiple) =
                occupancy::head_safety(pose, count, &heads, &c.bed, c.multiple_confidence);
            let frame_time = if replay {
                index as f64 / 4.
            } else {
                begin.elapsed().as_secs_f64()
            };
            let (mut pose, count, multi_recovering) = multi_pause.update(frame_time, pose, count);
            let uncertain =
                !head_multiple && !multi_recovering && rules::uncertain_people(&people, &c.bed);
            let reference = if count == 1 {
                people
                    .iter()
                    .find(|p| rules::in_bed(p, &c.bed))
                    .map(assist::Reference::extract)
            } else {
                None
            };
            {
                let mut i = app.inner.lock().unwrap();
                if i.config.bed != c.bed
                    || i.config.person_confidence != c.person_confidence
                    || i.config.multiple_confidence != c.multiple_confidence
                {
                    continue;
                }
                if !replay {
                    i.samples.set_analysis_fps(c.analysis_fps);
                    i.samples.push(reference.clone());
                }
            }
            let mut assisted = false;
            if !replay && c.assist_enabled && count == 1 && pose != Pose::Lying {
                if let (Some(l), Some(u), Some(p)) =
                    (&c.assist_lying, &c.assist_upright, &reference)
                {
                    let auxiliary = assist::classify(l, u, p);
                    if auxiliary != Pose::Unknown && (pose == Pose::Unknown || auxiliary == pose) {
                        pose = auxiliary;
                        assisted = true;
                    }
                }
            }
            if pose == Pose::Upright && head_conflict {
                pose = Pose::Unknown;
                assisted = false;
            }
            let suspected = occupancy.update(
                if replay {
                    index as f64 / 4.
                } else {
                    begin.elapsed().as_secs_f64()
                },
                &heads,
                &c.bed,
                &c.rest_head_region,
                pose,
                count,
            );
            draw_overlay(&mut bytes, &people, &c.bed, &heads, c.multiple_confidence);
            let mut jpeg = vec![];
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 75).encode(
                &bytes,
                640,
                360,
                image::ExtendedColorType::Rgb8,
            )?;
            {
                let mut i = app.inner.lock().unwrap();
                if i.generation != generation || !i.view.running {
                    return Ok(());
                }
                i.frame = jpeg;
                i.frame_evidence = Some(json!({
                    "version":env!("CARGO_PKG_VERSION"),
                    "pose":pose,"people":people,"heads":heads,
                    "head_conflict":head_conflict,"assisted":assisted,
                    "head_multiple":head_multiple,"multi_recovering":multi_recovering,
                    "threshold":c.threshold,"person_confidence":c.person_confidence,
                    "multiple_confidence":c.multiple_confidence,
                    "bed":c.bed,"rest_head_region":c.rest_head_region
                }));
                i.view.last_frame =
                    Some(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64());
            }
            observe_frame(
                &app,
                pose,
                count,
                if replay {
                    index as f64 / 4.
                } else {
                    begin.elapsed().as_secs_f64()
                },
                replay,
                generation,
                uncertain,
                assisted,
            );
            let mut i = app.inner.lock().unwrap();
            if i.generation != generation {
                return Ok(());
            }
            if i.view.message == "视频流中断，5 秒后重新连接" {
                App::event(&mut i, "摄像头画面已恢复，等待重新确认躺卧");
            }
            i.view.suspected_lying = suspected;
            i.view.last_frame = Some(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64());
            i.view.video_position = if replay { index as f64 / 4. } else { 0. };
            i.view.fps = if replay {
                4.
            } else {
                (1. / last_analysis.elapsed().as_secs_f64()).min(c.analysis_fps as f64)
            };
            last_analysis = Instant::now();
            i.view.message = if multi_recovering {
                "多人状态恢复确认中，连续 3 秒无多人证据后恢复判断"
            } else if head_multiple {
                "检测到多个独立头部，已暂停坐起告警"
            } else if uncertain {
                "被子或遮挡影响人数判断，已暂停告警"
            } else if pose == Pose::Multiple {
                "多人场景，监测暂停"
            } else if replay {
                "视频逐帧分析中（不发送通知）"
            } else if suspected {
                "疑似卧床：卧床头部区域内持续检测到头部，躺卧姿态未确认"
            } else if pose == Pose::Unknown && count > 0 {
                "检测到人体，但姿态受遮挡影响"
            } else if pose == Pose::Unknown
                && heads.iter().any(|h| {
                    h.score >= 0.5
                        && c.rest_head_region
                            .as_ref()
                            .is_some_and(|r| region::overlap(r, h.bbox) >= 0.5)
                })
            {
                "检测到头部候选，但卧床线索仍不足，姿态未确认"
            } else if pose == Pose::Unknown && count == 0 {
                "未检测到清晰人体，姿态无法判断"
            } else {
                "监测中"
            }
            .into();
        }
    }
}
fn draw_overlay(
    rgb: &mut [u8],
    people: &[rules::Person],
    bed: &Region,
    heads: &[occupancy::Head],
    threshold: f32,
) {
    let mut rect = |x1: f32, y1: f32, x2: f32, y2: f32, color: [u8; 3]| {
        let x1 = (x1 * 639.).clamp(0., 639.) as usize;
        let x2 = (x2 * 639.).clamp(0., 639.) as usize;
        let y1 = (y1 * 359.).clamp(0., 359.) as usize;
        let y2 = (y2 * 359.).clamp(0., 359.) as usize;
        for x in x1..=x2 {
            for y in [y1, y2] {
                rgb[(y * 640 + x) * 3..(y * 640 + x) * 3 + 3].copy_from_slice(&color);
            }
        }
        for y in y1..=y2 {
            for x in [x1, x2] {
                rgb[(y * 640 + x) * 3..(y * 640 + x) * 3 + 3].copy_from_slice(&color);
            }
        }
    };
    for p in people {
        let b = p.bbox;
        rect(
            b[0],
            b[1],
            b[2],
            b[3],
            if rules::in_bed(p, bed) {
                [240, 160, 60]
            } else {
                [150, 150, 150]
            },
        );
    }
    for h in occupancy::distinct_heads(heads, bed, threshold) {
        let b = h.bbox;
        rect(b[0], b[1], b[2], b[3], [40, 190, 220]);
    }
    let points = bed.vertices();
    for k in 0..points.len() {
        let a = points[k];
        let b = points[(k + 1) % points.len()];
        let steps = ((b[0] - a[0]).abs() * 640.)
            .max((b[1] - a[1]).abs() * 360.)
            .ceil() as usize;
        for step in 0..=steps.max(1) {
            let t = step as f32 / steps.max(1) as f32;
            let x = ((a[0] + (b[0] - a[0]) * t) * 639.).clamp(0., 639.) as usize;
            let y = ((a[1] + (b[1] - a[1]) * t) * 359.).clamp(0., 359.) as usize;
            rgb[(y * 640 + x) * 3..(y * 640 + x) * 3 + 3].copy_from_slice(&[80, 210, 140]);
        }
    }
}
async fn action(
    State(app): State<App>,
    axum::extract::Path(action): axum::extract::Path<String>,
    Json(data): Json<Value>,
) -> Response {
    let result = (|| -> Result<()> {
        let mut i = app.inner.lock().unwrap();
        match action.as_str() {
            "start" => {
                if i.view.running {
                    bail!("请先停止监测");
                }
                let mode = data["mode"].as_str().unwrap_or("camera");
                if !["demo", "camera", "replay"].contains(&mode) {
                    bail!("模式不正确");
                }
                let requested = data["source"].as_str().unwrap_or("").trim();
                let source = if requested.is_empty() {
                    if mode == "replay" {
                        i.replay_source.clone()
                    } else {
                        i.camera_source.clone()
                    }
                } else {
                    requested.to_string()
                };
                if mode == "camera" && source != "0" {
                    let u = reqwest::Url::parse(&source)?;
                    if !["rtsp", "http", "https"].contains(&u.scheme()) {
                        bail!("请输入摄像头视频流地址");
                    }
                }
                if mode == "replay" {
                    let p = Path::new(&source);
                    if !p.is_absolute()
                        || !p.is_file()
                        || !p.extension().is_some_and(|e| {
                            ["mp4", "mov", "mkv", "avi", "m4v"]
                                .contains(&e.to_str().unwrap_or("").to_lowercase().as_str())
                        })
                    {
                        bail!("视频路径不正确");
                    }
                }
                if mode == "camera"
                    && source != i.camera_source
                    && (i.config.assist_lying.is_some() || i.config.assist_upright.is_some())
                {
                    bail!("请先保存新视频来源并重新校准");
                }
                i.generation += 1;
                let generation = i.generation;
                i.rule = Rule::default();
                i.samples.clear();
                i.view.assisted = false;
                i.frame.clear();
                i.preview.clear();
                i.view.last_preview = None;
                i.view.running = true;
                i.view.mode = Some(mode.into());
                i.view.pose = Pose::Unknown;
                i.view.people_uncertain = false;
                i.view.people = 0;
                i.view.armed = false;
                i.view.last_frame = None;
                i.view.suspected_lying = false;
                i.view.video_position = 0.;
                i.view.message = if mode == "demo" {
                    "演示模式"
                } else {
                    "正在连接视频来源…"
                }
                .into();
                App::event(&mut i, "已启动监测任务；多人时自动暂停");
                if mode == "demo" {
                    i.demo_pose = Pose::Unknown;
                    let app = app.clone();
                    i.task = Some(tokio::spawn(async move {
                        let mut timer = tokio::time::interval(Duration::from_millis(250));
                        loop {
                            timer.tick().await;
                            let i = app.inner.lock().unwrap();
                            if !i.view.running || i.generation != generation {
                                break;
                            }
                            // observe rechecks the latest shared selection under its lock.
                            let pose = i.demo_pose;
                            drop(i);
                            observe(
                                &app,
                                pose,
                                demo_count(pose),
                                Utc::now().timestamp_millis() as f64 / 1000.,
                                true,
                                generation,
                            );
                        }
                    }));
                } else {
                    let app = app.clone();
                    let replay = mode == "replay";
                    i.task = Some(tokio::spawn(async move {
                        let result = monitor(app.clone(), source, replay, generation).await;
                        let mut i = app.inner.lock().unwrap();
                        if i.generation == generation {
                            i.view.running = false;
                            i.rule.reset_baseline();
                            i.view.armed = false;
                            if result.is_err() {
                                i.view.pose = Pose::Unknown;
                                i.view.people_uncertain = false;
                                i.view.last_frame = None;
                                i.view.suspected_lying = false;
                                i.view.message =
                                    "无法启动视频监测，请检查视频来源、资源文件及摄像头权限".into();
                                let message = i.view.message.clone();
                                App::event(&mut i, message);
                                if !replay && i.config.active() {
                                    tokio::spawn(notify_reason(
                                        app.clone(),
                                        true,
                                        generation,
                                        None,
                                        Some("failure"),
                                    ));
                                }
                            }
                        }
                    }));
                }
            }
            "stop" => {
                i.generation += 1;
                if let Some(task) = i.task.take() {
                    task.abort();
                }
                i.care_book.resolve();
                persist_care(&i);
                i.rule = Rule::default();
                i.view.running = false;
                i.samples.clear();
                i.view.assisted = false;
                i.view.mode = None;
                i.view.pose = Pose::Unknown;
                i.view.people_uncertain = false;
                i.view.people = 0;
                i.view.armed = false;
                i.view.last_frame = None;
                i.view.suspected_lying = false;
                i.view.message = "已停止".into();
                i.preview.clear();
                i.view.last_preview = None;
            }
            "demo" => {
                if !i.view.running || i.view.mode.as_deref() != Some("demo") {
                    bail!("请先启动演示");
                }
                // Old browser tabs used to submit a pose on every state poll.
                // Only explicit selections may change the shared demo state.
                if data["select"].as_bool() != Some(true) {
                    return Ok(());
                }
                let pose: Pose = serde_json::from_value(data["pose"].clone())?;
                i.demo_pose = pose;
                let generation = i.generation;
                let now = Utc::now().timestamp_millis() as f64 / 1000.;
                drop(i);
                observe(
                    &app,
                    pose,
                    if pose == Pose::Multiple {
                        2
                    } else if pose == Pose::Unknown {
                        0
                    } else {
                        1
                    },
                    now,
                    true,
                    generation,
                );
                let mut i = app.inner.lock().unwrap();
                i.view.message = if pose == Pose::Multiple {
                    "多人场景，监测暂停"
                } else {
                    "演示模式"
                }
                .into();
            }
            "test-channel" => {
                let id = data["id"].as_str().unwrap_or("").to_string();
                if !i.push_channels.iter().any(|c| c.id == id) {
                    bail!("渠道不存在");
                }
                drop(i);
                tokio::spawn(notify_routes(app.clone(), false, 0, Some(id)));
            }
            "test-alert" => {
                drop(i);
                tokio::spawn(notify(app.clone(), false, 0));
            }
            _ => bail!("接口不存在"),
        }
        Ok(())
    })();
    match result {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(_) => error("操作无效，请检查视频来源、模式或当前运行状态"),
    }
}
fn root() -> PathBuf {
    brand::env("HOME").map(PathBuf::from).unwrap_or_else(|| {
        let cwd = std::env::current_dir().unwrap();
        if cwd.join("models").exists() {
            cwd
        } else {
            let dir = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf();
            if dir.join("../Resources/models").exists() {
                dir.join("../Resources")
            } else {
                dir
            }
        }
    })
}
async fn serve(ready: Option<std::sync::mpsc::Sender<()>>) -> Result<()> {
    let root = root();
    let data = data_dir(&root);
    std::fs::create_dir_all(&data)?;
    let mut config: Config = std::fs::read(data.join("config.json"))
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default();
    config.care.focus_sitting();
    config.validate()?;
    let mut saved = credentials::load_all(&data)?;
    let push_channels = channels::migrate(&saved, &config.channels);
    if saved.push_channels.is_none() {
        saved.push_channels = Some(push_channels.clone());
        saved.webhook.clear();
        saved.wecom.clear();
        saved.dingtalk.clear();
        saved.dingtalk_secret.clear();
        credentials::save_all(&data, &saved)?;
    }
    ort::init().with_name("pillowguard").commit()?;
    let password = brand::env("PASSWORD")
        .and_then(|v| v.into_string().ok())
        .filter(|s| s.len() >= 12);
    let bind = brand::env("BIND")
        .and_then(|v| v.into_string().ok())
        .unwrap_or("127.0.0.1".into());
    if bind != "127.0.0.1" && password.is_none() {
        bail!("局域网访问须设置至少 12 字符的 PILLOWGUARD_PASSWORD");
    }
    let history_file = data.join("history.json");
    let (events, history_path, history_status) = match history::load(&history_file) {
        Ok(events) => (events, Some(history_file), "历史记录已保存".to_string()),
        Err(_) => (
            vec![],
            None,
            "历史记录读取失败，原文件已保留；本次活动仅在内存中显示".to_string(),
        ),
    };
    let book_file = data.join("care-alerts.json");
    let (care_book, care_book_path) = if !book_file.exists() {
        (care::Book::default(), Some(book_file))
    } else {
        match std::fs::metadata(&book_file)
            .ok()
            .filter(|m| m.len() <= 2_000_000)
            .and_then(|_| std::fs::read(&book_file).ok())
            .and_then(|b| serde_json::from_slice::<care::Book>(&b).ok())
        {
            Some(mut book) => {
                book.alerts.truncate(100);
                (book, Some(book_file))
            }
            None => (care::Book::default(), None),
        }
    };
    let app = App {
        inner: Arc::new(Mutex::new(Inner {
            frame_evidence: None,
            captures: captures::Store::load(data.join("alert-captures.json")),
            care_book,
            escalation_id: None,
            care_book_path,
            samples: assist::Samples::default(),
            history_path,
            push_channels,
            config,
            view: View {
                events,
                history_status,
                ..View::default()
            },
            token: saved.token,
            webhook: saved.webhook,
            wecom: saved.wecom,
            dingtalk: saved.dingtalk,
            dingtalk_secret: saved.dingtalk_secret,
            camera_source: saved.camera_source,
            replay_source: saved.replay_source,
            frame: vec![],
            preview: vec![],
            rule: Rule::default(),
            generation: 0,
            task: None,
            busy: false,
            demo_pose: Pose::Unknown,
        })),
        root,
        data,
        password,
        wecom_sender: wecom_app::Sender::default(),
        client: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()?,
    };
    let router = Router::new()
        .route(
            "/",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    include_str!("../web/index.html"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/icon.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png")],
                    &include_bytes!("../assets/pillowguard-icon-256.png")[..],
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route("/api/state", get(state))
        .route("/api/care/ack", post(acknowledge))
        .route("/api/demo-event", post(demo_event))
        .route("/api/calibrate", post(calibrate))
        .route("/api/channel/reveal", post(channel_reveal))
        .route("/api/ha-check", post(ha_check))
        .route("/api/source/reveal", post(source_reveal))
        .route("/frame.jpg", get(frame))
        .route("/preview.jpg", get(preview))
        .route("/api/alert-capture/{id}", get(alert_capture))
        .route("/api/config", post(save))
        .route("/api/token/{action}", post(token_action))
        .route("/api/{action}", post(action))
        .layer(axum::extract::DefaultBodyLimit::max(65536))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(format!("{bind}:8765")).await?;
    tokio::spawn(watch_visibility(app.clone()));
    tokio::spawn(watch_care(app.clone()));
    let resume = {
        let i = app.inner.lock().unwrap();
        i.config.autostart && i.config.source_mode == "camera"
    };
    if resume {
        let response = action(
            State(app.clone()),
            axum::extract::Path("start".into()),
            Json(json!({"mode":"camera"})),
        )
        .await;
        let mut i = app.inner.lock().unwrap();
        App::event(
            &mut i,
            if response.status().is_success() {
                "已按设置自动恢复摄像头监测"
            } else {
                "自动开始监测失败，请检查已保存的视频源"
            },
        );
    }
    if let Some(ready) = ready {
        let _ = ready.send(());
    }
    println!("枕安 Rust 版：http://{bind}:8765");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
fn data_dir(root: &Path) -> PathBuf {
    if let Some(path) = brand::env("DATA_DIR") {
        return path.into();
    }
    if cfg!(feature = "desktop") {
        if cfg!(windows) {
            let parent = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default());
            return brand::desktop_data(&parent, "Shouwang", "枕安");
        }
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        return if cfg!(target_os = "macos") {
            brand::desktop_data(
                &home.join("Library/Application Support"),
                "Shouwang",
                "枕安",
            )
        } else {
            {
                let parent = home.join(".local/share");
                if !parent.join("pillowguard").exists() && parent.join("zhenan").is_dir() {
                    brand::desktop_data(&parent, "zhenan", "pillowguard")
                } else {
                    brand::desktop_data(&parent, "shouwang", "pillowguard")
                }
            }
        };
    }
    root.join("data")
}
fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--self-test" || a == "--head-self-test") {
        let root = root();
        ort::init().with_name("pillowguard-test").commit()?;
        let mut detector = vision::Detector::new(&root.join("models/yolov8n-pose.onnx"))?;
        let image = std::env::args()
            .skip_while(|a| a != "--self-test" && a != "--head-self-test")
            .nth(1);
        let rgb = if let Some(path) = image {
            let scaled = image::open(path)?
                .resize(1280, 720, image::imageops::FilterType::Triangle)
                .into_rgb8();
            let mut frame = image::RgbImage::new(1280, 720);
            image::imageops::overlay(
                &mut frame,
                &scaled,
                ((1280 - scaled.width()) / 2) as i64,
                ((720 - scaled.height()) / 2) as i64,
            );
            frame.into_raw()
        } else {
            vec![0; 1280 * 720 * 3]
        };
        if std::env::args().any(|a| a == "--head-self-test") {
            let mut head = crate::head::Detector::new(&root.join("models/head-detector.onnx"))?;
            // Head-only diagnostics must retain the source aspect ratio, just as
            // detect_region does for real camera crops.
            let frame = if let Some(path) = std::env::args()
                .skip_while(|a| a != "--head-self-test")
                .nth(1)
            {
                image::open(path)?.into_rgb8()
            } else {
                image::RgbImage::from_raw(1280, 720, rgb).unwrap()
            };
            let region: Region = std::env::args()
                .skip_while(|a| a != "--head-self-test")
                .nth(2)
                .map(|value| serde_json::from_str(&value))
                .transpose()?
                .unwrap_or_else(|| [0., 0., 1., 1.].into());
            region.validate()?;
            let heads = head.detect_region(&frame, &region)?;
            println!(
                "头部自检完成：线索={}, 最高分={:.2}",
                heads.len(),
                heads.iter().map(|h| h.score).fold(0f32, f32::max)
            );
            println!("候选框：{}", serde_json::to_string(&heads)?);
            return Ok(());
        }
        let diagnostic: Option<Config> = std::env::args()
            .skip_while(|a| a != "--self-test")
            .nth(2)
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        let c = diagnostic.unwrap_or_else(|| Config {
            bed: [0., 0., 1., 1.].into(),
            person_confidence: 0.5,
            multiple_confidence: 0.75,
            threshold: 0.65,
            rest_head_region: Some([0., 0., 1., 1.].into()),
            ..Config::default()
        });
        c.validate()?;
        let (people, _, heads, head_conflict) = detector.detect_detailed(
            &rgb,
            &c.bed,
            c.person_confidence,
            c.multiple_confidence,
            c.threshold,
            c.rest_head_region.as_ref(),
        )?;
        let selected =
            rules::monitor_people(&people, &c.bed, c.person_confidence, c.multiple_confidence);
        if std::env::var_os("PILLOWGUARD_DIAGNOSTIC_JOINTS").is_some() {
            for p in &selected {
                println!(
                    "{}",
                    json!({"bbox":p.bbox,"score":p.score,"points":p.points})
                );
            }
            println!("{}", json!({"heads":heads,"head_conflict":head_conflict}));
        }
        let (pose, body_count) = rules::classify(&selected, &c.bed, c.threshold);
        let (mut pose, count, head_multiple) =
            occupancy::head_safety(pose, body_count, &heads, &c.bed, c.multiple_confidence);
        println!("人体计数={body_count}, 多头部保护={head_multiple}");
        if head_conflict && pose == Pose::Upright {
            pose = Pose::Unknown;
        }
        println!(
            "Rust ONNX 自检完成：人数={count}, 姿态={pose:?}, 头部线索={}, 最高分={:.2}",
            heads.len(),
            heads.iter().map(|h| h.score).fold(0f32, f32::max)
        );
        return Ok(());
    }
    #[cfg(feature = "desktop")]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Err(e) = tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(serve(Some(tx)))
            {
                eprintln!("本地服务启动失败：{e}");
            }
        });
        tauri::Builder::default()
            .setup(move |app| {
                rx.recv_timeout(Duration::from_secs(15))
                    .map_err(|_| "本地服务启动失败，可能已运行其他实例")?;
                tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::External("http://127.0.0.1:8765/".parse()?),
                )
                .title("枕安")
                .inner_size(1280., 900.)
                .build()?;
                Ok(())
            })
            .run(tauri::generate_context!())
            .expect("桌面窗口启动失败");
        Ok(())
    }
    #[cfg(not(feature = "desktop"))]
    {
        tokio::runtime::Runtime::new()?.block_on(serve(None))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_duration_metadata() {
        assert_eq!(
            first_windows_camera(
                "[dshow] \"Front Camera\" (video)\n[dshow] \"Microphone\" (audio)"
            ),
            Some("Front Camera".into())
        );
        assert!(first_windows_camera("[dshow] \"Microphone\" (audio)").is_none());
        assert_eq!(media_duration("Duration: 01:02:03.50, start: 0"), 3723.5);
        assert_eq!(media_duration("Duration: N/A, start: 0"), 0.);
        assert_eq!(media_duration("no metadata"), 0.);
    }
    #[test]
    fn state_templates_and_test_are_distinct() {
        let mut c = Config::default();
        c.alert_text = "{time} 检测到{status}，床位内{people}人".into();
        assert_eq!(
            notification_text(&c, true, Pose::Upright, 1, "22:10", false),
            "22:10 检测到坐起，床位内1人"
        );
        assert!(notification_text(&c, false, Pose::Lying, 1, "22:10", false).contains("通知测试"));
        assert!(
            notification_text(&c, false, Pose::Multiple, 2, "22:10", false).contains("多人暂停")
        );
        c.test_text = "{status}，人数{people}".into();
        assert_eq!(
            notification_text(&c, false, Pose::Multiple, 2, "22:10", true),
            "遮挡，人数待确认，人数待确认"
        );
        c.alert_text = "{time}".repeat(42);
        assert_eq!(
            notification_text(&c, true, Pose::Upright, 1, "2026-10-18 22:10:00", false)
                .chars()
                .count(),
            255
        );
    }
    #[test]
    fn analysis_frequency_migrates_validates_and_survives_config_reload() {
        let legacy: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.analysis_fps, 4);
        assert!(!legacy.realtime_preview);

        let dir = std::env::temp_dir().join(format!(
            "pillowguard-fps-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for fps in [1, 2, 3, 4] {
            let c = Config {
                analysis_fps: fps,
                realtime_preview: true,
                ..Config::default()
            };
            assert!(c.validate().is_ok());
            write_config(&dir, &c).unwrap();
            let saved: Config =
                serde_json::from_slice(&std::fs::read(dir.join("config.json")).unwrap()).unwrap();
            assert_eq!(saved.analysis_fps, fps);
            assert!(saved.realtime_preview);
        }
        for fps in [0, 5, 255] {
            assert!(Config {
                analysis_fps: fps,
                ..Config::default()
            }
            .validate()
            .is_err());
        }
        assert!(serde_json::from_str::<Config>(r#"{"analysis_fps":1.5}"#).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn config_rejects_invalid() {
        let mut c = Config::default();
        assert!(c.validate().is_ok());
        c.schedule_end = c.schedule_start.clone();
        assert!(c.validate().is_err());
        c = Config::default();
        c.channels.clear();
        assert!(c.validate().is_err());
    }
    #[test]
    fn multi_schedule_validation_and_legacy_fallback() {
        let mut c = Config::default();
        c.schedule_mode = "custom".into();
        assert_eq!(c.time_windows()[0].start, "22:00");
        c.schedule_windows = Some(vec![]);
        assert!(c.validate().is_err());
        let w = rules::TimeWindow {
            start: "13:00".into(),
            end: "14:00".into(),
        };
        c.schedule_windows = Some(vec![w.clone(); 17]);
        assert!(c.validate().is_err());
        c.schedule_windows = Some(vec![w.clone(); 16]);
        assert!(c.validate().is_ok());
        c.schedule_windows = Some(vec![rules::TimeWindow {
            start: "25:00".into(),
            end: "07:00".into(),
        }]);
        assert!(c.validate().is_err());
        c.schedule_windows = Some(vec![rules::TimeWindow {
            start: "13:00".into(),
            end: "13:00".into(),
        }]);
        assert!(c.validate().is_err());
    }
    #[test]
    fn no_secrets_serialized() {
        let c = serde_json::to_value(Config::default()).unwrap();
        assert!(c.get("token").is_none());
        assert!(c.get("webhook_url").is_none());
    }
}
