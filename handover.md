# ccsw 交接（2026-10-08）

## 当前状态

- 仓库为 `https://github.com/newbdez33/ccsw`，公开；默认分支为 `main`。
- 改名已通过 PR #1 合并；PR #2 完成 `v0.4.0` 发布。四个平台的归档和校验文件已验证。
- 用户已完成旧安装和数据的手动迁移，并确认可用。不实现首次启动迁移，不保留旧环境变量别名，不使用 `purge` 迁移。
- Claude 自动切换第二阶段已通过 PR #3 合并并发布 `v0.5.0`。
- Claude session 第三阶段已实现，版本设为 `v0.6.0`；按 PR → CI → squash merge → 标签发布流程交付。
- 当前工作区为 `cabezon`；本阶段分支为 `newbdez33/claude-session-phase3`。
- 项目文档统一使用 `ccsw`。不推送旧标签，避免重新构建历史版本。

## 已完成：Claude Code 自动切换

- 计划：`docs/plans/2026-10-08-ccsw-claude-auto-phase2.md`，八项任务均完成。
- 规格：`docs/specs/2026-10-07-ccsw-claude-provider-design.md` §9。
- 引擎保留 `provider` 字段，产品只构造 `Provider::Claude`。`auto` 和 `auto claude` 运行；`auto codex` 或无 Claude 账号时在首个 tick 前拒绝执行。
- 采集仅涉及当前 provider，保护 tick 开始时和重新读取后的 live slot；Claude 活跃 token 不由自动切换刷新，Codex 凭据和 daemon 不受影响。
- Keychain 不可读时持续保持当前账号，超过 30 分钟也不触发 failover；过期 token 原有的 30 分钟上限不变。该规则消除了原计划中上限与“绝不 failover”的矛盾。
- 独立审查发现的 live token 副本漏洞已修复：采集和目标刷新按凭据指纹保护，自动切换也排除其他身份下保存的同一 live token；切换前重新检查。
- Keychain 失败且没有可读文件回退时，所有 Claude 刷新均暂停；回退文件恢复后可以刷新真正不活跃的凭据。
- 旧 provider 的隔离记录保持不动，不会误发归属 Claude 的恢复事件。
- 事件使用 `schemaVersion: 2` 和 `provider: "claude"`；状态文件仍为 v1。
- `autoswitch.model` 拼写提示仅在所有相关用量可用于决策时检查一次，不额外请求用量，不用不可读状态下的旧缓存作判断。
- TUI 自动视图仅展示 Claude，筛选后重新计算活跃账号；没有 Claude 账号时显示 OFF，隐藏不可用操作，不启动引擎。
- 测试使用临时数据、假 Keychain 和本地 mock；新增混合 provider 隔离、Keychain 超时保持、延迟模型提示、CLI 拒绝和 TUI 状态覆盖。
- 本机门禁：格式检查、Clippy 和全量 455 项测试通过。发布构建与跨平台 CI 结果以对应 PR 和 Actions 记录为准。

## 已完成：Claude session 模式

- 计划：`docs/plans/2026-10-08-ccsw-claude-session-phase3.md`。
- 按用户要求直接参考和移植 cswap `3a4e5c1` 的源码；README 注明来源，NOTICE 保留完整 MIT 许可。
- `run` 和 `env` 支持两种 provider，按账号归属启动对应 CLI；`run claude` 和 `run codex` 按各自最近的目录映射解析。
- 同一目录可映射两种 provider；旧映射文件按 Codex 读取。`unmap DIR PROVIDER` 只删除一个，省略 provider 删除两个。
- Claude profile 使用独立配置目录和按原始路径哈希的 Keychain 项；共享默认目录的设置、指令与扩展，可选择合并并共享历史。
- 保留 POSIX `exec`：在后续命令中回收轮换凭据；Windows 在子进程退出后也回收。只采纳身份匹配且更新的 token。
- 运行中的 profile 和相同 token 副本不被刷新；从准备到启动持有预约，网络刷新通过凭据指纹锁协调。切换、删除、移动和 purge 前检查会话占用，多个槽位共用凭据时只锁一次。凭据或 PID 记录不可读时保守拒绝。
- 重新登录后将旧 profile 标为失效，避免它覆盖新备份；会话退出后再替换。purge 清理托管 profile 的 Keychain 项，不影响默认或外部 profile。
- `env` 仅准备配置和输出命令，不预约整个 shell；直接启动 CLI 后依赖其 PID 记录。需要完整启动保护时使用 `run`。
- 同账号默认登录保留直接启动行为；`--require-session` 可拒绝该路径。`env --unset` 清除两种 home，指定 provider 可只清除一个。
- 自定义 Claude home 不读写默认 OAuth Keychain 项或默认托管 API-key 项。
- 独立审查的五项重要问题均已补充失败回归测试并修复，无延期小问题。最终验证和跨平台结果以本次 PR 和 Actions 为准。
- 最终本机门禁：484 项测试、Clippy、格式和锁定依赖构建通过，8 项隔离 CLI smoke 通过。

## 后续阶段

- 第四阶段：export/import v2 和 `.cswap` 导入，尚未实现。
- 首次快照加载前进入自动视图会显示 OFF；等待加载后重新进入即可。
- 第四阶段需要单独规划；本次不包含自动迁移。

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
