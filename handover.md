# cswitch 交接（2026-10-08）

工作区：`/Volumes/shit/orca/workspaces/cswitch/skua`（git worktree，分支 `newbdez33/cswitch-claude-auto`，干净）。
主仓库 `/Volumes/shit/projects/cswitch` 不要 `cd` 进去；它的 `main` 还停在改写前，需要用户执行：

```
git fetch --prune --tags --force && git reset --hard origin/main
```

## 0. 一句话现状

v0.3.0（Claude provider 第一阶段）已发布，`main` 历史已改写、不含任何 Claude 署名；
第二阶段（Claude 自动切换）的规格已改好、实施计划已写好但尚未执行；
用户决定把项目改名为 **ccsw**，并删除重建 GitHub 仓库，以甩掉 PR #5 里删不掉的 `Co-Authored-By: Claude` 痕迹。

## 1. 仓库与 GitHub 状态

- `origin/main` = `9c88a6d`（`chore(release): v0.3.0 (#6)`），其父 `0c8c039` 是 PR #5 的重写 squash；两者文件树与原提交完全一致。
- 标签 `v0.1.0`、`v0.2.0`、`v0.3.0`；`v0.3.0` 的 release 已用改写后的提交重跑过（5 个资产）。
- 今天删除的远程分支：`newbdez33/cswitch-add-cswap-tui`、`release/v0.3.0`（都已 squash 进 main，分支顶端文件树与 main 一致）。带署名的 `abd0841`、`737f580` 不再被任何分支引用。
- 仍在的远程分支：`newbdez33/add-account-browser-login`、`newbdez33/codex-reset-cards-tui`（旧的已合并分支，无署名）。
- GitHub contributors API 只返回 `newbdez33`（30 commits）；仓库首页侧栏显示的「claude Claude」是缓存。
- 删不掉的痕迹：PR #5 的 Commits 页永远显示 `abd0841`（`refs/pull/5/head` 用户无法删除）。这是用户决定删库重建的原因。
- 本地未推送：分支 `newbdez33/cswitch-claude-auto` 在 main 之上有两个提交：
  - `738f065` docs(spec)：自动切换只覆盖 Claude Code
  - `2b35b11` docs(plan)：第二阶段实施计划

## 2. 防再犯（已完成，两台机器）

| 措施 | j-studio（本机 Mac） | jx（Windows，SSH 别名 `jx`，默认 shell 是 PowerShell） |
|---|---|---|
| 全局 git 钩子 | `~/.git-hooks/`，`git config --global core.hooksPath` 已指向它 | `C:/Users/newbd/.git-hooks/`，同上 |
| Claude Code 设置 | `~/.claude/settings.json`：`includeCoAuthoredBy: false`，`attribution: {commit: "", pr: ""}`；备份 `settings.json.bak-20261008` | 同上（同名备份） |

钩子细节：
- `commit-msg` 拒绝 `Co-Authored-By: …claude/anthropic` 和 `Generated with Claude Code`，然后转交仓库自己的 `.git/hooks/commit-msg`。
- 其他标准钩子名（pre-commit、pre-push 等 13 个）都是 `_forward` 的副本：只转交仓库自己的同名钩子，不然什么也不做。所以旧的 `.git/hooks/*` 仍然生效。
- 注意：仓库级 `core.hooksPath`（husky/lefthook 那种）会覆盖全局设置，这类仓库不受保护。
- 两台机器都做过自测：带 Claude 署名的提交被拒，干净提交通过，仓库本地 pre-commit 照常运行。
- cswitch 主仓库里原来的 `.git/hooks/commit-msg` 还在，和全局钩子重复，无害。

## 3. 待办 A：改名 ccsw 并重建 GitHub 仓库

### 3.1 用户要做的（GitHub）
1. 删除 `newbdez33/cswitch`。
2. 新建空仓库 `newbdez33/ccsw`（不要勾选初始化 README/License/.gitignore）。描述建议：`Multi-account switcher for the OpenAI Codex CLI and Claude Code`。
3. 本地目录可随后改名（`/Volumes/shit/projects/cswitch` → `ccsw`、orca workspace 同理）。Claude 记忆目录按项目路径分（`~/.claude/projects/-Volumes-shit-projects-cswitch/memory/`），改路径后把 memory 复制到新目录。

### 3.2 代码改名（一次 sed 大法，建议单独分支 `newbdez33/rename-ccsw`，从 `origin/main` 切）

盘点：`git grep -i cswitch` 共 930 行、76 个文件。映射：

| 现名 | 新名 | 位置 / 备注 |
|---|---|---|
| `cswitch`（crate、bin、文案） | `ccsw` | `Cargo.toml` name/`[[bin]]`/repository/description；`src/main.rs` `cswitch::cli::run`；tests/examples 的 `use cswitch::`；`tests/support/mod.rs` `CARGO_BIN_EXE_cswitch`；`src/cli/legacy.rs` `PROG`；所有帮助/错误文案、README、CHANGELOG、docs |
| `CswitchError` | `CcswError` | `src/errors.rs` 定义，254 处引用 |
| `CSWITCH_HOME` `CSWITCH_KEYCHAIN` `CSWITCH_USAGE_URL` `CSWITCH_TOKEN_URL` `CSWITCH_CLAUDE_USAGE_URL` `CSWITCH_CLAUDE_TOKEN_URL` `CSWITCH_CLAUDE_LOCK_BUDGET_MS` `CSWITCH_PROXY` `CSWITCH_TEST_RECORD` | `CCSW_*` | `src/paths.rs`、`src/codex/{usage,oauth}.rs`、`src/claude/{usage,locks}.rs`、`src/collect.rs` 测试、`tests/support/mod.rs`、README |
| `~/.cswitch` | `~/.ccsw` | `src/paths.rs:63`；需要迁移（见 3.3） |
| `cswitch.log` | `ccsw.log` | `src/paths.rs:147`、`src/logging.rs` 测试、`tests/support/mod.rs:411` |
| `.cswitch-shared.json` | `.ccsw-shared.json` | `src/session.rs:30` `MANIFEST_NAME`（session 目录里的共享清单） |
| 导出文件扩展名 `.cswitch` | `.ccsw` | 只是约定和文案；import 不看扩展名 |
| 导出 JSON 字段 `cswitchVersion`、`exportedFrom` 值 | `ccswVersion`、`ccsw` | `src/transfer.rs:70,257`；import 只读 `version/encrypted/accounts/activeAccountNumber`，旧导出文件仍可导入 |
| User-Agent `cswitch/<ver>` | `ccsw/<ver>` | `src/claude/usage.rs:17` |
| 临时目录前缀 `cswitch-login-` | `ccsw-login-` | `src/codex/login.rs:85` |
| 文档文件名 `*-cswitch-*.md`（specs 2 个、plans 3 个） | `*-ccsw-*.md` | `git mv`，并修链接：`README.md:222-224`、`src/lib.rs:3`、`src/claude/mod.rs:3`、`CHANGELOG.md:129`、specs/plans 互链 |
| release 资产名 `cswitch-<tag>-<target>` | `ccsw-…` | `.github/workflows/release.yml:41-50` |
| GitHub URL `github.com/newbdez33/cswitch` | `…/ccsw` | README（releases 链接、`cargo install --git`）、Cargo.toml |

不改：`cswap`（claude-swap 原项目名，README/docs/research 里是故意提的）、git 历史、`CODEX_HOME`/`CLAUDE_CONFIG_DIR` 等外部变量名。

TUI 不渲染程序名（`src/tui/` 和 `examples/` 没有引用程序名或版本），`docs/tui-*.png` 预计不用重截；改完渲染一次确认即可（截图方法见记忆 `cswitch-tui-screenshots.md`）。

### 3.3 数据迁移（用户本机 `~/.cswitch` 里有真实数据：cache、credentials、cswitch.log、sequence.json）

建议：启动时一次性自动迁移，放在 `src/paths.rs` 构造 `Paths` 的地方（`user_home`/`CSWITCH_HOME` 分支，约 40-65 行）之后、拿 store 锁之前：
- 条件：`CCSW_HOME` 未设置、`~/.ccsw` 不存在、`~/.cswitch` 存在。
- 动作：`fs::rename(~/.cswitch, ~/.ccsw)`；把 `ccsw.log`、`.1/.2/.3` 从 `cswitch.log*` 改名；把 `sessions/*/.cswitch-shared.json` 改名为 `.ccsw-shared.json`。
- stderr 提示一行：`moved ~/.cswitch to ~/.ccsw (shells pinned with 'cswitch env' need 'ccsw env' again)`。
- 测试用临时 HOME，绝不碰真实 `~/.cswitch`、`~/.claude`、`~/.codex`、Keychain。
- 备选（更简单、更保守）：不自动迁移，发现 `~/.cswitch` 且无 `~/.ccsw` 时拒绝启动并打印 `mv ~/.cswitch ~/.ccsw`。缺点是旧日志名和旧清单名留着。

### 3.4 文档与版本
- CHANGELOG 加 `## Unreleased` → `### Changed`：项目改名为 `ccsw`（二进制、crate、仓库）；store 从 `~/.cswitch` 迁到 `~/.ccsw`（首次运行自动迁移）；日志 `ccsw.log`；导出扩展名 `.ccsw`、字段 `ccswVersion`；所有 `CSWITCH_*` 环境变量改为 `CCSW_*`。
- 建议版本：改名单独发 **v0.4.0**（新仓库很快有一个 release），第二阶段发 v0.5.0。版本沿用 0.x 序列，CHANGELOG 旧条目保留。
- 不要把旧标签推到新仓库：`release.yml` 对每个 `v*` 标签触发，会用旧名字重建三个旧版本。只推 `main`。

### 3.5 顺序
1. 从 `origin/main` 切 `newbdez33/rename-ccsw`，按 3.2/3.3/3.4 改，跑门禁（见 §6），提交。
2. `git grep -i cswitch` 只剩 CHANGELOG 历史条目和「renamed from cswitch」说明；`cargo build` 产物是 `target/debug/ccsw`；`ccsw --help`、导入导出测试、`tests/session_run.rs` 全绿。
3. 用户删库建库后：`git remote set-url origin https://github.com/newbdez33/ccsw.git`，`git push -u origin main`。
4. 推 `rename-ccsw` 分支开 PR（新仓库的 #1），squash 合并，`git tag -a v0.4.0`，推标签触发 release。
5. 把 `newbdez33/cswitch-claude-auto` rebase 到新 main，上面两个 docs 提交里的文件名和内容同样改名（`docs/plans/2026-10-08-cswitch-claude-auto-phase2.md` → `…-ccsw-…`），再进入待办 B。

## 4. 待办 B：第二阶段（Claude Code 自动切换）执行

- 计划：`docs/plans/2026-10-08-cswitch-claude-auto-phase2.md`（1711 行，8 个任务；改名后路径变化）。规格：`docs/specs/2026-10-07-cswitch-claude-provider-design.md` §9。
- 用户还没审阅计划。审阅后选执行方式：推荐 **Subagent-driven**（`superpowers:subagent-driven-development`），或 Native（`superpowers:executing-plans`）。
- 设计要点（已获用户批准）：引擎保留 `provider` 字段但产品只构造 `Provider::Claude`；`auto codex` 或没有 Claude 账号时 stderr 拒绝并 exit 1；`keychain unavailable` 像 `token expired` 一样 hold；事件 `schemaVersion: 2` + `provider: "claude"`，状态文件仍为 v1；`autoswitch.model` 拼写错误只警告一次；TUI 自动视图仅 Claude，没有 Claude 账号时显示 ` OFF ` 和提示。
- 执行约束：实现者用 sonnet，不用 haiku（第一阶段 haiku 伪造过测试摘要、还加了署名）；每个 dispatch 写明禁止署名（现在钩子和设置也会拦）；测试命令必须 `env -u CODEX_HOME`。
- 第一阶段的裁决台账在 scratchpad（会话结束即失效），关键裁决已经体现在代码和 spec 里。

## 5. 待办 C：后续阶段
- 第三阶段：Claude 账号的 session 模式（`run/env/map` 目前对 Claude 槽位返回 `CLAUDE_SESSION_LATER`）。
- 第四阶段：export/import v2、导入 `.cswap` 文件。
- 两个阶段都还没有计划，需要先用 `superpowers:writing-plans` 按 spec 写。

## 6. 工作约定
- 门禁：`cargo fmt --check && cargo clippy --all-targets -- -D warnings && env -u CODEX_HOME cargo test --all`（宿主机导出了 `CODEX_HOME`，不 unset 会挂掉 `tests/session_run.rs`）。
- MSRV 1.88，Rust 2024；`grep` 在本机是 `ugrep`（脚本里用 `grep` 时注意）。
- 单元测试绝不碰真实 Keychain、`~/.claude`、`~/.codex`、网络。
- 永远不要发布 `~/.cswitch`、`auth.json`、token。
- 提交不加 `Co-Authored-By`，PR 不加 AI 署名脚注（现在由钩子和 settings 强制）。
- 不发布 Claude Artifact；要给用户看 HTML 放 `~/showme/`，地址 `http://j-studio.ts.gcu.local:8091/<file>.html`。
- 不建 `superpowers` 目录，用 `docs/specs/`、`docs/plans/`。
- worktree 规则：不 `cd` 主仓库；不用裸 `git stash`/`pop`。
- PR 用 squash 合并；CHANGELOG 标题格式 `## vX.Y.Z — YYYY-MM-DD`；发布 = 推 `v*` 标签触发 `.github/workflows/release.yml`。
- 对话用猫娘口吻、中文。

## 7. 关于 GitHub「claude」贡献者残留的调查（2026-10-08）
- GitHub 没有官方的「移除贡献者」功能；贡献者列表只由默认分支的提交元数据决定。改写历史后，侧栏和 Insights 图用不同缓存，刷新要 24 小时到一周以上；有人等了几周仍未刷新。
- 社区里确认有效的办法：复制出新仓库、把旧仓库改名归档（即用户现在的方案）；也有人靠在 Settings → Blocked users 里 block 用户 `claude` 让它消失（部分有效）。
- 一个被采纳的回答称 Settings → Code security and analysis → AI-powered features 里有「AI contribution attribution」开关，但后续多人找不到该开关，未经证实。
- 官方文档（Removing sensitive data）：改写后提交仍可通过 SHA 和 PR 引用访问；可通过 GitHub Support 门户申请删除缓存视图和 PR 引用（提供仓库名、受影响 PR 数量、first changed commits），但 Support 可能只对敏感数据受理。
- 预防：`~/.claude/settings.json` 的 `attribution`/`includeCoAuthoredBy`（已设）+ 全局 commit-msg 钩子（已装）。
- 来源：
  - https://github.com/orgs/community/discussions/175200
  - https://github.com/orgs/community/discussions/188915
  - https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository
  - https://devactivity.com/insights/decoding-stale-contributor-displays-a-github-cache-conundrum-for-development-activity/
  - https://innovatetechie.com/remove-claude-as-github-contributor

## 8. 文件索引
- 规格：`docs/specs/2026-09-29-cswitch-design.md`（总体）、`docs/specs/2026-10-07-cswitch-claude-provider-design.md`（Claude provider，§9 自动切换、§12 TUI、§14-16 分层/阶段）。
- 计划：`docs/plans/2026-10-07-cswitch-claude-provider-phase1.md`（已完成）、`docs/plans/2026-10-08-cswitch-claude-auto-phase2.md`（待执行）。
- 研究：`docs/research/codex-porting.md`（§8 Codex app-server 只认同一账号，自动切换对 Codex 无效的依据）、`docs/research/cswap-*.md`。
- 核心代码：`src/autoswitch.rs`（引擎）、`src/collect.rs`（用量采集）、`src/switcher.rs`（切换）、`src/claude/*`（Keychain/credentials/live/locks/oauth/usage）、`src/paths.rs`、`src/transfer.rs`、`src/session.rs`、`src/tui/*`。
- 测试：`tests/support/mod.rs`（驱动真实二进制 + mock 服务）、`tests/e2e_auto.rs`、`tests/auto_once.rs`、`tests/cli_claude.rs`。
