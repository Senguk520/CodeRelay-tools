// 单一版本源同步脚本
//
// 版本号唯一来源是 package.json 的 "version" 字段。本脚本将其同步到：
//   - src-tauri/tauri.conf.json 的 version（Tauri 打包版本）
//   - src-tauri/Cargo.toml 的 version（Rust crate 版本）
//   - src-tauri/Cargo.lock 中 coderelay 条目的 version（cargo 只在依赖变化时
//     才回头核对自身版本，不能指望它顺手改掉；而锁文件是提交进仓库的）
//   - package-lock.json 的两处项目版本（根级 version 与 packages[""] 内的 version）
//   - README.md / README.en.md 的 version 徽章
// 前端展示的版本号由 vite.config.ts 通过 define 注入，无需在此处理。
//
// 升级版本的入口是 `npm version patch --no-git-tag-version`（或 CI 里的同样一步），
// 它会改掉 package.json 与 package-lock.json；本脚本负责剩下的全部副本。
//
// 用法：node scripts/sync-version.mjs
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const pkg = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf-8'));
const version = pkg.version;
if (!/^\d+\.\d+\.\d+/.test(version)) {
  throw new Error(`package.json 的 version 非法: ${version}`);
}

// 统一收尾：同步成功写日志，目标缺失或结构变化一律降级为告警而不静默跳过，
// 这样"版本号悄悄漂了"会以可见的方式暴露出来。
function syncFile({ label, path, pattern, replacement }) {
  const source = readFileSync(path, 'utf-8');
  if (!pattern.test(source)) {
    console.warn(`[sync-version] ${label} 未找到可替换的版本号，未做替换`);
    return;
  }
  const next = source.replace(pattern, replacement);
  if (next === source) {
    console.log(`[sync-version] ${label} 已是最新 (${version})`);
    return;
  }
  writeFileSync(path, next);
  console.log(`[sync-version] ${label} -> ${version}`);
}

// 1. 同步 tauri.conf.json
const tauriConfPath = resolve(root, 'src-tauri/tauri.conf.json');
const tauriConf = JSON.parse(readFileSync(tauriConfPath, 'utf-8'));
if (tauriConf.version !== version) {
  tauriConf.version = version;
  writeFileSync(tauriConfPath, JSON.stringify(tauriConf, null, 2) + '\n');
  console.log(`[sync-version] tauri.conf.json -> ${version}`);
} else {
  console.log(`[sync-version] tauri.conf.json 已是最新 (${version})`);
}

// 2. 同步 Cargo.toml（仅替换 [package] 段顶层首个 version 行）
syncFile({
  label: 'Cargo.toml',
  path: resolve(root, 'src-tauri/Cargo.toml'),
  pattern: /^version\s*=\s*"[^"]*"$/m,
  replacement: `version = "${version}"`,
});

// 3. 同步 Cargo.lock 中 coderelay 自身的条目
//
// lock 里有一两百个 [[package]] 块，每块都有自己的 name/version，所以不能只匹配
// 一行 version —— 必须把"块头 + name"一起锚定，才只命中本项目那一条。
// 锚定串里含换行，故显式吃掉 \r，避免 Windows 检出（autocrlf）下匹配失败。
syncFile({
  label: 'Cargo.lock',
  path: resolve(root, 'src-tauri/Cargo.lock'),
  pattern: /(\[\[package\]\]\r?\nname = "coderelay"\r?\nversion = ")[^"]*(")/,
  replacement: `$1${version}$2`,
});

// 4. 同步 package-lock.json 的两处项目版本
//
// lock 里的版本号是同一事实的第 4 个副本（根级 `version` 与 `packages[""]` 内的
// `version`），不纳入脚本就会在每次发版后继续漂。这里刻意不用
// JSON.parse + JSON.stringify：npm 生成的键序与缩进就是 lock 的规范形态，重排会
// 产生覆盖整个文件的无关 diff。改用定向替换，只改版本值、其余字节逐字不变。
//
// 定位策略（依赖包条目里也有大量同名 `version`，一律不能动）：
//   - 第 1 处：在文件开头到 `"packages"` 键之间的片段里替换首个匹配；
//   - 第 2 处：在 `"packages"` 键往后的片段里替换首个匹配，即空键条目 `""` 内的那个。
// 两段各自只取第一个匹配，且匹配整行（`^...$`），因此不会误伤依赖项。
const lockPath = resolve(root, 'package-lock.json');
const lock = readFileSync(lockPath, 'utf-8');
const packagesKey = lock.indexOf('"packages"');
if (packagesKey === -1) {
  console.warn('[sync-version] 未在 package-lock.json 中找到 packages 段');
} else {
  const versionLine = /^(\s*"version"\s*:\s*)"[^"]*"(\s*,?\s*)$/m;
  const [lockHead, lockTail] = [lock.slice(0, packagesKey), lock.slice(packagesKey)];
  // 两段都必须存在可替换的 version 行，否则说明 lock 结构已变，宁可告警也不要静默跳过。
  if (!versionLine.test(lockHead) || !versionLine.test(lockTail)) {
    console.warn('[sync-version] package-lock.json 结构与预期不符，未做替换');
  } else {
    const replaceFirstVersion = (segment) => segment.replace(versionLine, `$1"${version}"$2`);
    const next = replaceFirstVersion(lockHead) + replaceFirstVersion(lockTail);
    if (next !== lock) {
      writeFileSync(lockPath, next);
      console.log(`[sync-version] package-lock.json -> ${version}`);
    } else {
      console.log(`[sync-version] package-lock.json 已是最新 (${version})`);
    }
  }
}

// 5. 同步两个 README 的版本徽章
//
// 只认 shields.io 的 version-…-blue 这一段：README 正文里的版本号是给人看的说明，
// 不参与同步，避免把叙述文字里的数字一起改掉。
const badgePattern = /(img\.shields\.io\/badge\/version-)[^-)]*(-blue)/;
for (const readme of ['README.md', 'README.en.md']) {
  syncFile({
    label: readme,
    path: resolve(root, readme),
    pattern: badgePattern,
    replacement: `$1${version}$2`,
  });
}

console.log(`[sync-version] 完成，版本号统一为 ${version}`);
