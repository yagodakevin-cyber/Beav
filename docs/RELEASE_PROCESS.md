# 版本发布与在线更新标准化流程（固定规则）

> 适用范围：Beav 桌面端（Windows NSIS / macOS DMG）、浏览器插件（Chrome MV3）。
> 本文档为发布工作的唯一标准流程，所有人员发布前必须完整阅读并逐步执行。
> 更新源绑定：`https://github.com/yagodakevin-cyber/Beav`（唯一合法来源，禁止引入任何第三方更新源）。

---

## 1. 版本号规则（语义化版本 SemVer）

格式：`MAJOR.MINOR.PATCH`（如 `2.10.0`）。

| 变更类型 | 升级位 | 示例 |
| --- | --- | --- |
| 不兼容的架构/数据格式变更 | MAJOR | 2.x → 3.0.0 |
| 新功能、模块重构 | MINOR | 2.9.2 → 2.10.0 |
| 缺陷修复、文案/资源微调 | PATCH | 2.10.0 → 2.10.1 |

**三处版本号必须完全一致**（Release 工作流会强校验）：

1. `desktop/package.json` —— 版本唯一权威源
2. `Plugin/src/manifest.json` —— 浏览器插件版本
3. `desktop/src-tauri/Cargo.toml` / `Cargo.lock` —— 由脚本自动同步，不手改

**操作要求**：

```powershell
# 1) 修改 desktop/package.json 与 Plugin/src/manifest.json 的 version 字段
# 2) 同步 Cargo 版本（在 desktop/ 目录执行）
node ./scripts/sync-version.mjs
# 3) 三方核对（预期输出三个相同版本号）
node -p "require('./package.json').version" ; node -p "require('../Plugin/src/manifest.json').version"
```

**校验标准**：新版本号不得与仓库已有 tag 冲突（tag 已被上游历史占用时，沿版本线向后取号，禁止复用/覆盖旧 tag）：

```powershell
git ls-remote --tags origin | Select-String "refs/tags/v"
```

---

## 2. 发布前校验（全部通过才允许发布）

| 校验项 | 命令（在 `desktop/` 执行） | 通过标准 |
| --- | --- | --- |
| 前端类型 | `pnpm exec tsc --noEmit -p tsconfig.json` | EXIT=0 |
| 更新器单测 | `cargo test --manifest-path src-tauri/Cargo.toml --bin beav commands::system::app_update` | 全部 ok |
| 桌面端冒烟 | 启动应用，设置 → 常规设置 → 应用版本卡片 | 「检查更新」可点击且结果文案正确 |

---

## 3. 提交与标签

**提交规范**（Conventional Commits，与仓库历史风格一致）：

- 格式：`<type>: <摘要>`，type 取 `feat` / `fix` / `chore` / `docs`
- 正文用列表说明关键变更；一次发布一个主提交 + 必要的修正提交
- **禁止入库**：`.trae*/`、`*.log`、会话报告目录（`beav-*`）、任何含密钥的文件（提交前执行 `git status --short` 逐项核对未跟踪文件）

**标签与推送**：

```powershell
git add <明确列出的文件>          # 禁止 git add -A 盲加
git commit -m "feat: ..."        # 提交信息经 here-string 传入
git tag -a v<版本号> -m "Beav v<版本号> - <一句话说明>"
git push origin HEAD:main v<版本号>   # 分支与标签一次推送；标签触发 Release 工作流
```

**校验标准**：推送后 `git rev-list --count HEAD..origin/main` 为 0；`gh run list` 可见 `Release v<版本号>` 工作流进入 in_progress。

**异常处理**：

| 异常 | 处理方式 |
| --- | --- |
| tag 已存在（`already exists`） | 用 `git ls-remote --tags origin` 查占用历史 → 换新版本号重新走第 1 节，禁止 `-f` 覆盖远端 tag |
| push 被拒（非 fast-forward） | `git fetch origin` 后核对差异；本地分支必须基于最新 main，禁止强推 main |
| 提交混入敏感文件 | 立即 `git rm --cached` 并修正提交；若已推送则先在 GitHub 吊销对应密钥再联系仓库管理员清理历史 |

---

## 4. Release 工作流（tag 触发，自动发布）

`.github/workflows/release.yml`，由 `v*` 标签推送触发：

1. **verify-version**（ubuntu）：校验 tag = `desktop/package.json` 版本 = `Plugin/src/manifest.json` 版本，不一致立即失败
2. **build-windows**（windows-latest）：下载 ffmpeg/ffprobe sidecar 二进制（BtbN/FFmpeg-Builds，按 target triple 命名放入 `desktop/src-tauri/binaries/`，该目录不入库）→ Rust 工具链（dtolnay/rust-toolchain + rust-cache 加速）→ `tauri build --bundles nsis` → 产出 `Beav_<版本>_x64-setup.exe`
3. **build-macos**（macos-latest，`continue-on-error`）：DMG 为可选产物，失败不阻断正式发布
4. **build-extension**（ubuntu）：`Beav-extension-<版本>.zip`
5. **publish**：`sha256sum --binary` 生成 `SHA256SUMS.txt`（排除自身）→ `gh release create --verify-tag --generate-notes` 发布全部资产

**监控命令**：

```powershell
gh run list --repo yagodakevin-cyber/Beav --limit 3      # 状态总览
gh run watch <run-id> --repo yagodakevin-cyber/Beav      # 实时跟踪
gh release view v<版本号> --repo yagodakevin-cyber/Beav  # 发布结果与资产清单
```

**产物完整性校验标准**（Release 页必须同时具备）：

- `Beav_<版本>_x64-setup.exe`（命名与更新器匹配规则一致：`-setup.exe` + `x64`）
- `SHA256SUMS.txt`（缺此文件更新器将拒绝安装——安全设计，fail-closed）
- 本地核对：`Get-FileHash <下载的安装包> -Algorithm SHA256` 与清单条目一致

**异常处理（CI 失败兜底）**：

```powershell
# CI 连续失败且短时间无法修复时，用本地构建产物手动发布（仅正式版本号，禁止 pre-release 混入）
gh release create v<版本号> --target <main 的 commit sha> --title "Beav v<版本号>" --generate-notes --verify-tag <安装包> SHA256SUMS.txt
```

注意：手动兜底后 CI 的 publish 步骤可能因 Release 已存在而失败，属预期，无需处理。

---

## 5. 本地全量构建（发布验证 / CI 兜底用）

```powershell
# 在 desktop/ 目录执行（beav 品牌）
pnpm tauri:build:beav
```

- 产物：`desktop/src-tauri/target/release/bundle/nsis/Beav_<版本>_x64-setup.exe`
- 校验：确认文件存在、体积正常（约 70-80 MB）、`Get-FileHash` 计算值与将发布的 SHA256SUMS 条目一致
- dev 实例（`tauri-dev` 调试目录）与 release 构建互不冲突，可并行

---

## 6. 在线更新全流程验证（每个正式版本发布后执行一次）

前置条件：GitHub 已发布正式 Release（非 draft、非 pre-release）且资产齐全；本机装有**低于**该版本的正式安装版。

1. **基座安装**：运行旧版安装包静默安装 `Beav_<旧版>_x64-setup.exe /S`，确认 `%LOCALAPPDATA%\Beav` 与注册表卸载项版本号正确
2. **版本检测**：启动应用 → 设置 → 常规设置 → 「检查更新」；预期弹出更新弹窗并显示新版本号
3. **下载与校验**：确认更新后观察进度事件（下载 → 校验 → 安装）；SHA256 不匹配时应用必须放弃安装并保留旧版可用（fail-closed）
4. **安装替换与重启生效**：应用自动退出 → 静默安装 → 自动重启 → 设置页版本号变为新版本，并出现「已更新到 v<版本>」提示；`%APPDATA%\Beav\updates\` 下 pending 标记被清除、生成 `backup\<旧版本>` 备份
5. **回滚验证**：设置页出现「回滚到 v<旧版本>」按钮 → 确认后应用自动重启并恢复旧版本
6. **无更新场景**：新版本应用上点「检查更新」→ 预期提示「当前已是最新版本」

**场景覆盖要求**：手动检查触发、启动后静默检测（15 秒后首查 + 每 30 分钟）、正式 Release 放行、draft/pre-release 与第三方仓库 URL 拒绝（由 `releases/latest` 语义 + 单测覆盖）、更新失败回滚。

**异常处理**：

| 异常 | 处理方式 |
| --- | --- |
| 检查更新报「更新源请求失败」 | 核对 GitHub API 可达性与仓库 Releases 是否存在；应用 6 小时节流可改用设置页手动检查（force）绕过 |
| 「安装包校验失败（SHA256 不匹配）」 | 重新上传 Release 资产并重算 SHA256SUMS.txt；应用侧无需改动 |
| 更新后应用无法启动 | 用 `%APPDATA%\Beav\updates\backup\<版本>` 备份目录按第 6.4 步回滚，或重装旧版安装包 |
| 误触发开发构建安装 | 属预期防护：debug 构建（`debug_assertions`）直接拒绝安装/回滚，防止自毁 |

---

## 7. 红线（任何时候不允许）

1. 禁止将更新源、下载地址、校验逻辑指向 `yagodakevin-cyber/Beav` 之外的任何仓库或第三方服务
2. 禁止发布 draft / pre-release 作为正式更新（更新器按设计拒绝）
3. 禁止无 `SHA256SUMS.txt` 的 Release 用于应用内更新
4. 禁止强推 main 与已发布的 tag；禁止复用历史版本号
