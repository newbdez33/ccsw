# ccsw 交接（2026-10-08）

## 当前状态

- 仓库为 `https://github.com/newbdez33/ccsw`，公开；默认分支为 `main`。
- 改名已通过 PR #1 合并；PR #2 完成 `v0.4.0` 发布。四个平台的归档和校验文件已验证。
- 用户已完成旧安装和数据的手动迁移，并确认可用。不实现首次启动迁移，不保留旧环境变量别名，不使用 `purge` 迁移。
- Claude 自动切换第二阶段已实现，版本设为 `v0.5.0`。按 PR → CI → squash merge → 标签发布流程交付。
- 当前工作区为 `cabezon`；本阶段分支为 `newbdez33/claude-auto-phase2`。
- 项目文档统一使用 `ccsw`。不推送旧标签，避免重新构建历史版本。

## 已完成：Claude Code 自动切换

- 计划：`docs/plans/2026-10-08-ccsw-claude-auto-phase2.md`，八项任务均完成。
- 规格：`docs/specs/2026-10-07-ccsw-claude-provider-design.md` §9。
- 引擎保留 `provider` 字段，产品只构造 `Provider::Claude`。`auto` 和 `auto claude` 运行；`auto codex` 或无 Claude 账号时在首个 tick 前拒绝执行。
- 采集仅涉及当前 provider，保护 tick 开始时和重新读取后的 live slot；Claude 活跃 token 不由自动切换刷新，Codex 凭据和 daemon 不受影响。
- Keychain 不可读时持续保持当前账号，超过 30 分钟也不触发 failover；过期 token 原有的 30 分钟上限不变。该规则消除了原计划中上限与“绝不 failover”的矛盾。
- 事件使用 `schemaVersion: 2` 和 `provider: "claude"`；状态文件仍为 v1。
- `autoswitch.model` 拼写提示仅在所有相关用量可用于决策时检查一次，不额外请求用量，不用不可读状态下的旧缓存作判断。
- TUI 自动视图仅展示 Claude，筛选后重新计算活跃账号；没有 Claude 账号时显示 OFF，隐藏不可用操作，不启动引擎。
- 测试使用临时数据、假 Keychain 和本地 mock；新增混合 provider 隔离、Keychain 超时保持、延迟模型提示、CLI 拒绝和 TUI 状态覆盖。
- 本机门禁：格式检查、Clippy 和全量 450 项测试通过。发布构建与跨平台 CI 结果以对应 PR 和 Actions 记录为准。

## 后续阶段

- 第三阶段：Claude session 模式，尚未实现。
- 第四阶段：export/import v2 和 `.cswap` 导入，尚未实现。
- 这两个阶段需要单独规划；本次不包含自动迁移或新的兼容层。

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
