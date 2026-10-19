//! Drain live video independently of inference. Only the latest complete frame
//! is retained; replay keeps its separate, lossless frame-by-frame reader.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::watch,
    task::JoinHandle,
};

pub fn analysis_interval(fps: u8) -> Duration {
    Duration::from_secs_f64(1. / fps.clamp(1, 4) as f64)
}

#[derive(Clone)]
enum State {
    Waiting,
    Frame(Arc<Vec<u8>>),
    End(&'static str),
}

pub struct LatestFrames {
    receiver: watch::Receiver<State>,
    reader: JoinHandle<()>,
}
impl LatestFrames {
    pub fn new<R: AsyncRead + Unpin + Send + 'static>(
        mut input: R,
        frame_size: usize,
        timeout: Duration,
    ) -> Self {
        let (sender, receiver) = watch::channel(State::Waiting);
        let reader = tokio::spawn(async move {
            loop {
                let mut bytes = vec![0; frame_size];
                let error = match tokio::time::timeout(timeout, input.read_exact(&mut bytes)).await
                {
                    Ok(Ok(_)) => {
                        sender.send_replace(State::Frame(Arc::new(bytes)));
                        continue;
                    }
                    Err(_) => "连续 12 秒未收到完整视频帧",
                    Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        "视频解码进程输出结束"
                    }
                    Ok(Err(_)) => "视频管道读取失败",
                };
                sender.send_replace(State::End(error));
                break;
            }
        });
        Self { receiver, reader }
    }
    pub async fn next(&mut self) -> Result<Vec<u8>, &'static str> {
        self.receiver
            .changed()
            .await
            .map_err(|_| "视频取流任务结束")?;
        match self.receiver.borrow_and_update().clone() {
            State::Frame(bytes) => Ok((*bytes).clone()),
            State::End(reason) => Err(reason),
            State::Waiting => Err("视频尚未就绪"),
        }
    }
}
impl Drop for LatestFrames {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

// Never expose decoder stderr: it may contain the authenticated camera URL.
// Retain only a fixed error category while continuously draining the pipe.
pub struct DecoderDiagnostics {
    category: Arc<Mutex<&'static str>>,
    reader: JoinHandle<()>,
}
impl DecoderDiagnostics {
    pub fn new<R: AsyncRead + Unpin + Send + 'static>(mut input: R) -> Self {
        let category = Arc::new(Mutex::new("未收到可归类的解码错误"));
        let result = category.clone();
        let reader = tokio::spawn(async move {
            let mut bytes = [0; 4096];
            let mut tail = Vec::new();
            while let Ok(n) = input.read(&mut bytes).await {
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&bytes[..n]);
                if let Some(value) = decoder_category(&String::from_utf8_lossy(&tail)) {
                    *result.lock().unwrap() = value;
                }
                if tail.len() > 512 {
                    tail.drain(..tail.len() - 512);
                }
            }
        });
        Self { category, reader }
    }
    pub fn summary(&self) -> &'static str {
        *self.category.lock().unwrap()
    }
}
impl Drop for DecoderDiagnostics {
    fn drop(&mut self) {
        self.reader.abort();
    }
}
fn decoder_category(text: &str) -> Option<&'static str> {
    let s = text.to_ascii_lowercase();
    if s.contains("timed out") || s.contains("timeout") {
        Some("网络接收超时")
    } else if s.contains("connection reset")
        || s.contains("broken pipe")
        || s.contains("end of file")
    {
        Some("远端连接关闭或重置")
    } else if s.contains("401 unauthorized") || s.contains("403 forbidden") {
        Some("摄像头拒绝认证或访问")
    } else if s.contains("connection refused") {
        Some("摄像头拒绝连接")
    } else if s.contains("invalid data")
        || s.contains("corrupt")
        || s.contains("error while decoding")
    {
        Some("视频数据或解码异常")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    #[test]
    fn analysis_rate_uses_seconds_instead_of_frame_counts() {
        assert_eq!(analysis_interval(1), Duration::from_secs(1));
        assert_eq!(analysis_interval(2), Duration::from_millis(500));
        assert_eq!(analysis_interval(4), Duration::from_millis(250));
    }
    #[tokio::test]
    async fn slow_inference_never_blocks_producer_or_replays_a_backlog() {
        let (mut producer, input) = tokio::io::duplex(8);
        let mut frames = LatestFrames::new(input, 4, Duration::from_secs(12));
        producer.write_all(&[1; 4]).await.unwrap();
        assert_eq!(frames.next().await.unwrap(), vec![1; 4]);
        // A small pipe would block unless the independent reader keeps draining
        // while inference (the consumer) does no reads at all.
        tokio::time::timeout(Duration::from_secs(2), async {
            for n in 2..=100 {
                producer.write_all(&[n; 4]).await.unwrap();
            }
            // Acknowledge the reader consumed all bytes, without relying on sleeps.
            loop {
                if matches!(&*frames.receiver.borrow(), State::Frame(b) if b[0] == 100) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(frames.next().await.unwrap(), vec![100; 4]);
        producer.write_all(&[101; 4]).await.unwrap();
        assert_eq!(frames.next().await.unwrap(), vec![101; 4]);
    }
    #[tokio::test]
    async fn partial_frame_is_not_delivered_and_eof_is_reported() {
        let (mut producer, input) = tokio::io::duplex(8);
        let mut frames = LatestFrames::new(input, 4, Duration::from_secs(12));
        producer.write_all(&[1; 2]).await.unwrap();
        drop(producer);
        assert_eq!(frames.next().await.unwrap_err(), "视频解码进程输出结束");
    }
    #[tokio::test]
    async fn stalled_input_times_out_and_drop_stops_the_reader() {
        let (_producer, input) = tokio::io::duplex(8);
        let mut frames = LatestFrames::new(input, 4, Duration::from_millis(20));
        assert!(frames
            .next()
            .await
            .unwrap_err()
            .contains("未收到完整视频帧"));
        let (mut producer, input) = tokio::io::duplex(8);
        let frames = LatestFrames::new(input, 4, Duration::from_secs(12));
        drop(frames);
        tokio::task::yield_now().await;
        assert!(producer.write_all(&[0; 9]).await.is_err());
    }
    #[tokio::test]
    async fn decoder_errors_are_drained_and_only_fixed_categories_survive() {
        let (mut producer, input) = tokio::io::duplex(32);
        let diagnostics = DecoderDiagnostics::new(input);
        producer
            .write_all(b"rtsp://admin:private-password@camera/ Connection res")
            .await
            .unwrap();
        producer.write_all(b"et by peer\n").await.unwrap();
        drop(producer);
        tokio::time::timeout(Duration::from_secs(2), async {
            while diagnostics.summary() != "远端连接关闭或重置" {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!diagnostics.summary().contains("private-password"));
        assert_eq!(
            decoder_category("Connection timed out"),
            Some("网络接收超时")
        );
        assert_eq!(
            decoder_category("401 Unauthorized"),
            Some("摄像头拒绝认证或访问")
        );
        assert_eq!(decoder_category("Unknown text rtsp://secret"), None);
    }
}
