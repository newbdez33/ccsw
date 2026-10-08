# ccsw 交接（2026-10-08）

## 当前状态

- 用户已删除旧仓库，新仓库为 `https://github.com/newbdez33/ccsw`，公开。
- `origin` 已更新；此次改名的基线为 `4f1eaa4`，包含 Claude provider 第一阶段和第二阶段的规格、计划。
- 当前工作区为 `cabezon`，改名分支为 `newbdez33/rename-ccsw`。
- 改名范围：crate、二进制、命令文案、环境变量、默认数据目录、日志、session 清单、导出元数据、发布资产及文档。
- 用户明确要求**不实现首次启动迁移**，只提供手动步骤；没有其他用户。旧环境变量也不保留兼容别名。
- 改名前检查：本机通过 Cargo 安装了旧版 v0.2.0，旧数据目录包含账号、凭据、缓存和日志，没有 session 清单，`~/.ccsw` 尚不存在。
- 此次代码改名不修改真实数据或旧安装；手动迁移和卸载命令已在对话中提供给用户，不纳入项目文档。验证账号后再卸载旧程序，禁止用 `purge` 迁移。
- 项目文档统一使用 `ccsw`，包括历史条目、示例、环境变量和文件名，不保留旧项目名称。
- 版本暂保持 `0.3.0`，改名记入 `Unreleased`。建议下一次正式发布使用 `v0.4.0`，Claude 自动切换随后单独发布。
- 不推送旧标签：`release.yml` 对每个 `v*` 标签触发，会用旧名称重建旧版本。
- 本地验证已通过：格式检查、Clippy（警告视为错误）、全量 439 项测试、锁定依赖构建，以及新二进制的帮助和版本输出。

## 下一阶段：Claude Code 自动切换

- 计划：`docs/plans/2026-10-08-ccsw-claude-auto-phase2.md`。
- 规格：`docs/specs/2026-10-07-ccsw-claude-provider-design.md` §9。
- 第二阶段尚未实现，计划尚待用户审阅；此次先完成改名。
- 已确定的设计：引擎保留 `provider` 字段，但产品只构造 `Provider::Claude`；`auto codex` 或无 Claude 账号时拒绝执行。
- Keychain 不可用时保持当前账号；事件使用 `schemaVersion: 2` 和 `provider: "claude"`；状态文件仍为 v1。
- `autoswitch.model` 拼写错误只警告一次；TUI 自动视图仅覆盖 Claude。
- 第三阶段为 Claude session 模式，第四阶段为 export/import v2 和 `.cswap` 导入，均尚未实现。

## 工作约定

- 门禁：

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
env -u CODEX_HOME cargo test --all
```

- 宿主环境设置了 `CODEX_HOME`，测试前必须移除该变量。
- MSRV 为 Rust 1.88，edition 为 2024。
- 测试使用临时目录、假 CLI 和本地 mock，禁止访问真实 Keychain 或真实账号数据。
- 不进入或改名主仓库及 Orca 工作区目录；不新建 worktree，不重写历史，不使用裸 `git stash` / `pop`。
- 提交和 PR 不加代理署名；全局 Git 钩子已在本机和 Windows 机器 `jx` 配置。
- PR 使用 squash 合并；发布通过推送 `v*` 标签触发 `.github/workflows/release.yml`。
- 对话使用中文猫娘语，代码、注释、提交和 PR 使用简洁 English。
