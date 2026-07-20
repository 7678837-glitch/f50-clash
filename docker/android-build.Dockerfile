ARG RUST_VERSION=1.95.0
ARG NDK_VERSION=26.3.11579264
ARG CARGO_NDK_VERSION=4.1.2
ARG CMDLINE_TOOLS_SHA256=2d2d50857e4eb553af5a6dc3ad507a17adf43d115264b1afc116f95c92e5e258

FROM debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818 AS android-sdk

ARG NDK_VERSION
ARG CMDLINE_TOOLS_SHA256
ENV ANDROID_HOME=/opt/android-sdk \
    ANDROID_SDK_ROOT=/opt/android-sdk \
    DEBIAN_FRONTEND=noninteractive \
    TZ=UTC

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl openjdk-17-jre-headless unzip \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p "${ANDROID_HOME}/cmdline-tools" \
    && curl -fsSL -o /tmp/cmdline-tools.zip https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip \
    && echo "${CMDLINE_TOOLS_SHA256}  /tmp/cmdline-tools.zip" | sha256sum -c - \
    && unzip -q /tmp/cmdline-tools.zip -d /tmp/cmdline-tools \
    && mv /tmp/cmdline-tools/cmdline-tools "${ANDROID_HOME}/cmdline-tools/latest" \
    && rm -rf /tmp/cmdline-tools /tmp/cmdline-tools.zip

RUN yes | "${ANDROID_HOME}/cmdline-tools/latest/bin/sdkmanager" --licenses >/dev/null \
    && "${ANDROID_HOME}/cmdline-tools/latest/bin/sdkmanager" \
        "platforms;android-33" \
        "ndk;${NDK_VERSION}"

FROM rust:${RUST_VERSION}-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS builder

ARG NDK_VERSION
ARG CARGO_NDK_VERSION
ENV ANDROID_HOME=/opt/android-sdk \
    ANDROID_SDK_ROOT=/opt/android-sdk \
    ANDROID_NDK_HOME=/opt/android-sdk/ndk/${NDK_VERSION} \
    CARGO_TERM_COLOR=always \
    DEBIAN_FRONTEND=noninteractive \
    TZ=UTC
ENV PATH="${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt/linux-x86_64/bin:${PATH}"

COPY --from=android-sdk /opt/android-sdk /opt/android-sdk

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates file \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add rustfmt clippy \
    && rustup target add aarch64-linux-android \
    && cargo install cargo-ndk --version "${CARGO_NDK_VERSION}" --locked

WORKDIR /workspace
