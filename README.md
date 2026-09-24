# CodeRelay

一个面向高级用户的 **Windows 桌面管理工具**：集中管理 CodeBuddy 中国站账号池，并运行一个本地 **OpenAI 兼容反代服务**，让 Cursor、CodeBuddy IDE 等客户端通过统一的本地地址接入多个账号，按策略做负载均衡、冷却与配额调度。

![License](https://img.shields.io/badge/license-MIT%20with%20Commons%20Clause-blue)
![Version](https://img.shields.io/badge/version-0.3.0-blue)
![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11-lightgrey)

[English](./README.en.md) | **中文**

> 视觉上采用「macOS 风味、Windows 行为」：黑白灰层级 + 语义状态色，窄栏导航、自定义无边框标题栏、底部持久服务状态栏。

---

## 功能特性

- **账号池管理**：支持 OAuth/网页登录、手动粘贴 Token、导入配置文件三种方式添加账号；账号健康状态、冷却、额度与绑定关系一目了然；支持长按拖拽调整账号顺序，账号导出用于备份与迁移。
- **每日签到**：单个账号签到或一键全部签到。
- **API Key 管理**：`sk-*` 前缀，支持绑定账号范围、限制模型、别名与启用/禁用。
- **模型管理**：从在线接口同步模型清单并本地缓存（跨重启保留），支持别名与禁用；同步失败时逐账号回退并给出可见的错误提示。
- **本地反代服务**：默认监听 `127.0.0.1:11435`，手动启动/停止，支持多账号调度策略与会话亲和。
- **局域网接入**：访问范围可切换为「本机 + 局域网」，界面自动识别并展示局域网接入地址（一键复制，附 Windows 防火墙放行命令）；切换网络后地址自动刷新。
- **请求统计与日志**：总请求数、Token、缓存命中率、Credit 消耗、按小时柱状图与按天聚合；请求日志当日保留，支持筛选、详情与 JSON 导出。
- **系统托盘与通知**：最小化到托盘、启动/停止反代、退出；启动失败/服务异常时发送 Windows 系统通知。
- **更新检测**：查询 GitHub 最新 Release 并与当前版本比对，发现新版本时在侧边栏与总览页提示；可查看更新说明并一键打开发布页下载安装包（CodeRelay **不会**自动下载或安装更新）。

---

## 界面预览

| 概览首页 | 服务配置 | API Key 管理 |
|:---:|:---:|:---:|
| ![概览首页](docs/image/home.png) | ![服务配置](docs/image/Service_configuration.png) | ![API Key 管理](docs/image/API_Key.png) |

---

## 快速开始

### 环境要求

- Node.js ≥ 20（推荐 20+）
- Rust / cargo ≥ 1.8x
- Go ≥ 1.22
- Tauri CLI（由 npm devDependency 提供，无需全局安装）

### 开发模式

```powershell
npm install
npm run tauri:dev
```

### 生产打包

```powershell
npm run tauri:build
```

打包产物（NSIS 安装包与 MSI）输出到 Cargo 目标目录的 `release/bundle/` 下。

> 注意：本项目的 Cargo 目标目录通过 `CARGO_TARGET_DIR` 或 `.cargo/config` 单独配置，非默认的 `src-tauri/target`，具体位置以你的构建环境为准。

---

## 使用方法

1. **添加账号**：进入「账号池」，通过浏览器认证、手动 Token 或配置文件导入添加 CodeBuddy 中国站账号。
2. **创建 API Key**：在「API Key」页面创建一个以 `sk-` 开头的 Key，并绑定可用的账号范围。
3. **启动服务**：在「服务配置」或底部状态栏手动启动反代服务（服务**不会**随软件启动自动运行）。
4. **接入客户端**：把客户端指向 `http://127.0.0.1:11435`，用你创建的 API Key 认证即可。若要让手机、平板等局域网设备接入，在「服务配置 → 网络 → 访问范围」切换为「本机 + 局域网」，复制页面展示的局域网地址（页面同时提供 Windows 防火墙放行命令）。

### 接入示例

**OpenAI 兼容 base_url**：

```
http://127.0.0.1:11435/v1
```

**curl**：

```bash
curl http://127.0.0.1:11435/v1/chat/completions \
  -H "Authorization: Bearer sk-xxxx" \
  -H "Content-Type: application/json" \
  -d '{"model":"auto","messages":[{"role":"user","content":"你好"}]}'
```

---

## 架构

```
┌──────────────────────────────────────────────┐
│  前端  React 19 + TypeScript + Vite          │
│        (zustand 状态管理 / lucide-react 图标) │
└──────────────────┬───────────────────────────┘
                   │ Tauri invoke / 事件
┌──────────────────▼───────────────────────────┐
│  桌面壳  Tauri 2 + Rust                       │
│        (gateway.rs: sidecar 进程管理/状态机)   │
└──────────────────┬───────────────────────────┘
                   │ 拉起 sidecar + stdout 事件
┌──────────────────▼───────────────────────────┐
│  反代 sidecar  Go                             │
│        (内嵌 CLIProxyAPI v7.2.140 + CodeBuddy │
│         CN 补丁，监听 127.0.0.1:11435)         │
└──────────────────────────────────────────────┘
```

**数据流**：Rust 把账号凭据写入临时 `auths/` 目录并生成 `config.json`/`manifest.json` → 拉起 sidecar → sidecar 通过 **stdout（JSON 行）** 发送结构化生命周期与请求事件 → Rust 解析并更新状态 → 通过事件通知前端刷新。

---

## 开发指南

### 目录结构

| 路径 | 职责 |
|---|---|
| `src/` | React 前端；`App.tsx` 承载主要页面；`services.ts` 封装 invoke/HTTP；`types.ts` 类型 |
| `src-tauri/src/lib.rs` | Tauri 入口（插件、单实例、托盘、窗口） |
| `src-tauri/src/gateway.rs` | 核心：sidecar 进程管理、状态机、事件解析、凭据热更新 |
| `src-tauri/src/codebuddy_oauth.rs` | 账号 OAuth 认证、token/额度刷新、签到 |
| `src-tauri/src/update.rs` | GitHub Releases 更新检查（版本比对、安装包地址解析） |
| `src-tauri/src/models.rs` | 请求日志/统计结构与应用状态模型 |
| `sidecars/coderelay-proxy/` | Go sidecar 主程序（relay 服务器、模型同步、账号池调度） |
| `scripts/` | `build-sidecar.ps1`、`sync-version.mjs` |

### 常用命令

```powershell
npm run typecheck          # 前端 TS 类型检查
npm run build              # 前端生产构建
npm run build:sidecar      # 按 Rust target triple 编译 Go sidecar
npm run sync-version       # 把 package.json 版本同步到 tauri.conf.json/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
go build ./...             # 在 sidecars/coderelay-proxy 下
go test ./...
```

### 版本号约定

**单一版本源 = `package.json` 的 `version`**。升级版本只改这一个文件，再运行 `npm run sync-version`（或直接 `tauri:build`，其 beforeBuild 已包含）。前端通过 `APP_VERSION`（由 Vite 注入）展示版本号，不要硬编码。Rust 侧通过 `env!("CARGO_PKG_VERSION")` 读取同一版本参与更新比对，无需另行维护。

### 发布约定

更新检测读取 GitHub Releases 的最新发布。发版时请把安装包上传到 Release 的 **Assets**（附件），命名建议 `CodeRelay_<版本>_x64-setup.zip`：

- **优先读 Assets**：能拿到稳定直链、文件名与体积，界面可直接展示安装包信息。
- **正文链接兜底**：若 Assets 为空，程序会从发布说明正文里解析 `user-attachments` 的 zip 直链（兼容 v0.3.0 及更早的发布方式）。该方式拿不到体积，界面显示为「—」。
- Tag 需可解析为版本号（如 `v0.3.0`）。`Bate_Version` 这类无法解析的 tag 会被视为「无更新」，不会误报。

更新检测结果属于会话态，不写入 `state.json`；「启动时检测更新」默认关闭，需在「设置 → 常规」手动开启。

---

## 致谢

本项目参考了 [cockpit-tools](https://github.com/jlcodes99/cockpit-tools) 的技术选型，并基于众多优秀的开源项目构建。完整的上游致谢与第三方依赖归属清单见 [ACKNOWLEDGMENTS.md](./ACKNOWLEDGMENTS.md)。

「Cursor 服务」所使用的本地桥接 sidecar，是 [cursor_byok](https://github.com/leookun/cursor-byok)（MIT License，Copyright (c) 2026 leookun）服务端的修改副本，固定在上游 commit `2068ab20513288e6a0289d33febe872a30608610`。感谢作者 leookun 与 cursor_byok 项目：没有这份上游实现，「Cursor 服务」无从做起。上游版权与许可声明已逐字保留，改动清单见 [sidecars/cursor-bridge/README.md](./sidecars/cursor-bridge/README.md)，详细来源记录见 [sidecars/cursor-bridge/UPSTREAM.md](./sidecars/cursor-bridge/UPSTREAM.md)。

> **排障提示**：该桥接通过 Cursor 的私有 protobuf 协议工作，Cursor 更新后可能失效。升级 Cursor 后请**完全退出并重新启动** Cursor，再重新开启注入。
>
> **Tab 补全说明**：本桥接不再提供 Tab 内联补全，相关请求会原样转发给 Cursor 官方服务，因此其可用性取决于你自己的 Cursor 账号额度。这是预期行为，不是缺陷。

## 第三方组件与许可

本项目反代 sidecar 基于 [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI)（MIT License）构建；Cursor 桥接 sidecar 基于 [cursor_byok](https://github.com/leookun/cursor-byok)（MIT License）构建。第三方组件的许可与归属信息见：

- [NOTICE.md](./NOTICE.md)
- [ACKNOWLEDGMENTS.md](./ACKNOWLEDGMENTS.md)
- `sidecars/coderelay-proxy/third_party/CLIProxyAPI/LICENSE`
- `sidecars/cursor-bridge/LICENSE`、`sidecars/cursor-bridge/UPSTREAM.md`

---

## 许可证

本项目自身代码采用 [MIT License with Commons Clause](./LICENSE) 开源：在 MIT License 基础上附加 **Commons Clause License Condition v1.0**，即允许自由使用、复制、修改、合并、发布与分发，但**禁止将本软件以收费或其他对价形式提供给第三方**（禁止 Sell）。第三方组件 [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) 与 [cursor_byok](https://github.com/leookun/cursor-byok) 仍保持各自的原始 MIT License 不变，不受上述 Commons Clause 附加条款约束；该附加条款只适用于本项目自身代码。

---

## 免责声明

CodeRelay 是独立的第三方工具，与腾讯、CodeBuddy 官方无关。请遵守 CodeBuddy 及相关上游服务的使用条款，仅将其用于合法、合规的学习与个人用途。账号凭据、Token 与 API Key 均只保存在本地，本项目不收集、不上传任何凭据。
