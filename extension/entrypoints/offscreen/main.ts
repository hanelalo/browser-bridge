// offscreen 唤醒源：断开期间每 5 秒唤醒 service worker 去重连 bridge server。
// offscreen 文档的生命周期独立于 worker（不会被 30 秒空闲回收），
// 但只能使用 runtime API，所以真正的连接与请求处理仍在 worker 里。
function wake(): void {
  // worker 可能正在休眠：这条消息会把它拉起来执行 onMessage -> connect()
  chrome.runtime.sendMessage({ type: 'wake' }).catch(() => {});
}

wake();
setInterval(wake, 5000);
