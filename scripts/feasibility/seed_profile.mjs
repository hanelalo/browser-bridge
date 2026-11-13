#!/usr/bin/env node
// 可行性验证：把扩展产物拷贝到独立目录、补丁 WS 地址、计算 unpacked 扩展 ID、
// 以 Secure Preferences 种入的方式"预装"进一个全新的 Chrome user-data-dir。
//
// 用法：
//   node seed_profile.mjs --ext-src <扩展产物目录> --profile <user-data-dir> --ws-url ws://... [--ext-dst <目录>] [--location <4|8>]
//
// 原理（macOS/Windows 实测结论见 docs/browser-slots.md §8）：
// 1. unpacked 扩展的 ID = SHA256(路径 UTF-8) 前 16 字节，每个 nibble 映射为 'a'-'p'
//    （对应 Chromium crx_file::id_util::GenerateIdForPath / ConvertHexadecimalToIDAlphabet）
// 2. 扩展设置必须写在 Default/"Secure Preferences"（写 Preferences 会被 Chrome 丢弃）
// 3. macOS 上 protection.macs 对种入条目不强制（CfT 真实安装也无 MAC）
// 4. 产物里的 ws://127.0.0.1:9225 全部替换为 --ws-url，实现"每实例指向自己的 server"

import { cpSync, mkdirSync, readFileSync, writeFileSync, rmSync, readdirSync, statSync, existsSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';

const args = process.argv.slice(2);
function argOf(name, fallback) {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : fallback;
}

const extSrc = resolve(argOf('--ext-src', 'extension/dist/chrome-mv3'));
const profile = resolve(argOf('--profile'));
const wsUrl = argOf('--ws-url');
const extDst = resolve(argOf('--ext-dst', join(profile, 'ext')));
const location = Number(argOf('--location', '4')); // 4=UNPACKED, 8=COMMAND_LINE
if (!profile || !wsUrl) {
  console.error('usage: seed_profile.mjs --profile <user-data-dir> --ws-url <url> [--ext-src <dir>] [--ext-dst <dir>] [--location <4|8>]');
  process.exit(1);
}

// 1. 拷贝扩展产物
rmSync(extDst, { recursive: true, force: true });
cpSync(extSrc, extDst, { recursive: true });

// 2. 补丁 WS 地址（background.js 等）
const origUrl = 'ws://127.0.0.1:9225';
function patchJs(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) patchJs(p);
    else if (name.endsWith('.js')) {
      const text = readFileSync(p, 'utf8');
      if (text.includes(origUrl)) writeFileSync(p, text.replaceAll(origUrl, wsUrl));
    }
  }
}
patchJs(extDst);

// 3. 计算 unpacked 扩展 ID
const digest = createHash('sha256').update(Buffer.from(extDst, 'utf8')).digest();
const extId = [...digest.subarray(0, 16)]
  .flatMap((b) => [(b >> 4) & 0xf, b & 0xf])
  .map((nibble) => String.fromCharCode(97 + nibble))
  .join('');

// 4. 从 manifest 推导权限（api 列表 + host 列表），字段结构对照 CfT --load-extension 真实安装
const manifest = JSON.parse(readFileSync(join(extDst, 'manifest.json'), 'utf8'));
const isHostPattern = (p) => p.includes('://') || p.includes('*');
const apiPerms = (manifest.permissions ?? []).filter((p) => !isHostPattern(p));
const hostPerms = [...(manifest.host_permissions ?? []), ...(manifest.permissions ?? []).filter(isHostPattern)];
const perms = { api: apiPerms, explicit_host: hostPerms, manifest_permissions: [], scriptable_host: [] };
// Chrome 时间：1601-01-01 起的微秒
const chromeTime = String((Date.now() + 11644473600000) * 1000);

const entry = {
  account_extension_type: 0,
  active_permissions: perms,
  content_settings: [],
  creation_flags: 38,
  disable_reasons: [],
  first_install_time: chromeTime,
  from_webstore: false,
  granted_permissions: perms,
  incognito_content_settings: [],
  incognito_preferences: {},
  install_time: chromeTime,
  last_update_time: chromeTime,
  location,
  newAllowFileAccess: true,
  path: extDst,
  preferences: {},
  regular_only_preferences: {},
  was_installed_by_default: false,
  was_installed_by_oem: false,
  withholding_permissions: false,
};

// 5. 写 Secure Preferences（扩展设置）+ Preferences（developer_mode 等未跟踪项）
const prefDir = join(profile, 'Default');
mkdirSync(prefDir, { recursive: true });

const securePrefsPath = join(prefDir, 'Secure Preferences');
let securePrefs = {};
if (existsSync(securePrefsPath)) {
  try { securePrefs = JSON.parse(readFileSync(securePrefsPath, 'utf8')); } catch {}
}
securePrefs.extensions = { ...(securePrefs.extensions ?? {}), settings: { ...(securePrefs.extensions?.settings ?? {}), [extId]: entry } };
writeFileSync(securePrefsPath, JSON.stringify(securePrefs));

const prefsPath = join(prefDir, 'Preferences');
let prefs = {};
if (existsSync(prefsPath)) {
  try { prefs = JSON.parse(readFileSync(prefsPath, 'utf8')); } catch {}
}
prefs.extensions = { ...(prefs.extensions ?? {}), ui: { ...(prefs.extensions?.ui ?? {}), developer_mode: true } };
prefs.profile = { ...(prefs.profile ?? {}), exit_type: 'Normal', exited_cleanly: true };
writeFileSync(prefsPath, JSON.stringify(prefs));

console.log(JSON.stringify({ extDst, extId, profile, wsUrl, location }, null, 2));
