#!/usr/bin/env node
// 跨平台 sidecar 构建脚本。
// 原仓库只提供 scripts/build-sidecar.ps1（PowerShell），在 macOS / Linux 上无法执行。
// 本脚本用 Node 重写同一套逻辑，Windows / macOS / Linux 通用。
//
// 逻辑与 build-sidecar.ps1 完全等价：
//   1. 取 rustc 的 host target triple
//   2. 输出到 sidecars/coderelay-proxy/bin/coderelay-proxy-<triple>[.exe]
//   3. go mod download && go build -trimpath -ldflags "-s -w"

import { execFileSync } from 'node:child_process';
import { mkdirSync, existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
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

console.log(`[build-sidecar] built ${bin}`);
