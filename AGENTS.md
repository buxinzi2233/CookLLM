# CookLLM Agent 指南与操作规范

## 1. `docs/` 目录定位与提交规则（最高准则）

- **定位**：`docs/` 目录仅用于存放 `README.md` 中引用的演示截图及维护者文档（如 `发布指引.md`），不参与 Tauri 应用打包与源码构建。
- **仅 `docs/` 变动时的处理规范**：
  - 当项目内**仅有 `docs/` 目录下的文件存在修改**（如用户更新了截图图片、修改了文档）时：
    1. **提交与推送须明确授权**：普通“更新文档”只授权编辑；“提交”与“推送”分别确认，只有用户明确授权两者后才执行 `git add docs/`、`git commit -m "docs: ..."` 并 `git push origin master`，且仅包含本次授权文件；
    2. **禁止修改版本号**：绝不要修改 `package.json`、`src-tauri/Cargo.toml`、`src/data.ts` 等任何版本号；
    3. **禁止重新编译**：无需运行 `npm run build` 或 `cargo check`；
    4. **禁止发布版本与打 Tag**：严禁创建或推送 `v*` Tag，绝不触发 GitHub Actions Release workflow。
- **纯指引/计划文档例外**：仅修改 `AGENTS.md`、`.snow/plan/` 下计划文档、README Linux 说明或上述 `docs/` 文档时，只审阅差异、核对路径/命令依据、链接和规则一致性，无需安装依赖、运行测试或构建；不升版本、不发版，提交推送仍须明确授权。混有功能代码变动时遵循第 3 节。

## 2. 软件版本发布规则

- 只有在**实际修改了应用功能/代码**，且**用户明确要求发版**（如输入“发版”、“版本号+1”、“发布新版本”）时，才执行完整的发版流程：
  - 详细规范见 `docs/发布指引.md`，**必须优先使用项目内置的 `scripts/release.mjs` 发布脚本执行自动化发版**；其中提交、推送和 Tag 操作须取得明确授权：
    1. 提升版本：运行 `npm run release:bump -- <patch|minor|major>`（自动同步 6 处版本号并在 `CHANGELOG.md` 与 `README.md` 插入占位段落）；
    2. 完善说明：根据本次实际改动内容，编辑 `CHANGELOG.md` 与 `README.md`，将占位符「- 待补充」替换为真实更新日志；
    3. 执行发布：运行 `npm run release -- --yes`（脚本默认执行版本核对、日志校验、`npm run build`、`cargo check --manifest-path src-tauri/Cargo.toml` 及 `npm run tauri build`，随后精确暂存发布文件、创建提交并打 `vX.Y.Z` Tag 推送至 GitHub 触发 Release workflow）；
    4. 严禁人工手动逐个文件替换版本号，防止漏改；README 更新日志只保留最近 5 个版本。
  - 6 处同步对象为 `package.json`、`package-lock.json`（含根包版本）、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`、`src-tauri/Cargo.lock` 的 CookLLM 条目、`src/data.ts` 的 `APP_VERSION`。`npm run release:check` 用于核对发布状态；不要以 `--skip-check` 绕过原有构建门槛。

## 3. 功能开发与代码修改完成后的构建规范

- 在完成功能开发与代码修改后，除执行基本的类型与语法检查外，主动为用户执行完整的本地编译打包（`npm run tauri build`），确保可执行文件产物就绪供用户直接使用与验证。
- 定向测试用于开发中快速定位，并不替代上述完整打包要求；涉及 IPC、配置迁移、下载或进程生命周期时，还需核对相应失败路径与关键交互。
- 若受平台或依赖条件阻碍，记录实际命令、失败原因和未完成项，明确“实现已完成，验证未完成”；不要把静态检查或 Windows CI 当作 Linux 打包通过的证据。

## 4. 架构入口与改动边界

- CookLLM 是本地推理引擎/模型桌面管理器，技术栈为 **Tauri 2 / Rust + React 19 / TypeScript / Vite 6**；前端使用 npm 与 `package-lock.json`。
- `src/App.tsx`：页面编排、配置状态、服务/下载交互；`src/components/`：各页面与弹窗。
- `src/tauri.ts`：前端 IPC 与事件封装；`src/types.ts`：共享数据契约；`src/data.ts`：默认配置、预设及旧配置迁移/导入合并。
- `src-tauri/src/lib.rs`：Tauri 命令注册、配置读写、进程生命周期、GGUF/NINFER 元数据、模型下载与应用/引擎更新。
- `src-tauri/tauri.conf.json`：窗口和打包配置；`src-tauri/tauri.linux.conf.json`：Linux deb/rpm/AppImage 覆盖配置；`src-tauri/capabilities/default.json`：窗口权限；`scripts/release.mjs` 与 `.github/workflows/release.yml`：本地发布和 CI 发版入口。
- `src-tauri/src/platform.rs`：平台辅助逻辑；`src-tauri/src/linux_engine_update.rs`：Linux CPU/Vulkan/CUDA 资产选择、校验、归档与托管安装；`linux_engine_update/cuda.rs`：驱动上限、双包配对、私有 runtime；同目录 `tests.rs`、`download_tests.rs`、`cuda_tests.rs` 承接夹具与真包入口。
- `src-tauri/src/linux_model_download.rs`、`linux_model_download/tests.rs`：Linux owned async transfer、完整文件名 `.part` / 自有 sidecar、Range/If-Range、pause/cancel、目标 lease/generation 与删除边界；Windows 保留旧 transfer。不要自动迁移未知 legacy 缓存或清理未知 JSON/symlink。
- `src-tauri/src/linux_app_update.rs`：Linux 安装 Context、官方资产复查、私有 receipt 与安装分派；`linux_app_update/appimage.rs`：AppImage 原子替换、备份、旧进程等待与 React 启动确认/回退（含真实 helper 测试）；`linux_app_update/tests.rs`：更新边界夹具。
- `src-tauri/src/linux_port_tests.rs`、`src-tauri/src/linux_system_tests.rs`：Linux 生命周期与桌面系统夹具；`scripts/test-linux-paths.mjs`、`scripts/test-linux-ui.mjs`：路径与 UI/IPC 夹具，后者导入 `test-linux-app-update.mjs` 与 `test-linux-cuda-ui.mjs`。
- 既有真实验收脚本使用 Python stdlib + WebKitWebDriver，不需 tauri-driver/selenium：`scripts/test-linux-desktop.py` 为 X11 桌面；`scripts/test-linux-interactions.py` 为私有 niri/Wayland seat 的 Send、剪贴板、预设/删除/退出；`scripts/test-linux-downloads.py` 为本地 HTTP + 真实 Tauri 下载 IPC。后两者自行隔离无服务 D-Bus；使用独立 HOME/XDG 与专属新建空 `/tmp` output，参数及未验边界见验收记录。
- `scripts/test-linux-upgrade.py`：stdlib + 系统 OpenSSL + WebKitWebDriver，本地 TLS 发布元数据 fixture 与真实旧/新 AppImage GUI 的检查/下载/安装、附着确认及单实例交接；不代表正式线上发布。`scripts/test-linux-desktop-services.py`：独立 niri/Wayland、真实 Thunar 与 GI/GTK 原生 SNI/DBusMenu consumer；协议测试 host 不是发行版 panel，渲染与物理菜单边界见验收记录。
- `scripts/build-linux.mjs`：Linux 本地 Tauri 打包入口；`.github/workflows/linux-check.yml`：非发布 Linux 门禁。
- 调整配置 schema 或 IPC 时同步 TypeScript 类型、调用参数与 Rust Serde 结构，保持现有 camelCase 序列化/参数契约和旧配置默认值、迁移兼容性。
- 用户配置由 Tauri `app_config_dir()` 下的 `config.json` 保存；模型默认在 `app_data_dir()/models`，也可使用用户自定义目录；引擎目录由配置或 `app_data_dir()/llamacpp` 决定。这些是**平台用户目录，不是源码目录**；不要读取/清理真实配置、Token、模型缓存或用户模型来完成文档维护，验证使用独立 fixture。
- `src-tauri/gen/schemas/` 是生成产物，不手工修改；权限/配置从对应源文件调整后由工具生成。

## 5. 真实命令与定向验证

以下命令从仓库根目录执行，依据 `package.json`、Cargo 配置及源码核对；命令表本身不代表每条均已运行，实际证据与未验范围见 Linux 计划：

| 用途 | 命令及边界 |
| --- | --- |
| 依赖准备 | `npm ci`：仅需要安装依赖时执行，不是文档维护的验收步骤 |
| 浏览器预览 | `npm run dev`：Vite 页面/演示或回退逻辑；原生 IPC、进程管理等需桌面环境验证 |
| 桌面调试 | `npm run tauri dev`：需 Rust 与目标平台 Tauri 系统依赖 |
| 前端类型与构建 | `npm run build`：实际为 `tsc && vite build` |
| Rust 编译检查 | `cargo check --locked --manifest-path src-tauri/Cargo.toml` |
| 元数据/引擎识别测试 | `cargo test --manifest-path src-tauri/Cargo.toml --lib gguf_meta_tests::` |
| 可执行文件查找测试 | `cargo test --manifest-path src-tauri/Cargo.toml --lib tests::find_executable_`：匹配递归查找、当前目录优先及缺失文件三个测试 |
| Linux 路径/删除夹具 | `node scripts/test-linux-paths.mjs` |
| Linux UI/IPC 夹具 | `node scripts/test-linux-ui.mjs`：导入应用更新与 CUDA UI 夹具；计数与执行时点见验收记录 |
| Linux 应用更新夹具 | `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib linux_app_update::`；真实 AppImage helper 为 ignored，隔离 D-Bus 与实包参数见 [验收记录](docs/Linux验收记录.md) |
| 真实 Linux 桌面 | `python3 scripts/test-linux-desktop.py --help` 查看必填 app/engine/model/output；真实执行命令与全新空输出目录要求见 [验收记录](docs/Linux验收记录.md) |
| 真实 Wayland 交互 | `python3 scripts/test-linux-interactions.py --help`：app/engine/model/output；需父 Wayland 与私有 niri seat，不需 `--session`；自行隔离 D-Bus |
| 真实模型下载 IPC | `python3 scripts/test-linux-downloads.py --help`：app/model/output；本地 HTTP、X11、独立 HOME/XDG/D-Bus，不替代外网 HF/代理/UI 按钮 |
| 真实跨版本升级 | `python3 scripts/test-linux-upgrade.py --help`：old-app/new-app/old-version/new-version/output，engine/model 成对提供；本地 TLS fixture，实跑参数与版本边界见验收记录 |
| 真实桌面服务 | `python3 scripts/test-linux-desktop-services.py --help`：app/engine/model/output、父 Wayland/niri/GI GTK/Thunar；专属空 `/tmp` output，host 不代表发行版 panel |
| Linux 模型下载夹具 | `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib linux_model_download::` |
| Rust 库测试 | `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib`；需真实上游包的 ignored 测试单独提供 fixture 后执行，见计划 |
| Linux 生命周期/系统夹具 | `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib linux_port_tests::` |
| Linux 引擎更新/下载夹具 | `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib linux_engine_update::` |
| 完整本地打包 | `npm run tauri build`：功能代码修改后的必需步骤，参见第 3 节 |
| Linux 完整本地打包 | `npm run build:linux -- --ci`：调用本地 Tauri build，默认仅对子进程设置 `NO_STRIP=1`，保留用户覆盖；包含前端 beforeBuildCommand，仍遵循第 3 节完整打包门槛 |

- 当前未定义 `npm test`、`npm run lint` 脚本，不虚构这些验收命令。
- Rust 原有 `gguf_meta_tests`、`tests` 模块继续保留；Linux 新增测试入口见第 4 节。平台模拟、SSR、假 server 与真实上游包版本查询各有边界，均不替代 Windows 实机、真实模型或完整 GUI 验收；报告 ignored 的原因与单独执行证据，不以跳过冒充通过。

## 6. Linux 现状与后续计划

- Linux x86_64 路径/进程/硬件/桌面/代理、CPU/Vulkan/CUDA 自动引擎与异步模型下载已实现。CUDA 官方双包私有 runtime 仅作用于托管 CUDA 子进程；用户偏好与实际 full 版本分离。应用按 Context 分派：AppImage 原子替换/备份与 React 确认回退，deb/rpm 仅图形安装器交接，未知布局/缺工具/digest 为 manual；Windows 保留原流程、未实机。
- 本机核心施工基本完成：第四轮主仓库编译、Rust 库测试与三包构建通过；前端路径/UI、官方 CUDA 12.8 双包、本地下载 IPC 与独立 Wayland 交互沿用第三轮证据，本轮未重跑 Node tests。新增本地模拟发布下真实旧/新 GUI 跨版本升级、Thunar 定位与原生 tray DBus 协议通过；GTK 菜单渲染为部分验证。AppImage exact APPDIR 修复保留唯一只读挂载/UID/source/fstype 与 runtime 设备/inode/open FD 核验，不以环境变量证明运行身份。
- 正式 GitHub 线上发布、deb/rpm 系统安装、发行版 panel/物理托盘菜单及 picker、外网 HF/代理/UI 下载、Windows、干净发行版/云 CI、CUDA 13/13.4 实包与 runtime 再分发条件仍待验；`.tmp` 的 0.3.3 仅测试快照，主版本保持 0.3.2。官方 NInfer 锁定 sm120a、选定/验证 RTX 5090，不保证所有 50 系或社区 fork；本机 sm89 不满足官方版，fork 独立验。保留原发布门槛及四阶段条件。当前与历史证据见 [Linux 累计验收记录](docs/Linux验收记录.md)，阶段/具体需求见 [移植计划](.snow/plan/linux-port.md)，AppImage 局部参数见 [README Linux](README.md#-linuxx86_64移植验证阶段)。
