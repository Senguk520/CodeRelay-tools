# 本地补丁清单：`third_party/CLIProxyAPI` vs 上游 `v7.2.140`

> 目的：长期维护时，能一眼看出"哪些文件是我们改的、不能随上游整体覆盖删除"。**这是红线依据，不是合并计划。**

## 1. 比对方法（关键：必须先归一化行尾）

| 项 | 值 |
| --- | --- |
| 本地快照 | `sidecars/coderelay-proxy/third_party/CLIProxyAPI`（**CRLF**） |
| 上游来源 | `goproxy.cn` → `github.com/router-for-me/CLIProxyAPI/v7@v7.2.140`，**1301 个文件** |
| 比对日期 | 2026-10-21 |
| 本地文件清单口径 | 仅 **git 已跟踪**（`git ls-files`），共 1471 条 |

**CRLF 陷阱（最重要的一条）**：本地快照是 CRLF、上游 zip 是 LF，且两侧**都没有 `.gitattributes`**。
裸字节 SHA256 比对会得到 **1264 个 `modified`**，其中 **1209 个纯粹只差行尾** —— 这是错的。
按下列规则归一化后重新计算，`modified` 从 1264 降到 **55**：

- **文本**：`\r\n` → `\n` 后再算 SHA256。
- **二进制**：按原字节算 SHA256。二进制扩展名白名单：`.png .jpg .jpeg .gif .ico .dll .lib .zip .exe .woff .woff2`。
- 判定顺序：**除上述二进制白名单外一律按文本归一化**。不要按"文本扩展名白名单"判定 —— 那样 `.c` / `.rs` / `.lock` / `Dockerfile` / `Makefile` / `.gitignore` / `.dockerignore` 会漏掉归一化，凭空多出 52 个假 `modified`（本应 55，会错成 107）。这正是本次踩过的坑，务必按"二进制白名单取反"实现。
- 本地侧只取 git 已跟踪文件；未跟踪的构建产物/临时文件不参与比对（这也避免了上游剥离 `!c!l!i!proxy!a!p!i` 大小写路径带来的干扰）。

## 2. 五类计数

| 类别 | 数量 |
| --- | --- |
| `identical` | **1233** |
| `modified` | **55** |
| `local_added` | **111** |
| `upstream_only` | **13** |
| `unverified_nested_module` | **72**（24 个嵌套 `go.mod` 子模块目录，见第 6 节） |

**闭合校验（两侧都严丝合缝，可直接拿去核对）**：

- 本地侧：`identical 1233 + modified 55 + local_added 111 + unverified 72 = 1471` = git 已跟踪条数 ✔
- 上游侧：`identical 1233 + modified 55 + upstream_only 13 = 1301` = 上游文件数 ✔
- 两侧交集（上游有且本地也有）= `1233 + 55 = 1288` = `1301 − 13` ✔

> 上表 `identical` 含全部"两侧都有且归一化后 SHA256 相同"的文件。`unverified_nested_module` 的 72 个只计入本地侧、不参与上游侧等式 —— 它们在上游 zip 中被剥离，所以对上游而言"不存在"，这也是五类必须分成两侧闭合的原因。

> 说明：上表 `upstream_only` 与 `unverified_nested_module` 的口径不同——前者是"上游有、本地没有"，后者是"本地有、但因嵌套 module 被上游 zip 剥离而无法比对"。两者都不计入 `identical`。

锚点校验（4/4 通过，均判为 `local_added`）：`internal/auth/codebuddy/codebuddy.go`、`internal/auth/codebuddy/token.go`、`sdk/cliproxy/cockpit_runtime.go`、`sdk/cliproxy/auth/reset_compat.go`。

## 3. 包目录汇总表（建议红线集合）

只列合计 > 0 的目录。**`含_test` 标记 = 该目录下有被修改或新增的 `*_test.go`**，删除/覆盖时风险最高。

| 包目录 | modified | local_added | 含_test |
| --- | ---: | ---: | :---: |
| `internal/runtime/executor` | 10 | 28 | ✔ |
| `internal/api/modules/amp` | 0 | 16 | ✔ |
| `sdk/cliproxy/auth` | 5 | 4 | ✔ |
| `internal/runtime/executor/helps` | 5 | 3 | ✔ |
| `internal/registry` | 4 | 3 | ✔ |
| `internal/client/codex/live` | 6 | 0 | ✔ |
| `sdk/cliproxy` | 4 | 2 | ✔ |
| `internal/translator/codex/gemini-cli` | 0 | 4 | ✔ |
| `internal/translator/gemini-cli/claude` | 0 | 4 | ✔ |
| `internal/translator/claude/gemini-cli` | 0 | 3 | |
| `internal/translator/gemini-cli/gemini` | 0 | 3 | |
| `internal/translator/gemini-cli/openai/chat-completions` | 0 | 3 | |
| `internal/translator/gemini-cli/openai/responses` | 0 | 3 | |
| `internal/translator/gemini/gemini-cli` | 0 | 3 | |
| `internal/translator/openai/gemini-cli` | 0 | 3 | |
| `internal/translator/openai/openai/responses` | 2 | 1 | ✔ |
| `internal/thinking` | 0 | 3 | ✔ |
| `sdk/api/handlers/openai` | 0 | 3 | ✔ |
| `sdk/auth` | 1 | 2 | |
| `internal/auth/claude` | 2 | 0 | |
| `internal/auth/codebuddy` | 0 | 2 | |
| `internal/auth/codex` | 2 | 0 | |
| `internal/auth/gemini` | 0 | 2 | |
| `internal/auth/xai` | 1 | 1 | |
| `sdk/api/handlers` | 2 | 0 | |
| `internal/api/handlers/management` | 1 | 0 | |
| `internal/config` | 1 | 0 | |
| `internal/misc` | 1 | 0 | |
| `internal/registry/models` | 1 | 0 | |
| `internal/translator/codex/openai/responses` | 1 | 0 | ✔ |
| `sdk/cliproxy/executor` | 1 | 0 | |
| `sdk/cliproxy/usage` | 1 | 0 | |
| `examples/plugin/frontend-auth-exclusive` | 1 | 0 | |
| `internal/api/modules` | 0 | 1 | |
| `internal/cmd` | 0 | 1 | |
| `internal/constant` | 0 | 1 | |
| `internal/runtime/geminicli` | 0 | 1 | |
| `internal/thinking/provider/geminicli` | 0 | 1 | |
| `internal/thinking/provider/kimi` | 0 | 1 | ✔ |
| `internal/thinking/provider/xai` | 0 | 1 | ✔ |
| `internal/util` | 0 | 1 | |
| `sdk/api/handlers/gemini` | 0 | 1 | |
| `test` | 0 | 1 | ✔ |
| `test/testdata/claude_code_sentinels`（测试夹具，非 Go 包） | 0 | 4 | |
| （仓库根，非包） | 3 | 0 | |
| **合计 44 个目录** | **55** | **111** | 16 个 |

根目录 3 个 `modified` 文件：`README.md`、`README_CN.md`、`docker-build.ps1`。
**路径异形提示**：本地存在目录 `<snapshot>/.goreleaser.yml/`，其内才是文件 `.goreleaser.yml`（git 跟踪路径为 `.goreleaser.yml/.goreleaser.yml`），上游是普通文件 `.goreleaser.yml` —— 该条目按目录归类，不计入上表 44 个目录的分母口径争议。

## 4. 专节：被修改或新增的 `*_test.go`（删除风险核心）

**共 50 个**：`modified` 9 个 + `local_added` 41 个，分布在 16 个包目录。

| 包目录 | modified | local_added |
| --- | ---: | ---: |
| `internal/runtime/executor` | 2 | 16 |
| `internal/api/modules/amp` | 0 | 8 |
| `sdk/cliproxy/auth` | 1 | 3 |
| `internal/runtime/executor/helps` | 2 | 1 |
| `internal/registry` | 0 | 2 |
| `internal/client/codex/live` | 2 | 0 |
| `sdk/api/handlers/openai` | 0 | 2 |
| `internal/thinking` | 0 | 3 |
| `internal/thinking/provider/kimi` | 0 | 1 |
| `internal/thinking/provider/xai` | 0 | 1 |
| `internal/translator/codex/gemini-cli` | 0 | 1 |
| `internal/translator/codex/openai/responses` | 1 | 0 |
| `internal/translator/gemini-cli/claude` | 0 | 1 |
| `internal/translator/openai/openai/responses` | 1 | 0 |
| `sdk/cliproxy` | 0 | 1 |
| `test` | 0 | 1 |

9 个 `modified` 测试文件（上游已有、我们改过）：
`internal/client/codex/live/capabilities_test.go`、`internal/client/codex/live/live_test.go`、`internal/runtime/executor/codex_executor_imagegen_test.go`、`internal/runtime/executor/codex_websockets_executor_test.go`、`internal/runtime/executor/helps/payload_helpers_disable_image_generation_test.go`、`internal/runtime/executor/helps/usage_helpers_test.go`、`internal/translator/codex/openai/responses/codex_openai-responses_request_test.go`、`internal/translator/openai/openai/responses/openai_openai-responses_response_test.go`、`sdk/cliproxy/auth/home_unauthorized_refresh_test.go`

> 41 个 `local_added` 测试文件名未逐一列出（受篇幅限制）。完整清单可用第 7 节命令重新生成，筛选条件为 `local_added` 且文件名以 `_test.go` 结尾。

## 5. 对原 11 目录清单的裁决：**漏 34 个 / 多 2 个**

| 原清单目录 | 实际改动数 | 结论 |
| --- | ---: | --- |
| `internal/api/handlers/management` | 1 | 命中 |
| `internal/auth/claude` | 2 | 命中 |
| `internal/auth/codex` | 2 | 命中 |
| `internal/config` | 1 | 命中 |
| `internal/logging` | **0** | **多项（零本地改动）** |
| `internal/redisqueue` | **0** | **多项（零本地改动）** |
| `internal/registry` | 7 | 命中 |
| `internal/runtime/executor` | 38 | 命中 |
| `internal/runtime/executor/helps` | 8 | 命中 |
| `sdk/api/handlers` | 2 | 命中 |
| `sdk/cliproxy/usage` | 1 | 命中 |

- 命中 9 / 11；**多余 2 个**：`internal/logging`、`internal/redisqueue`（二者本地零改动，不应进红线集）。
- 原清单**遗漏 34 个**含改动的包目录，完整列表即第 3 节中未出现在原 11 项里的目录。遗漏最严重的前几名：
  `internal/runtime/executor` 之外还有 `sdk/cliproxy/auth`(9)、`internal/client/codex/live`(6)、`sdk/cliproxy`(6)、`internal/api/modules/amp`(16)、`internal/translator/gemini-cli/claude` 等 7 个 gemini-cli 翻译包、`internal/thinking` 及其 3 个 provider 子包、`sdk/auth`(3)、`sdk/api/handlers/openai`(3)、`internal/auth/codebuddy`(2)、`internal/auth/gemini`(2)、`internal/auth/xai`(2)。
- 结论：原 11 目录清单**不能**作为红线依据，须以第 3 节 44 个目录为准。

## 6. `unverified_nested_module` 的含义与数量

本地快照含 **24 个 `go.mod`**（1 个仓库根 + 23 个嵌套子模块，全部位于 `examples/plugin/*/go/` 与 `examples/realtime-openai-go/`）。
这些子模块目录下的 **72 个文件**在 `go mod download` 取回的模块 zip 中被剥离（Go 模块发布时会剔除嵌套 module 目录），因此**无法与上游比对**，单列计数，**不计入 `local_added`**（否则会虚增 72 个"我们新增的文件"）。

> 另：`examples/plugin/*/c/` 与 `examples/plugin/*/rust/` 下的 `.c` / `.rs` / `Cargo.lock` 文件并非嵌套 Go module，可正常比对 —— 它们在上游存在且内容一致（仅行尾不同），归类为 `identical`。

## 7. 如何复做

```powershell
# 1) 取上游到临时目录（不要 git clone，github 不通）
$t = "$env:TEMP\up-$([guid]::NewGuid().ToString('N').Substring(0,6))"
New-Item -ItemType Directory -Force -Path $t | Out-Null
$env:GOCACHE = "$t\gocache"; $env:GOPATH = "$t\gopath"; $env:GOFLAGS = ""
Set-Location $t
go mod init probe
go mod download github.com/router-for-me/CLIProxyAPI/v7@v7.2.140
# 源码落在 $t\gopath\pkg\mod\github.com\router-for-me\!c!l!i!proxy!a!p!i\v7@v7.2.140

# 2) 本地侧只取 git 已跟踪文件
git -C H:\CodeRelay\Independent_new_program ls-files -z -- sidecars/coderelay-proxy/third_party/CLIProxyAPI

# 3) 归一化比对要点
#    BIN = {.png .jpg .jpeg .gif .ico .dll .lib .zip .exe .woff .woff2}
#    若 ext NOT IN BIN: data = data.replace(b"\r\n", b"\n")
#    hash = SHA256(data)；相等 → identical，两侧都有但不等 → modified，
#    仅本地有 → local_added（嵌套 go.mod 目录下的除外），仅上游有 → upstream_only
```

自检基线（数字不符说明归一化没生效）：`identical=1233`、`modified=55`、`local_added=111`、`upstream_only=13`、`unverified_nested_module=72`；且裸字节比对必须重现 `modified=1264`。
`upstream_only` 共 13 个，即「上游有、本地快照中不存在」的文件。已知包含：`.env.example`、`.env.cluster.example`（本地由上游 `.gitignore` 的 `.env*` 规则排除）、`AGENTS.md`、`CLAUDE.md`、`auths/.gitkeep`，以及 `docs/` 下若干未被 vendor 的 SDK 文档（`docs/sdk-access*`、`docs/sdk-advanced*`、`docs/sdk-usage*`、`docs/sdk-watcher*` 的中英两版）。完整逐条清单请用第 7 节命令重新生成后核对——本行**不逐个断言**，以免与实际计数不符。

## 8. 按 43 个受保护包目录重算的删除量

**判定规则（一句话）**：**按包粒度豁免** —— 包目录 ∈ P ⇒ 该目录下**所有** `*_test.go` 全部保留；不在 P 里的包，其 `*_test.go` 才进入删除集。不是按单个文件判断。

**P = 43，而不是 44。** 第 3 节表体实际是 45 行：43 个真正的包目录 + 1 行 `test/testdata/claude_code_sentinels`（测试夹具，4 个 JSON，**不是 Go 包**）+ 1 行「仓库根，非包」。表头「合计 44 个目录」把夹具行计入了分母（仓库根行未计入，见第 3 节末注）。因此真实包目录集合 P = **43**，本节一律按 43 重算，不沿用 44。

### 8.1 分组删除量

| 组 | 内容 | 文件数 | 字节 | MiB |
| --- | --- | ---: | ---: | ---: |
| A | `internal/`＋`sdk/` 之下、包目录 ∉ P 的 `*_test.go` | 190 | 2,631,980 | 2.510 |
| B1 | `examples/`（163 减 P 中 3 个 → 删 160） | 160 | 539,936 | 0.515 |
| B2 | `test/`（**全保留，删 0**，见 8.3） | 0 | 0 | 0 |
| B3 | `assets/` | 25 | 1,637,905 | 1.562 |
| B4 | `.github/` | 9 | 38,901 | 0.037 |
| B5 | `cmd/` | 6 | 55,164 | 0.053 |
| B6 | `.goreleaser.yml/`（目录套同名文件） | 1 | 939 | 0.001 |
| B7 | Docker 套件（保留 `docker-build.ps1` 口径） | 5 | 5,101 | 0.005 |
| **B** | **小计（B1–B7）** | **206** | **2,277,946** | **2.172** |
| **A+B** | **CLIProxyAPI 侧删除** | **396** | **4,909,926** | **4.683** |
| C | `sidecars/cursor-bridge/protocols/cursor/` 4 个 proto | 4 | 216,242 | 0.206 |
| **合计** | **A+B+C** | **400** | **5,126,168** | **4.889** |

- A 组校验：`internal` 413 + `sdk` 148 = **561** 个 `*_test.go`；P 中位于 `internal/`/`sdk/` 的目录（27 个）共 **371** 个测试 → A = 561 − 371 = **190**，闭合 ✔。
- C 组逐文件字节：`anyrun_v1.proto` 48,696 / `git_forge_v1.proto` 25,108 / `internapi_v1.proto` 1,039 / `origin_v1.proto` 141,399 → 216,242 B = 0.206 MiB ✔。
- 全部字节均为 `Get-Item.Length` 磁盘实际值求和；MiB 一律 **÷1024÷1024**，与本文既有口径一致。

### 8.2 重算后的最终数字

| 指标 | 值 |
| --- | ---: |
| 删除文件总数 | **400**（A 190 + B 206 + C 4） |
| 删除总体积 | **5,126,168 B = 4.889 MiB** |
| CLIProxyAPI 删后 | 1471 − 396 = **1075** |
| cursor-bridge 删后 | 272 − 4 = **268** |
| sidecars 合计删后 | 1743 − 400 = **1343** |
| sidecars 合计体积 | 22,009,253 B = 20.99 MiB → 删后约 **16.10 MiB**，降 **23.29%** |

### 8.3 `test/` 与 `examples/` 的保留明细（这两个目录都在 P 内）

- `test/` 共 **11** 个 git 跟踪文件：7 个 `*_test.go`（`amp_management`、`builtin_tools_translation`、`claude_code_compatibility_sentinel`、`codex_claude_parallel_function_calls`、`summary_intent_translation`、`thinking_conversion`、`usage_logging`）+ 4 个 `test/testdata/claude_code_sentinels/*.json`。`test` ∈ P 且 `test/testdata/claude_code_sentinels` ∈ P，整目录豁免 → **保留 11、删除 0**。
- `examples/` 共 **163** 个：P 中 `examples/plugin/frontend-auth-exclusive` 实测 **3** 个文件（`README.md`、`go/go.mod`、`go/main.go`）→ **保留 3、删除 160**。

### 8.4 与首版口径的对比

| 指标 | 首版口径 | 本次（43 包粒度） | 差 |
| --- | ---: | ---: | ---: |
| 删除文件数 | 570 | **400** | −170 |
| 删除体积 | 7.06 MiB | **4.889 MiB** | −2.171 MiB |
| CLIProxyAPI 删后 | 905 | **1075** | +170 |
| sidecars 合计删后 | 1173 | **1343** | +170 |

**差异来源（按组拆解，两组合计 −170）**：

| 组 | 首版 | 本次 | 差 | 原因 |
| --- | ---: | ---: | ---: | --- |
| A（测试） | 345 | **190** | **−155** | 包粒度豁免把 P 内 **378** 个测试全部保留（internal/sdk 侧 371 + `test/` 侧 7），而首版按 11 目录清单只保留 216 个 |
| B（整目录） | 221 | **206** | **−15** | `examples/` 163→160（保留 3 个 `frontend-auth-exclusive`）、`test/` 11→**0**（整个 `test/` 在 P 内）、Docker 6→5（保留 `docker-build.ps1`）；`assets/` 25、`.github/` 9、`cmd/` 6、`.goreleaser.yml/` 1 不变 |
| C（proto） | 4 | **4** | 0 | 无变化 |
| **合计** | **570** | **400** | **−170** | −155（A）+ −15（B） |

**要点**：首版 570 = A 345 + B 221 + C 4，其中 B 组 221 **本来就包含** `examples/assets/test/.github/cmd/.goreleaser.yml/Docker`，`1471 − 345 − 221 = 905` 正是在包含这些目录的前提下闭合的。本次差异**完全来自包粒度豁免**——它让 155 个原本会删的测试被保留（A 组），并顺带保住 `test/` 全部 11 个文件、`examples/` 3 个文件、Docker 1 个文件（B 组）。B 组不是"新增"，而是"缩小"。

**净差**：**−170 个文件、−2.171 MiB**；905/1173 相应上调为 **1075/1343**。

### 8.5 Docker 两种口径

| 口径 | 文件数 | 字节 | 总删除数 | 删除体积 | CLIProxyAPI 删后 | sidecars 删后 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| **保留** `docker-build.ps1`（推荐，第 3 节列为根目录 modified） | 5 | 5,101 | **400** | **4.889 MiB** | **1075** | **1343** |
| 一并删除 | 6 | 7,145 | 401 | 4.891 MiB | 1074 | 1342 |

两口径相差 **1 个文件 / 2,044 B**。本文 8.1–8.4 全部采用「保留 `docker-build.ps1`」口径。

### 8.6 红线自检：**通过（交集为空）**

- 将被删除的 `*_test.go` 共 **202** 个，与第 4 节列出的 **9 个 `modified` 测试**求交集 → **0 个命中**。
- 与第 4 节全部 **16 个包目录**（9 modified + 41 local_added，共 50 个测试）求交集 → **0 个命中**；并已实测第 4 节这 16 个目录 **100% 落在 P 内**（0 个遗漏），这正是包粒度豁免能够安全成立的原因。
- 结论：删除集与红线集**不相交**，无受保护测试文件被误删。

**一句话结论**：按 43 个受保护包目录的包粒度豁免口径重算，可安全删除 **400 个文件 / 4.889 MiB**（CLIProxyAPI 396、cursor proto 4），CLIProxyAPI 保留 1075、cursor-bridge 保留 268、sidecars 合计保留 1343，整体体积降 23.29%，且与首版相比少删 170 个文件、少删 2.171 MiB —— 少删的部分正是此前会误伤的受保护包内测试。

## 9. 2026-10-27 防封功能新增本地文件

以下文件不属于上游 v7.2.140 快照，**禁止随上游同步删除**：

- `internal/auth/codebuddy/client_profile.go`（local_added）
- `internal/auth/codebuddy/client_profile_test.go`（local_added）
- `internal/runtime/executor/helps/codebuddy_fallback.go`（local_added，11128 单次兼容 body 中和）
- `internal/runtime/executor/codebuddy_retry_test.go`（local_added，11128 兼容重试回归测试）
- ~~`internal/runtime/executor/helps/codebuddy_sanitize.go`（已删除：基于错误诊断的静态提示词表）~~
- ~~`internal/runtime/executor/helps/codebuddy_sanitize_test.go`（已删除）~~
- `internal/runtime/executor/helps/codebuddy_traffic.go`（local_added，账号级限速）
- `internal/runtime/executor/helps/codebuddy_traffic_test.go`（local_added）
- `internal/runtime/executor/codebuddy_client_test.go`（local_added，uTLS 选择测试）
- `upstream/codebuddy-client-profile.md`（协议基线与字段可信度表）

这些改动同时修改了 `internal/auth/codebuddy/codebuddy.go`、`internal/runtime/executor/codebuddy_executor.go`、`internal/runtime/executor/helps/utls_client.go`。它们不改变 Codex/Claude 链路；账号 profile 写入 auth 文件，代理字段只预留、不写入 auth 文件。
