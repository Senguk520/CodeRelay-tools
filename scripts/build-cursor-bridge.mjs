#!/usr/bin/env node
// 跨平台 cursor-bridge 构建脚本。
// 原仓库只提供 scripts/build-cursor-bridge.ps1（PowerShell），在 macOS / Linux 上无法
// 执行。本脚本用 Node 重写同一套逻辑，Windows / macOS / Linux 通用。
//
// 与 build-cursor-bridge.ps1 的对应关系：
//   1. 取 rustc 的 host target triple（同 build-sidecar.mjs 的兜底策略）
//   2. 目标目录：优先 CODERELAY_CURSOR_TARGET_DIR；否则用 checkout 内
//      sidecars/cursor-bridge/target（上游注释明确该目录已被 .gitignore 覆盖）。
//      上游默认的 F:/target-cursor-bridge 是作者机器专属，非 Windows 直接跳过。
//   3. cargo build --release -p coderelay-cursor-bridge（bridge 是独立 workspace，
//      不与 src-tauri 共用 target，避免互相清逐依赖产物）
//   4. 产物拷贝为 sidecars/cursor-bridge/bin/cursor-bridge-<triple>[.exe]

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, renameSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptDir, '..');
const bridgeDir = join(root, 'sidecars', 'cursor-bridge');

const FALLBACK_TRIPLES = {
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'win32-x64': 'x86_64-pc-windows-msvc',
  'win32-arm64': 'aarch64-pc-windows-msvc',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
};

function resolveTargetTriple() {
  try {
    const out = execFileSync('rustc', ['-vV'], { encoding: 'utf8' });
    const line = out.split('\n').find((l) => l.startsWith('host:'));
    if (line) {
      const triple = line.slice('host:'.length).trim();
      if (triple) {
        return triple;
      }
    }
  } catch {
    // rustc 不在 PATH，走兜底
  }
  const key = `${process.platform}-${process.arch}`;
  const fallback = FALLBACK_TRIPLES[key];
  if (!fallback) {
    throw new Error(`无法确定 Rust target triple（rustc 不可用，且 ${key} 不在兜底表中）`);
  }
  console.warn(`[build-cursor-bridge] rustc 不可用，使用兜底 triple: ${fallback}`);
  return fallback;
}

function resolveTargetDir() {
  if (process.env.CODERELAY_CURSOR_TARGET_DIR) {
    return resolve(process.env.CODERELAY_CURSOR_TARGET_DIR);
  }
  // 上游在 Windows 上默认 F:/target-cursor-bridge；其他平台直接用 checkout 内目录。
  return join(bridgeDir, 'target');
}

function replaceBinary(from, to) {
  // Windows 上运行中的进程映射着旧镜像，不能原地覆盖：先落 .new 再原子改名。
  const staged = `${to}.new`;
  rmSync(staged, { force: true });
  copyFileSync(from, staged);
  try {
    renameSync(staged, to);
  } catch (error) {
    const aside = `${to}.old-${Date.now()}`;
    renameSync(to, aside);
    renameSync(staged, to);
    console.warn(`[build-cursor-bridge] 旧二进制被占用，已挪到 ${aside}：${error.message}`);
  }
}

const targetTriple = resolveTargetTriple();
const extension = targetTriple.includes('windows') ? '.exe' : '';
const targetDir = resolveTargetDir();
const built = join(targetDir, 'release', `cursor-bridge${extension}`);
const bin = join(bridgeDir, 'bin', `cursor-bridge-${targetTriple}${extension}`);

if (!existsSync(join(bridgeDir, 'Cargo.toml'))) {
  throw new Error(`找不到 cursor-bridge 源码目录: ${bridgeDir}`);
}

mkdirSync(dirname(bin), { recursive: true });

console.log(`[build-cursor-bridge] target triple = ${targetTriple}`);
console.log(`[build-cursor-bridge] target dir    = ${targetDir}`);
console.log(`[build-cursor-bridge] output        = ${bin}`);

execFileSync(
  'cargo',
  ['build', '--release', '--target-dir', targetDir, '-p', 'coderelay-cursor-bridge'],
  { cwd: bridgeDir, stdio: 'inherit' },
);

if (!existsSync(built)) {
  throw new Error(`cargo 未产出预期的二进制: ${built}`);
}

replaceBinary(built, bin);

console.log(`[build-cursor-bridge] built ${bin}`);
