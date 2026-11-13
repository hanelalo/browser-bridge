#!/bin/sh
# 有头沙箱入口：Xvfb 虚拟显示 + openbox + Debian Chromium（带验证专用 flag 集）
set -x
Xvfb :99 -screen 0 1440x900x24 -nolisten tcp &
sleep 1
openbox &

CHROME_FLAGS="--no-sandbox --disable-dev-shm-usage \
  --no-first-run --no-default-browser-check --disable-session-crashed-bubble \
  --disable-background-timer-throttling --disable-backgrounding-occluded-windows \
  --disable-renderer-backgrounding --disable-features=IntensiveWakeUpThrottling \
  --enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader"

# Debian Chromium 未品牌化，--load-extension 仍可用；正式实现阶段换 Preferences 种入
exec chromium $CHROME_FLAGS --load-extension=/opt/bridge-ext --user-data-dir=/tmp/profile
