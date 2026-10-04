# PR 自动并入规则

main 分支由三重机制保护。本文说明哪些改动会自动并入、哪些必须人工判断。

## 会自动并入

同时满足以下条件的 PR，会在必需检查全绿后自动并入 main：

- 作者在维护者名单内（Senguk520、hmbbjack），或与仓库有 OWNER / MEMBER / COLLABORATOR 关系；
- 全部改动仅限 Markdown 文档：仓库根目录的 `*.md`，或 `docs/` 下的 `*.md`；
- 不超过 10 个文件、300 行，且不含二进制文件；
- PR 非草稿、描述非空、与 main 无冲突、必需检查（三条 `Analyze`）无失败。

文档不进安装包，改错也不影响可执行产物，因此允许自动并入。

## 必须人工判断

以下情况不会自动并入，闸门会打上 `needs-human` 并留言说明原因：

- 任何代码、脚本、配置、工作流改动（`src/`、`src-tauri/`、`scripts/`、`.github/`、`package.json` 等）；
- 改动 `LICENSE`、图片等二进制文件；
- 超出规模上限；PR 为草稿、描述为空、有冲突、检查失败；
- 作者不在维护者名单内；
- 闸门自身出错（fail-closed：任何异常一律转人工）。

代码改动一律人工：一行代码也可能改变程序行为，机器无法判断「改动是否合理」。

## 三个标签

| 标签 | 含义 |
| --- | --- |
| `automerge` | 闸门已放行，等必需检查全绿后自动并入 |
| `needs-human` | 等你人工判断。修好后摘掉此标签，闸门会重新判定一次 |
| `do-not-merge` | 人工否决开关：打上即撤销 auto-merge 并转人工 |

## 参与机制

- **闸门**：`.github/workflows/pr-gate.yml`。由 GitHub Actions 以 main 分支的上下文运行，只做客观条件判断（作者、路径、规模、检查状态），不阅读代码内容。
- **安全检查**：`.github/workflows/codeql.yml`。Go / JavaScript-TypeScript / Rust 三语言静态扫描，其三条 `Analyze` 检查是 main 的必需检查。
- **版本号**：`.github/workflows/version-bump.yml`。main 出现非文档改动时自动 +1 patch 并推回 main；纯文档改动不触发（本文件即属此类）。

没有 AI 审查层：PR 内容是可被构造的输入，AI 的结论只能作为参考，不能作为合并依据。
