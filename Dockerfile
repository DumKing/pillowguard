FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY web ./web
COPY assets ./assets
RUN cargo build --release --locked

FROM debian:bookworm-slim
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg ca-certificates curl libgomp1 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
RUN mkdir -p bin models data && \
    curl -fL https://huggingface.co/Xenova/yolov8-pose-onnx/resolve/main/yolov8n-pose.onnx -o models/yolov8n-pose.onnx && \
    echo '04f6d2416266f2aba6c5ba8b26de33ed9eba3279f972ac02e33f9f1366547586  models/yolov8n-pose.onnx' | sha256sum -c - && \
    curl -fL https://s3.ap-northeast-2.wasabisys.com/pinto-model-zoo/426_YOLOX-Body-Head-Hand/resources_n.tar.gz -o /tmp/heads.tar.gz && \
    tar -xzOf /tmp/heads.tar.gz yolox_n_body_head_hand_post_0461_0.4428_1x3x256x320.onnx > models/head-detector.onnx && \
    echo 'f995ac5315917f54654b4ca47f9e574be7bdb636775f289fb8b82c25be040233  models/head-detector.onnx' | sha256sum -c - && \
    rm /tmp/heads.tar.gz && \
    useradd -u 10001 -M pillowguard && chown -R 10001:10001 /app/data
COPY --from=build /src/target/release/pillowguard /app/pillowguard
COPY assets/HeadDetector-LICENSE.txt /app/HeadDetector-LICENSE.txt
ENV PILLOWGUARD_HOME=/app PILLOWGUARD_DATA_DIR=/app/data PILLOWGUARD_FFMPEG=/usr/bin/ffmpeg PILLOWGUARD_BIND=0.0.0.0
USER 10001
EXPOSE 8765
VOLUME ["/app/data"]
CMD ["/app/pillowguard"]
