import { defineConfig } from 'wxt';

export default defineConfig({
  modules: ['@wxt-dev/module-vue'],
  manifest: {
    name: 'Browser Bridge',
    description: '通过 WebSocket 桥接本地工具与真实浏览器（无需 CDP）',
    permissions: ['tabs', 'scripting', 'activeTab'],
    host_permissions: ['<all_urls>'],
  },
});
