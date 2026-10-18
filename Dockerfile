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
    useradd -u 10001 -M laoren && chown -R 10001:10001 /app/data
COPY --from=build /src/target/release/laoren /app/laoren
ENV LAOREN_HOME=/app LAOREN_DATA_DIR=/app/data LAOREN_FFMPEG=/usr/bin/ffmpeg LAOREN_BIND=0.0.0.0
USER 10001
EXPOSE 8765
VOLUME ["/app/data"]
CMD ["/app/laoren"]
