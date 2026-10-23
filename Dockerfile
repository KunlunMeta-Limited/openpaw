FROM rust:1.88-bookworm AS build
ARG HTTP_PROXY
ARG HTTPS_PROXY
ENV HTTP_PROXY=$HTTP_PROXY HTTPS_PROXY=$HTTPS_PROXY
WORKDIR /src
ENV RUSTUP_TOOLCHAIN=1.88.0
COPY . .
RUN cargo build --release --locked --features sidecar -p openpaw-cli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/openpaw /usr/local/bin/openpaw
ENV OPENPAW_HOME=/data/openpaw \
    OPENPAW_SIDECAR_HOST=0.0.0.0 \
    OPENPAW_SIDECAR_PORT=8788
RUN mkdir -p /data/openpaw
VOLUME ["/data/openpaw"]
EXPOSE 8788
CMD ["openpaw", "--sidecar", "--sidecar-transport", "http"]
