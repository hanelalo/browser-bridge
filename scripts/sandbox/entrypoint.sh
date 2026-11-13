#!/bin/sh
# 沙箱入口：Xvfb 虚拟显示 + openbox 窗口管理 + x11vnc/noVNC 远程查看 + Chromium
# - 焦点隔离：所有窗口只存在于容器内的 X server，抢焦点抢不到宿主机屏幕
# - 降速 flag 全开：后台 tab / 遮挡窗口不降速，页面加载完整性向"前台"看齐
# - profile 在 /data/profile（宿主机 bind mount），扩展手动安装后长期有效
set -x
Xvfb :99 -screen 0 1440x900x24 -nolisten tcp &
sleep 1
openbox &

# noVNC：浏览器里直接看沙箱桌面（首次装扩展、人工登录、过验证码都用它）
x11vnc -display :99 -rfbport 5900 -nopw -forever -shared -quiet &
websockify --web=/usr/share/novnc 6080 localhost:5900 &

CHROME_FLAGS="--no-sandbox --disable-dev-shm-usage \
  --no-first-run --no-default-browser-check --disable-session-crashed-bubble \
  --disable-background-timer-throttling --disable-backgrounding-occluded-windows \
  --disable-renderer-backgrounding --disable-features=IntensiveWakeUpThrottling \
  --enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader"

exec chromium $CHROME_FLAGS --user-data-dir=/data/profile about:blank
