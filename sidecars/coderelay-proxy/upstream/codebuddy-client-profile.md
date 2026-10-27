# CodeBuddy CN 客户端 profile 基线（官方 1.106.1）

> 来源：只读分析本机官方客户端 `G:\CodeBuddy CN`（Electron 解包资源），未发起任何上游请求。
> 目标：为 CodeRelay 的 CodeBuddy 反向代理提供可审计的客户端身份字段，区分“已证实”“部分证实”“未知”，默认只启用高可信字段。

## 1. 官方版本基线

| 项 | 值 | 证据 |
| --- | --- | --- |
| 主程序版本 | `1.106.1` | `resources/app/package.json` |
| 产品名 | `CodeBuddy CN` | `resources/app/package.json`、`product.json` |
| 应用标识 | `buddycn` | `resources/app/product.json` |
| 平台标识 | `CodeBuddyIDE` | `resources/app/product-ide-cn.json` |
| 扩展 | `Tencent-Cloud.coding-copilot` | `resources/app/extensions/genie/package.json` |
| 扩展包版本 | `3.10.0` | 扩展 `package.json` |
| 运行时 genieVersion | `4.12.2` | `resources/app/product.json` |
| 更新/服务域名 | `https://copilot.tencent.com` | `product.json`、`product-ide-cn.json` |
| 鉴权路径前缀 | `/plugin` | `product-ide-cn.json` |

**注意**：扩展 `package.json` 的 `3.10.0` 与 `product.json` 的 `genieVersion: 4.12.2` 不一致。`X-IDE-Version` 优先使用主程序 `1.106.1`；插件版本只在有独立证据的字段中使用 `4.12.2`，不要混用。

## 2. 官方协议头（存在性已证实）

来源：`resources/app/out/codebuddy/main.js` 的协议常量表。

- `X-Trace-ID`
- `X-Request-ID`
- `X-Root-Request-ID`
- `X-Parent-Conversation-ID`
- `X-Agent-Type`
- `X-Conversation-ID`
- `X-Session-ID`
- `X-Conversation-Request-ID`
- `X-Conversation-Message-ID`
- `X-Agent-Intent`
- `X-Product`
- `X-IDE-Type`
- `X-IDE-Name`
- `X-IDE-Version`
- `X-API-Key`
- `X-Product-Version`
- `X-User-Id`
- `X-Model-ID`
- `X-Data-Tag`

官方客户端还有通用请求拦截器：`X-Request-ID` 缺失时生成 UUID（去掉 `-`），并支持 `X-Trace-ID`。

## 3. 字段可信度与默认启用

| 字段 | 来源/格式 | 适用端点 | 可信度 | CodeRelay 默认 |
| --- | --- | --- | --- | --- |
| `Authorization: Bearer <token>` | `product-ide-cn.json` 的 `tokenHeader` | chat/models/images/refresh | 已证实 | 启用（已有） |
| `X-User-Id` | `usernameHeader`，uid | chat/models/images | 已证实 | 启用（已有，保持原值） |
| `X-Enterprise-Id` / `X-Tenant-Id` | 企业账号租户 | chat/models | 部分证实 | 有值才发（已有） |
| `X-Domain` | 区域/域名 | chat/refresh | 部分证实 | 有值才发（已有） |
| `X-Product: SaaS` | 产品部署类型 | chat/models | 部分证实 | 启用（已有） |
| `X-IDE-Name: CodeBuddyIDE` | `product-ide-cn.json` 的 `platform` | chat/models | 已证实 | 启用（已有） |
| `X-IDE-Version: 1.106.1` | `package.json` 版本 | chat/models/images | 部分证实（头名+版本值） | **新增启用** |
| `X-Product-Version: 1.106.1` | 主程序版本 | chat/models | 部分证实 | **新增启用** |
| `X-IDE-Type: CodeBuddyIDE` | 平台标识 | chat/models | 部分证实 | **新增启用** |
| `X-Requested-With: XMLHttpRequest` | 官方 webview 形态 | chat/models | 部分证实 | 启用（已有） |
| `User-Agent: CodeBuddyIDE/1.106.1` | 官方存在 `CodeBuddyIDE/` 形态 | 全部 | 部分证实（形态） | **改为带版本**；不伪造完整 OS 拼接 |
| `X-Request-ID` | 每请求 UUID | chat/models/images | 部分证实 | **新增，每请求生成** |
| `X-Trace-ID` | 与 request 同源或独立 trace | chat | 部分证实 | **新增，与 request 同值** |
| `X-Session-ID` | 会话级稳定 ID | chat | 部分证实 | 有入站/会话 ID 才发，不随机生成 |
| `X-Conversation-ID` / `X-Conversation-*` | 对话标识族 | chat | 推测 | 默认不发；仅有可信入站值时按原值/派生发送 |
| `X-Model-ID` | 当前模型 | chat | 部分证实 | **新增发送** |
| `X-Agent-Type` / `X-Agent-Intent` / `X-Data-Tag` | 代理/意图/数据标签 | chat | 未知值 | 默认不发 |
| `machineId` | telemetry `common.machineId` | 仅遥测/反馈 | 已证实为遥测 | **不发** |
| `sessionId`（telemetry） | `commonProperties.sessionID` | 仅遥测 | 已证实为遥测 | **不发**（与 `X-Session-ID` 不同） |

## 4. User-Agent 规则

官方 `UserAgentHttpInterceptor.buildUserAgent` 的形态是：

```text
platform/platformVersion productName/productVersion [existing UA]
```

已证实的是拼接逻辑和 `CodeBuddyIDE` 平台标识；**完整 productName/platformVersion 的顺序与值未逐字取证**。因此 CodeRelay 默认只发送最小、可解释的：

```text
CodeBuddyIDE/1.106.1
```

不伪造操作系统版本，也不把 `machineId` 拼进 UA。后续若拿到官方真实请求样本，再升级为完整拼接。

## 5. 请求 ID 与会话 ID

- `X-Request-ID`：每个上游请求新生成 UUID（去 `-`），不复用客户端入站 ID。
- `X-Trace-ID`：与本次 `X-Request-ID` 同值，形成单请求链路。
- `X-Session-ID`：只从可信来源取：
  - 入站 `X-Session-Id` / `X-Conversation-Id`；
  - SDK 的 `execution_session_id` / `derived_session_id` 元数据。
  取不到时**不发**，不允许每请求随机生成会话 ID。
- `X-Conversation-ID` 族：默认不发，避免凭空制造对话身份。

## 6. CodeRelay 当前缺口

`sidecars/coderelay-proxy/third_party/CLIProxyAPI/internal/runtime/executor/codebuddy_executor.go` 的 `applyCodebuddyHeaders` 当前只发：

`Content-Type`、`Accept`、`Authorization`、`X-User-Id`、`X-Enterprise-Id`、`X-Tenant-Id`、`X-Domain`、`X-Product`、`X-IDE-Name`、`X-Requested-With`、`User-Agent: CodeBuddyIDE`。

需要补齐：

1. `X-IDE-Version`、`X-Product-Version`、`X-IDE-Type`
2. `X-Request-ID` / `X-Trace-ID`（每请求）
3. 可信来源下的 `X-Session-ID` / `X-Conversation-ID`
4. `X-Model-ID`
5. 带版本 UA
6. 刷新端点也使用同一套 IDE 指纹头（当前刷新只带 Authorization/X-Refresh-Token/X-Domain）
7. 模型列表与聊天共用同一 header builder（当前两处实现重复）

## 7. 不做的事

- 不发送 `machineId`、telemetry `sessionId`，没有证据表明它们进入聊天头。
- 不伪造 `X-Agent-Type`、`X-Agent-Intent`、`X-Data-Tag` 的具体值。
- 不按请求随机化账号 profile；同一账号必须稳定。
- 不复制官方客户端二进制或私有资源到仓库。
- 默认不在日志中输出 token、完整 prompt 或用户数据。`CODEBUDDY_DEBUG_BODY=1` 是显式的本地诊断开关，可能包含 prompt 原文；使用后应清理 `docs/log/codebuddy_debug.log`，且该开关不得在生产环境常开。

## 8. CodeRelay 实现默认值

`ClientProfile` 由账号持久化，默认值与上述官方基线一致：

| 字段 | 默认值 |
| --- | --- |
| `platform` / `ideType` / `ideName` | `CodeBuddyIDE` |
| `productName` | `CodeBuddy` |
| `productVersion` / `baseUserAgent` | `1.106.1` / `CodeBuddyIDE/1.106.1` |
| `pluginName` / `pluginVersion` | `coding-copilot` / `4.12.2` |
| `headerMode` | `enforce`（高可信字段） |
| `tlsMode` | `off`（uTLS 实验需显式开启） |
| 请求节奏 | 240 次/分钟、并发 4、最小间隔 250ms、抖动 10% |
| `proxyUrl` | 仅预留字段，当前不写入 auth 文件、不参与出网 |

`headerMode` 可选 `off` / `shadow` / `enforce`。`shadow` 只计算但不下发，用于灰度；`off` 完全回退到旧头。`tlsMode=experiment` 时使用 `NewChromeUtlsHTTPClient` 的 Chrome uTLS；默认 `off` 使用原有 Go transport。

### 8.1 11128 与账号冷却

`11128` 是请求体/渠道形态校验，不是账号级风控。实际验证中，同一请求首次 11128 后，使用**通用 body 中和 + 新 request ID** 重试一次即可成功；因此保留这条一次性兼容重试，但不使用静态竞品句表，也不在首次请求前改写用户 prompt。

同时：

- OpenAI 新 `developer` role 归一化为官方客户端使用的 `system`。
- `prompt_cache_key`、`prompt_cache_retention`、`store` 会被移除。
- `max_completion_tokens` 统一转换为 `max_tokens`。
- `11128` 的有效状态按 **400 请求错误**处理，不能触发账号冷却；否则一次坏请求会把整个账号池打空。
- `11140` 是账号被风控/限制，允许调度器切换或冷却该账号。

之前基于“竞品提示词指纹表”的静态清洗已移除；保留的是通用的单次兼容重试，且只在 11128 后触发。

### 8.2 安装包

`src-tauri/tauri.conf.json` 的 `beforeBuildCommand` 已包含 `npm run build:sidecar`，正式构建会把当前 sidecar 打进安装包；用户安装后直接使用，不需要复制二进制或迁移 auth 文件。版本号由远程发布工作流维护。

## 9. 灰度与回滚

- `headerMode=off`：完全不发新增头，回到旧 `CodeBuddyIDE` 裸 UA 与旧头集合。
- `headerMode=shadow`：只计算不发送，用于观察；当前账号默认 `enforce`，可在账号 profile 中改成 `shadow` 做单账号灰度。
- `tlsMode=experiment` 才使用 Chrome uTLS；改回 `off` 即回退 Go transport。
- `proxyUrl` 当前不写入 auth 文件，不参与出网；删除该字段无副作用。
- 关闭 fingerprint 不需要数据库回滚：profile 字段是新增的、可忽略；旧 state.json 会由启动迁移自动补 profile。

## 10. 后续取证（需要用户批准）

若要升级“部分证实”字段为“已证实”，需要官方客户端真实请求头样本（本地抓包/HAR，或 Electron `--inspect` + 代理拦截）。在此之前，默认只启用第 3 节标注为“新增启用”的高可信字段；低可信字段保持关闭或仅在影子模式下记录。
