#!/usr/bin/env node
// 跨平台 sidecar 构建脚本（Windows / macOS / Linux 通用）。
//
// 逻辑：
//   1. 取 rustc 的 host target triple
//   2. 输出到 sidecars/coderelay-proxy/bin/coderelay-proxy-<triple>[.exe]
//   3. go mod download && go build -trimpath -ldflags "-s -w"
//   4. best-effort 回收 bin/ 里 Go 链接器让位留下的 `<name>~` 残骸

import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, rmSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptDir, '..');
const sidecarDir = join(root, 'sidecars', 'coderelay-proxy');

// Windows / macOS / Linux 的默认 triple 兜底表，rustc 不可用时使用
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
  console.warn(`[build-sidecar] rustc 不可用，使用兜底 triple: ${fallback}`);
  return fallback;
}

// best-effort 残骸清理。Go 链接器替换运行中的二进制时，会把它改名到固定的
// `<name>~`：最多只留一个，但它会在目标名空闲时留下陈旧的那份，所以每次构建顺手
// 回收。旧进程仍映射着的残骸根本删不掉，因此全程吞掉错误，绝不让构建失败。
// 该模式匹配不到 canonical 二进制本身，它永远不是候选。
//
// 本文件是这段清理逻辑的唯一定义处（旧 PowerShell 构建脚本已删除）。
function sweepDebris(binPath) {
  const binDir = dirname(binPath);
  const debrisName = `${basename(binPath)}~`;
  let entries;
  try {
    entries = readdirSync(binDir);
  } catch {
    return; // 目录不存在或读不到：没有任何东西可回收。
  }
  for (const name of entries) {
    if (name !== debrisName) {
      continue;
    }
    try {
      rmSync(join(binDir, name), { force: true });
    } catch (error) {
      console.warn(`[build-sidecar] 残骸回收跳过 ${name}（仍被占用）：${error.message}`);
    }
  }
}

const targetTriple = resolveTargetTriple();
const extension = targetTriple.includes('windows') ? '.exe' : '';
const bin = join(sidecarDir, 'bin', `coderelay-proxy-${targetTriple}${extension}`);

if (!existsSync(join(sidecarDir, 'go.mod'))) {
  throw new Error(`找不到 sidecar 源码目录: ${sidecarDir}`);
}

mkdirSync(dirname(bin), { recursive: true });

console.log(`[build-sidecar] target triple = ${targetTriple}`);
console.log(`[build-sidecar] output        = ${bin}`);

execFileSync('go', ['mod', 'download'], { cwd: sidecarDir, stdio: 'inherit' });
execFileSync('go', ['build', '-trimpath', '-ldflags', '-s -w', '-o', bin, '.'], {
  cwd: sidecarDir,
  stdio: 'inherit',
});

sweepDebris(bin);

console.log(`[build-sidecar] built ${bin}`);
