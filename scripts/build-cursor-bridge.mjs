#!/usr/bin/env node
// 跨平台 cursor-bridge 构建脚本（Windows / macOS / Linux 通用）。
//
// 流程：
//   1. 取 rustc 的 host target triple（同 build-sidecar.mjs 的兜底策略）
//   2. 解析目标目录：与 scripts/cursor-bridge-target-dir.ps1 使用同一套顺序
//        CODERELAY_CURSOR_TARGET_DIR
//          → F:/target-cursor-bridge（存在 F: 盘时的默认值，保住已有缓存）
//          → <repo>/sidecars/cursor-bridge/target（回落；.gitignore 已覆盖）
//      5 个 verify-*.ps1 通过那个 helper 解析同一目录，两处逻辑必须同步修改。
//   3. cargo build --release --target-dir <目标目录> -p coderelay-cursor-bridge
//      （bridge 是独立 workspace，不与 src-tauri 共用 target，避免互相清逐依赖产物）
//   4. 产物落到 sidecars/cursor-bridge/bin/cursor-bridge-<triple>[.exe]：
//      先暂存 `.new` 再原子改名；目标名被占用时先把旧二进制挪到 `.old-<时间戳>`
//   5. best-effort 回收 bin/ 里由上述替换或历史工作流留下的残骸

import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readdirSync, renameSync, rmSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
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

// 目标目录解析：必须与 scripts/cursor-bridge-target-dir.ps1 逐一对应（那份是权威
// 语义，5 个 verify-*.ps1 都走它）。改这里时同步改那边，否则构建与验证会指向不同
// 目录，verify 脚本会集体报「找不到 debug 二进制」。
function resolveTargetDir() {
  // 1. 显式环境变量优先，与非空即收的 `.ps1` 语义一致（该 .ps1 不检查是否为绝对路径，
  //    相对值同样照收）。这里额外用 resolve() 归一成绝对路径再交给 cargo。
  if (process.env.CODERELAY_CURSOR_TARGET_DIR) {
    return resolve(process.env.CODERELAY_CURSOR_TARGET_DIR);
  }
  // 2. F:/target-cursor-bridge —— 仅当 F: 盘存在时。对应 `.ps1` 的 `Test-Path 'F:/'`；
  //    这里也写 'F:/' 而不是 'F:\\'：带反斜杠的写法在 fs 里会退化成查 F: 根目录，
  //    在非 Windows 上直接抛 ENOENT。这一层是「保住既有 F: 缓存」的关键，不能省。
  if (existsSync('F:/')) {
    return 'F:/target-cursor-bridge';
  }
  // 3. 回落：checkout 内目录，sidecars/cursor-bridge/.gitignore 的锚定 `/target/` 已覆盖。
  return join(bridgeDir, 'target');
}

// best-effort 残骸清理。每次冲突替换都会留下一个 `<name>.old-<时间戳>`（中断的运行会
// 留下 `<name>.new`，更早的手工流程留下 `<name>~`），不回收就会在 bin/ 里持续堆积约
// 44 MB 的副本。被运行中的进程映射的残骸根本无法删除，所以全程吞掉错误；三个模式都
// 不足以匹配 canonical 二进制本身，它永远不是候选。
//
// 本文件是这段清理逻辑的唯一定义处（旧 PowerShell 构建脚本已删除）；若将来要改模式，
// 先想清楚 `.old-*` 是唯一带时间戳的那一类。
function sweepDebris(binPath) {
  const binDir = dirname(binPath);
  const binName = basename(binPath);
  let entries;
  try {
    entries = readdirSync(binDir);
  } catch {
    return; // 目录不存在或读不到：没有任何东西可回收。
  }
  for (const name of entries) {
    const isDebris =
      name.startsWith(`${binName}.old-`) || name === `${binName}.new` || name === `${binName}~`;
    if (!isDebris) {
      continue;
    }
    try {
      rmSync(join(binDir, name), { force: true });
    } catch (error) {
      console.warn(`[build-cursor-bridge] 残骸回收跳过 ${name}（仍被占用）：${error.message}`);
    }
  }
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
sweepDebris(bin);

console.log(`[build-cursor-bridge] built ${bin}`);
