# 可行性验证镜像：Debian + Debian Chromium（无品牌版，--load-extension 可用）
# + Xvfb 虚拟显示 + openbox 最小窗口管理器 + 中文字体
# 扩展产物在构建前由 seed_profile.mjs 补丁好 WS 地址后拷入本目录
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    chromium xvfb dbus-x11 openbox \
    fonts-liberation fonts-noto-cjk \
    ca-certificates iproute2 procps \
    && rm -rf /var/lib/apt/lists/*

COPY chrome-mv3 /opt/bridge-ext
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV DISPLAY=:99
ENTRYPOINT ["/entrypoint.sh"]
