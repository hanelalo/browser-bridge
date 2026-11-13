# 有头沙箱镜像：Debian + Debian Chromium（非品牌，指纹主流）+ Xvfb + openbox + noVNC
# 扩展不打入镜像：运行时把宿主机的 extension/dist/chrome-mv3 只读挂载到 /opt/bridge-ext，
# 由用户经 noVNC 在 Chromium 里手动「加载已解压的扩展程序」安装（品牌版 Chrome 已禁用
# --load-extension，Preferences 种入被完整性校验清除，见 docs/browser-slots.md §14）。
# profile 通过 bind mount 持久化到宿主机，容器删除不丢登录态。
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    chromium xvfb dbus-x11 openbox \
    x11vnc novnc websockify \
    fonts-liberation fonts-noto-cjk \
    ca-certificates iproute2 procps \
    && rm -rf /var/lib/apt/lists/*

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV DISPLAY=:99
ENTRYPOINT ["/entrypoint.sh"]
