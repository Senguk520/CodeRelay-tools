# 把 vendored Go 依赖排除出 CodeQL 分析范围

本文回答一个问题：`sidecars/coderelay-proxy/third_party/CLIProxyAPI` 这份 vendored 的上游源码快照（**1293 个 `.go` 文件**），如何才能真正不被 CodeQL 扫描。

结论先行：**CodeQL 配置文件里的 `paths-ignore` 做不到这件事，必须让该目录在构建阶段离开 CodeQL 的「源码根」。**

---

## 1. 问题背景

- `sidecars/coderelay-proxy/go.mod` 通过 replace 指令把上游模块指向本地副本：

  ```
  replace github.com/router-for-me/CLIProxyAPI/v7 => ./third_party/CLIProxyAPI
  ```

- 该副本是**嵌套 module**（自带 `go.mod`，module 行为 `github.com/router-for-me/CLIProxyAPI/v7`），因此 `go build ./...` 不会把它当根包，它只作为**依赖**参与编译。
- Code scanning 页面长期滞留约 212 条来自该目录的告警，且**每一轮分析都会重新产生**，不是旧告警未被回收。
- 原因：`.github/codeql/codeql-config.yml` 里写了 `paths-ignore: third_party`，但这条对 Go **完全无效**。

---

## 2. 为什么 `paths-ignore` 对 Go 无效：官方文档依据

GitHub Docs, *Workflow configuration options for code scanning*：

> When codebases are analyzed without building the code, you can restrict code scanning to files in specific directories by adding a `paths` array to the configuration file. You can also exclude the files in specific directories from analysis by adding a `paths-ignore` array. You can use this option when you run the CodeQL actions on an **interpreted language (Python, Ruby, and JavaScript/TypeScript)** or when you **analyze a compiled language without building the code (currently supported for C/C++, C#, Java and Rust)**.

> For analysis where code is built, if you want to limit code scanning to specific directories in your project, **you must specify appropriate build steps in the workflow**. The commands you need to use to exclude a directory from the build will depend on your build system.

关键点：`paths-ignore` 的适用范围被限定为「**不构建代码**的分析」。Go 不在支持 `build-mode: none` 的语言列表里。

GitHub Docs, *CodeQL code scanning for compiled languages* 给出的可用构建模式表中，`none` 一列标注的是：

> Yes (C/C++, C#, Java and Rust)

Go 只有 `autobuild` 与 `manual` 两种模式：

> CodeQL supports build modes `autobuild` or `manual` for Go code.

并且 manual 模式下 CodeQL 的分析范围等价于**你实际编译了什么**：

> For C/C++, C#, Go, Java, Kotlin, and Swift, CodeQL will analyze whatever source code is built by your specified build steps.

所以本仓库的 Go job 用的是 `build-mode: manual`，`paths-ignore` 这个入口在它身上是关闭的。

---

## 3. 真正的机制：提取器如何决定「提取哪些包」

这是本文最重要的一节。以下代码来自 CodeQL Go 提取器源码 `github/codeql` 仓库的 `go/extractor/extractor.go`（`ExtractWithFlags` 函数）。

**第一段：构造排除目录列表，起始项就是 `..`**

```go
// Construct a list of directory segments to exclude from extraction, starting with ".."
excludedDirs := []string{`\.\.`}

if !includeVendor {
    excludedDirs = append(excludedDirs, "vendor")
}

// If a path matches this regexp, we don't extract this package. It checks whether the path
// contains one of the `excludedDirs`.
noExtractRe := regexp.MustCompile(`.*(^|` + sep + `)(` + strings.Join(excludedDirs, "|") + `)($|` + sep + `).*`)
```

**第二段：源码根（`wantedRoots`）是怎么来的**

```go
for _, pkg := range pkgs {
    pkgInfo, ok := pkgInfos[pkg.PkgPath]
    if !ok || pkgInfo.PkgDir == "" {
        log.Fatalf("Unable to get a source directory for input package %s.", pkg.PkgPath)
    }
    wantedRoots[pkgInfo.PkgDir] = true
    if pkgInfo.ModDir != "" {
        wantedRoots[pkgInfo.ModDir] = true
    }
}
```

注意 `pkgs` 是 `packages.Load(cfg, patterns...)` 的**顶层结果**，也就是被打包成 `./...` 的那些**根包**；`wantedRoots` 只包含这些根包的目录与其 module 根目录。依赖包的目录**不会**被加进去。

**第三段：逐个包判断是否提取**

```go
for root := range wantedRoots {
    pkgInfo := pkgInfos[pkg.PkgPath]
    relDir, err := filepath.Rel(root, pkgInfo.PkgDir)
    if err != nil || noExtractRe.MatchString(relDir) {
        // if the path can't be made relative or matches the noExtract regexp skip it
        continue
    }

    extraction.extractPackage(pkg)
    // ... 提取该包的 go.mod ...
    return
}

log.Printf("Skipping dependency package %s.", pkg.PkgPath)
```

**推导出的两条判据：**

1. 若某依赖的包目录**位于源码根之下**（相对路径不含 `..` 段），且路径里没有 `vendor` 段 → **被提取**。这正是本仓库当前的处境：`third_party/CLIProxyAPI/...` 落在 `sidecars/coderelay-proxy` 之下，于是 212 条告警。
2. 若某依赖的包目录**位于源码根之外**（相对路径以 `..` 开头）→ 命中 `noExtractRe` → **打印 `Skipping dependency package` 并跳过**。这正是所有普通 Go 项目的 module cache 依赖（`gin`、`pion`、`go-redis`、`utls` 等）产生了 0 条告警的原因。

换句话说：**CodeQL 判定「这是不是我的代码」的标准，不是配置文件，而是构建时该文件相对于源码根的物理位置。**

---

## 4. 方案 A（采用）：构建阶段把 vendored 目录移出源码根

在 CodeQL 的 Go job 里，先解引用后编译：

```bash
set -euo pipefail
EXT="$RUNNER_TEMP/coderelay-ext"
mkdir -p "$EXT"
mv third_party/CLIProxyAPI "$EXT/CLIProxyAPI"
go mod edit -replace github.com/router-for-me/CLIProxyAPI/v7="$EXT/CLIProxyAPI"
go build ./...
```

配套条件与说明：

| 项 | 说明 |
| --- | --- |
| 为什么用 `mv` 而不是 `cp` | `$RUNNER_TEMP` 与 `$GITHUB_WORKSPACE` 在 GitHub 托管 runner 上同属一个文件系统，`mv` 是目录项改名，1293 个文件无数据拷贝 |
| `replace` 必须用绝对路径 | 相对写法在目录已被移走后无法解析 |
| 必须改的是 runner 上的 `go.mod` | `go mod edit` 只改工作区副本，不会回写仓库，无需清理 |
| 构建仍然成功 | 移动的是**位置**，不是内容；被移动目录内的 `go.mod` 声明未变，模块图仍能解析，`go build ./...` 行为不变 |
| 失败可见性 | `set -euo pipefail` 保证任一环节失败即 job 失败，不会产出「没编译但零告警」的假成功 |

**为什么这个方案可靠**：它不是依赖某个新配置项或未文档化的开关，而是复用提取器既有的、每日在无数 Go 项目上生效的 `..` 排除逻辑（第 3 节第二段判据）。

---

## 5. 备选方案（已评估，不采用）：`go mod vendor`

如果方案 A 失效，技术上还存在第二条路：切到 Go 的 vendor 模式。**但本仓库不采用它**，理由见本节末尾。

依据同样来自官方文档与提取器源码。GitHub Docs, *CodeQL build options and steps for compiled languages*，**Extractor options for Go** 一节：

> Additionally, `vendor` directories are excluded from CodeQL Go analysis by default. You can override this by passing the `--extractor-option extract_vendor_dirs=true` option when using the CodeQL CLI, or by setting the environment variable `CODEQL_EXTRACTOR_GO_OPTION_EXTRACT_VENDOR_DIRS` to `true`.

提取器源码中的对应实现（第 3 节第一段）：`includeVendor` 默认为 false，此时 `vendor` 被追加进 `excludedDirs`。

**不采用的理由**：它要求全仓库切换为 vendor 模式——源码树内多出一份约 1293 个文件的副本需要随版本提交与维护，并引入 `go.mod` / `go.sum` / `vendor/modules.txt` 之间的一致性约束。这笔长期维护成本，明显高于「万一排除失效，就把受影响告警批量标记为误报」这一操作。

**本仓库的决策：只保留方案 A。** 万一方案 A 未生效，处置方式是**批量 dismiss**（见第 7 节第 2 条），而不是引入 vendor 模式。

> 注：本仓库当前**不存在** `vendor/` 目录，也没有 `vendor/modules.txt`。

---

## 6. 决策记录

| 维度 | 方案 A：移出源码根 | `go mod vendor` |
| --- | --- | --- |
| 官方文档依据 | 「必须在构建步骤中排除目录」 | 「vendor 目录默认被排除」 |
| 源码依据 | `excludedDirs` 含 `..` | `excludedDirs` 含 `vendor` |
| 对源码树的影响 | 无（改动只存在于 CI 运行时的临时目录） | 新增约 1293 个文件的 `vendor/` 目录 |
| 对本地构建的影响 | 无（本地流程不执行该移动） | 全仓库切换为 vendor 模式 |
| 回退成本 | 删除构建步骤内两行 | 删除目录并恢复 `-mod=mod` |
| 结论 | **采用** | **不采用**（长期维护成本高于收益） |

若方案 A 未能生效，处置方式见第 7 节第 2 条：批量 dismiss，而不是切换到 vendor 模式。

---

## 7. 实施后如何验证是否生效

按可信度从高到低：

1. **看 Go job 日志**：提取阶段应出现

   ```
   Skipping dependency package github.com/router-for-me/CLIProxyAPI/v7/internal/...
   ```

   这行是提取器主动打印的排除证据，出现即代表排除逻辑命中。**这是最直接的判据。**

2. **看告警归属变化**：Code scanning 页面中，路径为
   `sidecars/coderelay-proxy/third_party/CLIProxyAPI/...` 的告警应转为 **Fixed**。
   **若未自动关闭**（CodeQL 某些版本不会回收路径被排除的旧告警），处置方式是**批量 dismiss**：按路径 `sidecars/coderelay-proxy/third_party` 过滤 → 批量 Dismiss → 选 "Won't fix"。这是本仓库在「引入 vendor 模式」之外的既定手段。

3. **确认构建未被破坏**：`Perform CodeQL Analysis` 不得以 exit 32 失败——exit 32 的语义是「检测到 Go 代码但本轮没有构建其中任何一部分」，若移动后误伤了根包，就会撞上这个错误码。

4. **确认首方代码仍被扫描**：`sidecars/coderelay-proxy/` 下的自研文件仍应产生告警（例如已知的 `manifest_policy.go`、`ollama_bridge.go`）。若它们也一起消失了，说明移动范围过大。

---

## 8. 回退方法

方案 A 的全部改动集中在 CodeQL workflow 的 Go 构建步骤内。删除以下两行即可完全恢复第三方检测：

```bash
mv third_party/CLIProxyAPI "$EXT/CLIProxyAPI"
go mod edit -replace github.com/router-for-me/CLIProxyAPI/v7="$EXT/CLIProxyAPI"
```

此时 `go build ./...` 回到直接读取仓库内 `third_party/CLIProxyAPI` 的状态，行为与改动前一致。

---

## 9. 关于配置文件里 `paths-ignore` 的处置

`codeql-config.yml` 中的 `paths-ignore` **不应删除**。关键认知是：**同一条目对不同语言的有效性完全不同。**

| 条目 | 对 Go | 对 Rust | 对 JavaScript/TypeScript |
| --- | --- | --- | --- |
| `third_party` / `**/third_party/**` | 无效（Go 必须编译） | **有效且必要**（排除 vendored 的 `examples/plugin/*/rust/`） | 有效，但前端源码不在该路径下，属防御性保留 |
| `node_modules` / `**/node_modules/**` | 无效 | 无效 | **有效且必要** |
| `dist` / `**/dist/**` | 无效 | 无效 | **有效且必要** |
| `**/target/**` | 无效 | 有效（`target/` 是 Cargo 构建产物目录） | 有效 |

因此配置文件里必须留下注释，说明 Go 的排除发生在构建步骤，避免后续维护者看到 `third_party` 条目仍在就误以为机制还在生效、进而删除构建步骤里的移动逻辑。

---

## 10. 补充：Rust 是另一回事，`paths-ignore` 对它是有效的

`sidecars/coderelay-proxy/third_party/CLIProxyAPI/examples/plugin/*/rust/src/lib.rs`（15 个文件）里是 vendored 的 Rust 代码，它产生的是另一类告警 —— 14 条 `Access of invalid pointer`。

**Rust 不需要第 4 节那套移动目录的手法。** 依据：

- CodeQL 的支持语言列表中包含 Rust（variants: Rust editions 2021 与 2024；extensions: `.rs`、`Cargo.toml`）。
- Rust 属于支持 `build-mode: none` 的语言组（C/C++、C#、Java、Rust）。

正因为它是「不构建即可分析」的语言，`paths-ignore` 对它**直接生效** —— 这正是第 2 节那条官方规则的**正面情形**。所以 vendored 的 Rust 代码由配置文件排除，而不是靠移动文件。

**但必须显式声明 `language: rust`。** 这是本仓库踩过的坑：早期 workflow 的 matrix 里只有 `go` 与 `javascript-typescript`，并附了一句「Rust (src-tauri/) is not a CodeQL-supported language」（**该说法是错的**）。从 default setup 切换到 advanced setup 之后，Rust 再没有被分析过，于是既有 Rust 告警成了**孤儿**：没有任何分析会重新评估它们，**已经修好的也永远停在 open**。

workflow 里的正确写法：

```yaml
          - language: rust
            build-mode: none
```

无需额外安装 `rustup` / `cargo`：GitHub 托管的 `ubuntu-latest` 已预装 Rust 工具链，`codeql-action` 自己的 Rust 测试 workflow（`.github/workflows/__rust.yml`）也不含任何工具链准备步骤。CodeQL 要求工具链不是 nightly —— 本仓库无 `rust-toolchain.toml`，不存在这个风险。

**补回 Rust 覆盖的直接后果**（预期行为，非回归）：该分析一上线就在首方代码里报出一条新告警 `#262`（`src-tauri/src/codebuddy_oauth.rs`）。它与此前的 `#256` 其实是**同一条语句** —— 修 `#256` 时插入的 `transport_error` 函数使行号由 208 漂移到 224。经复核为保守污点传播导致的误报（Token 经 `.bearer_auth()` 进入 HTTP 头，而 `reqwest::Error` 的 `Display` 只含 URL、不含请求头），已按误报关闭。这说明补回覆盖会立刻显现此前被搁置的发现 —— 这是恢复覆盖的目的，不是它带来的麻烦。

> 同一文件另有 13 处同类写法**保持原样**，其长期处置策略见第 11 节。

> **与第 4 节的对照**：Go 因为必须编译，配置层的 `paths-ignore` 完全无效；Rust 因为免构建，配置层直接有效。判断某个语言该用哪种机制，只需回答一个问题 —— **它是「不构建即可分析」的语言吗？**

---

## 11. 长期处置策略：`codebuddy_oauth.rs` 中剩余的裸 `{error}` 插值

> **这是一条既定决策，不是待办事项。** 后续任何一轮 CodeQL 若报出下列位置，**按误报（False positive）关闭，不要改代码。**

### 背景

`src-tauri/src/codebuddy_oauth.rs` 中「把 `reqwest::Error` 直接插进错误文案」这种写法共 **14 处**：

| 位置 | 状态 |
| --- | --- |
| 第 224 行（`send_billing` 的 `.send()` 分支） | 已改走 `transport_error()`。原为 `#256`，因修复插入 16 行使行号由 208 漂移到 224，被重新报为 `#262`，已按误报关闭 |
| 其余 13 处：69、70、92、93、149、151、226、284、285、511、513、544、546 | **保持原样** |

> 行号基于提交 `b4835ca`。后续改动代码会使行号漂移 —— **判断依据是写法本身（裸 `{error}` 插值），不是行号。**

### 决策：按误报关闭，不改代码

理由与 `#262` 完全一致：

1. **Token 进的是 HTTP 头，不是 URL。** `access_token` 经 `.bearer_auth(access_token)` 写入请求头；而 `reqwest::Error` 的 `Display` 只渲染 URL 与传输层原因，**从不包含请求头**。凭据本身不会出现在任何错误文案里。
2. **URL 里也没有秘密。** 本模块构造的 URL 都是字面量路径（如 `{API_ENDPOINT}{API_PREFIX}/auth/state?platform=ide`），查询串中无敏感值。
3. **告警来自保守污点传播。** CodeQL 沿 `access_token → bearer_auth → request → Error → format!` 一路标污点，并不判断该值是否真的会出现在字符串中。
4. **有对照证据。** 同一文件里写法完全相同的其余 13 处**一次都没有被报出**，唯独报了那条输出恒为常量的 —— 这说明告警取决于调用图位置，而非插值本身。

### 为什么不改代码

改走 `transport_error()` 确实能消除这一整类告警，但代价是**错误文案不再包含 URL**。这 14 处分布在登录、账号信息、Token 校验、额度刷新、签到等多条链路，全部抹掉 URL 的诊断价值损失，大于「关掉一条本就不成立的告警」的收益。

### 届时怎么做

Code scanning 页面 → 打开该告警 → `Dismiss alert` → 选 **False positive** → 粘贴与 `#262` 相同的理由（文案在 `CodeQL告警清理清单.md` 第七节第三轮，只需把其中的行号替换为当前告警的行号）。

### 什么情况下需要推翻这个决策

出现以下任一变化时，应**重新评估**而非直接 dismiss：

- `reqwest` 改变了 `Error` 的 `Display` 实现，开始包含请求头或请求体；
- 代码改为把 Token 放进 URL 查询串（例如 `?token=...`）—— 此时 URL 就真的带秘密了；
- 新增的请求路径携带查询串形式的敏感参数。

---

## 12. 引用来源

| 编号 | 内容 | 来源 |
| --- | --- | --- |
| S1 | `paths-ignore` 仅适用于不构建代码的分析；需要构建时必须在 workflow 构建步骤中排除目录 | GitHub Docs — *Workflow configuration options for code scanning*，<https://docs.github.com/en/code-security/code-scanning/creating-an-advanced-setup-for-code-scanning/customizing-your-advanced-setup-for-code-scanning> |
| S2 | `build-mode: none` 支持 C/C++、C#、Java、Rust；Go 仅支持 `autobuild` / `manual`；manual 下分析范围等于实际编译内容 | GitHub Docs — *CodeQL code scanning for compiled languages*，<https://docs.github.com/en/code-security/code-scanning/creating-an-advanced-setup-for-code-scanning/codeql-code-scanning-for-compiled-languages> |
| S3 | `vendor` 目录默认被排除，可用 `--extractor-option extract_vendor_dirs=true` 或 `CODEQL_EXTRACTOR_GO_OPTION_EXTRACT_VENDOR_DIRS` 覆盖；`_test.go` 同样默认排除 | GitHub Docs — *CodeQL build options and steps for compiled languages*（Extractor options for Go），<https://docs.github.com/en/code-security/reference/code-scanning/codeql/build-options-for-compiled-languages> |
| S4 | 提取器**源码**：`excludedDirs` 以 `..` 起始、`vendor` 条件加入；`wantedRoots` 仅由根包目录与 module 根构成；对每个包计算 `filepath.Rel(root, pkgInfo.PkgDir)` 并以 `noExtractRe` 判定是否跳过 | `github/codeql` 仓库 `go/extractor/extractor.go`（`ExtractWithFlags`），<https://github.com/github/codeql/blob/main/go/extractor/extractor.go> |

---

## 附：本仓库的既有事实（用于避免重复推断）

- `sidecars/coderelay-proxy/third_party/CLIProxyAPI/go.mod` 存在，`module github.com/router-for-me/CLIProxyAPI/v7` → 该目录是嵌套 module。
- 该目录含 **1293** 个 `.go` 文件。
- 仓库内**只有** `.github/workflows/codeql.yml` 一个 workflow；除它以外没有任何 CI 流程构建 sidecar、打包或校验该目录。
- 本地构建路径为 `scripts/build-sidecar.ps1`（由 `package.json` 的 `build:sidecar` 调用，并被 `src-tauri/tauri.conf.json` 的 `beforeDevCommand` / `beforeBuildCommand` 间接使用），它**不**在 CodeQL job 内运行，因此不受本次改动影响。
- `sidecars/coderelay-proxy` 有 15 个自研 Go 文件 import `github.com/router-for-me/CLIProxyAPI/v7/...`，这些 import 依赖 replace 目标存在——这是移动目录后仍必须重写 replace 的原因。
- 仓库当前不存在 `vendor/` 目录。
