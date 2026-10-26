# Linux 累计验收记录

## 范围与证据时点

2026-10-12 第四轮文档收尾：只核读现有源码、metadata、日志与结果；仅更新五份指定文档，未复跑构建、测试、安装或联网，未执行 Git 写操作、云发布或主版本提升。主仓库与当前产物仍为 `0.3.2`，`.tmp` 的 `0.3.3` 仅为跨版本测试快照。本记录区分**实现、本机实测、完整发布门禁**；四阶段条件见 [移植计划](../.snow/plan/linux-port.md)，局部通过不替代整阶段验收。

环境沿用 CachyOS x86_64，Node 26 / npm 12，Rust 1.99，GTK 3.24，WebKitGTK 2.54（4.1 API），OpenSSL 3.6；NVIDIA RTX 4070 Laptop（sm89）。旧轮 `/tmp` 路径只作历史索引，重启清理后不保证仍存在；当前资产与日志已归档到仓库忽略的 `.tmp/`，不纳入 Git，也不是发布资产。

## 第四轮：当前结果（真实跨版本升级与桌面服务）

### 1. 当前主仓库检查与产物

已有日志 `.tmp/linux-validation/final-upgrade/{check.log,rust-tests.log,build.log}`：`cargo check --locked --manifest-path src-tauri/Cargo.toml` 成功、零 Rust warning；`cargo test --locked --manifest-path src-tauri/Cargo.toml --lib` 为 **125 passed / 0 failed / 3 ignored**；`npm run build:linux -- --ci` 的 `tsc && vite build` 与 deb/rpm/AppImage 三包通过。三个条件 ignored 未计入 passed；其历史独立执行见第三轮。Vite chunk size 等提示仍保留，零 Rust warning 不外推全部日志。

本轮无前端改动，**未重新执行 Node tests**；路径 18/18、UI 61/61 沿用第三轮 `final-cuda/{paths.log,ui.log}`，不是当前源码再次运行的结果。第三轮 Rust 124 项与旧产物只作历史，不串入本轮 125 项。以下为当前主版本 **0.3.2** 的实物字节数及 SHA-256（路径相对仓库根）：

| 当前产物路径 | 字节数 | SHA-256 |
| --- | ---: | --- |
| `src-tauri/target/release/cookllm` | 6066600 | `a5a27bf2e6c57350ebf30a5ddd7091f29c49cdd2c9813ff63c2d9523ac1a6cbe` |
| `src-tauri/target/release/bundle/deb/CookLLM_0.3.2_amd64.deb` | 2963190 | `852185ed008b90a4dd11a615b1560d2f85ffd9be49c0a279cfd1f6be95619450` |
| `src-tauri/target/release/bundle/rpm/CookLLM-0.3.2-1.x86_64.rpm` | 2964391 | `7f60e7a612a2c1b067758a8040036ebfd822d6745fe6239cf7dafd717e4f91f6` |
| `src-tauri/target/release/bundle/appimage/CookLLM_0.3.2_amd64.AppImage` | 113011192 | `4e7835805329b5b7483e5b08efc22fcab9f5739e2be847088d10a0c65fa36e2e` |

### 2. 唯一产品修复：当前 GUI 的 FUSE 挂载绑定

`src-tauri/src/linux_app_update.rs::image_mount_binding` 原按同 filename 全局 `matches == 1`：真实旧 GUI 已退出，旧 daemonized FUSE runtime 仍存活，新旧同名包挂在不同 APPDIR；新 React 已 render，`notify_ready` 却因两挂载失败，90 秒后 helper 自动回退。此次只统计**当前 GUI exact APPDIR**，要求该挂载唯一、ro、正确 UID、source/fstype；same APPDIR 的 stacked/重复/覆盖挂载、wrong UID、rw、other FS 仍拒绝。`active_image_runtime` 对 runtime 可执行文件与 backing-image open FD 的设备/inode、UID 及 `/dev/fuse` FD 核验保留，未放宽为仅环境变量证明。

`src-tauri/src/linux_app_update/tests.rs` 新增 `kernel_mount_binding_allows_old_image_at_another_mountpoint` 一项：两挂载正反顺序、仅旧 APPDIR、重复/covered、UID、ro；既有测试继续覆盖 other FS。125 项与第三轮 124 项的差额来自此新增回归项。主源码这两个修复文件与 fixture 对应文件 byte 相同已核对。

### 3. 本地模拟发布、真实旧/新 GUI 升级

证据根 `.tmp/linux-validation/upgrade-afterfix-20261012/`：`result-report.md`、`final-integrity.json`、`afterfix/results.json`、`afterfix/command.json`、`afterfix/attached/new-gui.png`；最终 `status=passed`、controller exit 0。`scripts/test-linux-upgrade.py` 使用 Python stdlib、系统 OpenSSL 与 WebKitWebDriver。隔离 snapshot 的六处版本为 **0.3.3**（仅 `.tmp/linux-validation/upgrade-20261012/snapshot`），独立 target 不覆盖主 target，主仓库六处版本始终 **0.3.2**。测试新包不是 root 发版资产，禁止将它混入正式发布清单。

| 测试包/原生输入 | 字节数 | SHA-256 |
| --- | ---: | --- |
| 旧 `old-032.AppImage` / 保留的 backup | 113011192 | `38d32206b50433be92e83328e43d8f53e63c5220923c025519be16458676633b` |
| 新 `CookLLM_0.3.3_amd64.AppImage`（fixture） | 113015288 | `94628775531947b6f5968064ff791e3c80027d4d7b54589c1eff683281744eed` |
| fixture 新 native | — | `45bf1e5ab96b8430356c592dfd7bb51b87cb348f9a8c8db9e69d3fc8364a64fd` |

- **网络边界**：127.0.0.1 CONNECT TLS fixture 只响应官方 API/GitHub 指定发布路径，临时 CA 仅以局部 `SSL_CERT_FILE/DIR` 供 app 信任；保持 TLS 校验与 `acceptInsecureCerts=false`，无系统 CA 改动。4 次允许 GET 200；7 次非 fixture GET 403、sent=0，不转发，Authorization 均 absent。模拟的是发布元数据/服务，不是 GUI、包或安装 IPC。
- **真实链路**：旧 GUI `plugin:app|version=0.3.2` → `check_app_update` available v0.3.3 → `download_app_update` 的 size/SHA、0600 receipt → `install_app_update` restarting；旧 PID 552900 消失，helper 启动新 PID 553172，ready 存在且 result=`startup-confirmed`。旧 runtime 未人为提前杀死，现场有两个同名不同 APPDIR 的只读 FUSE 挂载。
- **附着而非重启测试**：WebKitWebDriver `--target=127.0.0.1:46299` 附着 exact helper 启动 GUI；附着前后 PID/starttime、socket inode **7584362** 与 PID553172 的 **FD18** 归属不变。真实 `plugin:app|version=0.3.3`、load_config 一致、再次检查 latest；截图为该 GUI，不是重启后另装测试实例。
- **交接及数据**：采样无旧新 GUI 重叠，前后重复启动均单实例；新 duplicate 退出 0，原新 PID 和单窗口/句柄不变。CPU server PID/group553111 已停、端口46353释放；配置语义与 config.json 字节 SHA `6a06c7b28b309dddc6cf92688a77162b7ba933cf3bce36d847ef3870ee1e7a60`、模型 SHA `270cba1bd5109f42d03350f60406024560464db173c0e387d91f0426d3bd256d`、旧备份 SHA 均保持。
- **计时与清理**：观察旧 GUI 消失约 **1.116 s**、confirmed 约 **2.017 s**，React render **540 ms**；仅本次记录，不作性能宣传。最终 GUI/helper/FUSE/WebKit/两个 driver/D-Bus 自有进程清零；driver/proxy/server/inspector 五端口均释放，`fixture_pids={}`、`dbus_group_members=[]`。

**失败证据独立保留**：`beforefix/` 与 `beforefix-report.md` 是产品缺陷导致的真实回退；`harness-capability-a/` 是附着 capability processID=0 的错误断言，已用内核监听 socket/FD 身份核验；`harness-sampling-b/` 是交接 poll 混入主动重复启动的短命进程，已分开采样与单实例探测。两次工具诊断不混入最终 `afterfix/` passed，未降低 ready、SHA、原生 version、数据保留或单实例断言。`final-integrity.json` 的 root 摘要记录的是随后主仓库重构建**之前**的时点，不用于当前主产物校验。

**边界**：本地模拟发布下真实跨版本升级已通过；正式 GitHub 线上发布、deb/rpm 系统安装、Windows 与修复后新包失败回退重测仍未验。修复前真实回退和既有回退 unit 是不同证据，不声称最终通过案例同时重测了全部失败矩阵。

### 4. 真实 Thunar 与原生 tray DBus 协议

`scripts/test-linux-desktop-services.py` 新增后语法检查、真实执行已有记录，未为此重新 build。汇总 `.tmp/linux-validation/desktop-services-20261012-0103/results.json` 为 `status=passed`，**10 entries = 9 functional passed + 1 render partial**，不是 10/10。输入 native SHA 为第三轮/挂载修复前 `7175353013e8ea7f807b7e2530bf098ba6976d5199cd5da33f4b0a1b79936ac9`；本次桌面行为与 AppImage 挂载修复无关，不冒充使用当前 root SHA 重验。

- **Thunar 4/4**（`01-filemanager-initial/`）：中文空格文件打开父目录、缺文件但 parent 存在、目录直接打开、无 parent 明确 error。私有 niri 新 socket、实际 PID/exe 与截图核验；产品打开 parent，不声称选中文件。
- **native tray DBus 5/5**（`02-tray-lifecycle/`）：GI/GTK fixture host 消费真实 SNI 注册、icon 与 DBusMenu/layout；tray=true 后真实 W3C Close 隐藏窗口但 app/CPU engine 继续；真实 `DBusMenu.Event(clicked)` show 恢复；再 hide 后第二 exe 退出0，原 PID/window 恢复且 only one；quit 清 engine/端口 **34419**。
- **render partial**（`03-tray-gtk-render/`）：图标在截图可见，GTK widgets 持有真实 menu 标签，但菜单截图被裁，标签的可视确认仅 partial；物理鼠标菜单、真实发行版 panel 未验。协议测试 host 不是发行版 panel。首轮 absolute icon path 被当主题名加载的问题修复在测试 consumer，非产品；日志保留。
- **隔离与清理**：三轮最终自有 groups/members/fixture_pids 为空，宿主 D-Bus 和 clipboard 不动；父 Wayland 只供新座席建立，其他桌面/发行版、物理 picker/drag 仍待验。

### 5. NInfer 官方要求与后续入口

本轮主代理已重新读取官方 README；本次文档收尾不联网。当前官方版明确 **64 位 Linux、选定/验证 RTX 5090，构建拒绝 sm120a 以外架构**；CUDA 13.1 为已验证开发工具链，CMake 不设 CUDA version floor，不能把 13.1 写成硬性最低版本。官方当前设计为 one GPU / one resident model / 无 CPU offload。**这不是所有 50 系通用保证，更不是所有社区 fork 都要求 50 系；本机 RTX 4070 Laptop sm89 不满足当前官方版，fork 需独立验证。** CookLLM 的名称/模型识别不等于官方引擎已运行。[NInfer README](https://github.com/Neroued/ninfer/blob/HEAD/README.md "官方说明 64 位 Linux、RTX 5090、sm120a 构建限制及单 GPU 常驻模型") [NInfer CMake](https://github.com/Neroued/ninfer/blob/HEAD/CMakeLists.txt "官方构建锁定 120a，未设置 CUDA 工具链版本下限")

后续主动验收入口（这里只记录参数，不运行）：`python3 scripts/test-linux-upgrade.py --help`，必填 old-app/new-app/old-version/new-version/output，engine/model 成对提供；完整实跑参数在 `afterfix/command.json`，不直接复用已非空 output。`python3 scripts/test-linux-desktop-services.py --help`，真实执行需 app/engine/model/output、父 Wayland/niri、GI GTK、Thunar 与 WebKitWebDriver，专属新建空 `/tmp` output；`--tray-only` / `--tray-ui-only` 分项结果不充当完整功能结果。正式线上/系统安装、Windows、干净 VM/云 CI、物理 panel 及其他硬件仍按原四阶段条件等待验证，细项清单见第三轮第 6 节的当前边界。

## 第三轮：历史结果（CUDA、模型下载与真实交互）

以下保留第三轮完整执行证据与当时产物；应用/桌面新增结果以第四轮为准，18 路径/61 UI 等未本轮重跑的证据仍可沿用。

### 1. 最终检查与产物

以下为已有执行记录，**本次只读核对，没有重跑**。日志根为 `.tmp/linux-validation/final-cuda/`，命令从仓库根执行：

| 命令 | 结果 | 日志（相对上述根） |
| --- | --- | --- |
| `cargo check --locked --manifest-path src-tauri/Cargo.toml` | 成功，零 Rust warning | `check.log` |
| `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib` | **124 passed / 0 failed / 3 ignored**；其中模型下载模块 26 项通过 | `rust-tests.log` |
| `node scripts/test-linux-paths.mjs` | **18/18**，含模拟 Windows 分支 | `paths.log` |
| `node scripts/test-linux-ui.mjs` | **61/61**；已导入 app-update 与 cuda-ui 夹具，平台/IPC/SSR 模拟非桌面实测 | `ui.log` |
| `npm run build:linux -- --ci` | beforeBuildCommand 前端构建及 deb/rpm/AppImage 三包成功 | `build.log` |

三个 ignored 分别是外部 CPU 包、真实 AppImage helper、真实 CUDA 双包；按条件独立执行，不计入 124 项。CPU 与 AppImage 前轮分别单跑通过，保留历史结论，未宣称第三轮重验；CUDA 第三轮单跑 **1 passed**，日志 `.tmp/linux-validation/cuda/real-cuda.log`。第三轮构建后当时仅测试脚本等待条件与文档改动；第四轮挂载产品修复与当前重构建在上文另列。Vite chunk size、桌面 GTK/appindicator 提示仍记录在各日志，不把 check 零 warning 扩展为所有日志无提示。

| 相对 `src-tauri/target/release/bundle/` 路径 | 字节数 | SHA-256 |
| --- | ---: | --- |
| `deb/CookLLM_0.3.2_amd64.deb` | 2963074 | `760f72407e9e459ddb9e9ad5b86dce31f4c684d1cd1d64cfab4945f2414803cf` |
| `rpm/CookLLM-0.3.2-1.x86_64.rpm` | 2964240 | `0345c31151ab6d0066ea4c58547c6b2b732871b442b250da91990005e7bddcaa` |
| `appimage/CookLLM_0.3.2_amd64.AppImage` | 113011192 | `38d32206b50433be92e83328e43d8f53e63c5220923c025519be16458676633b` |

原生 `src-tauri/target/release/cookllm` SHA-256：`7175353013e8ea7f807b7e2530bf098ba6976d5199cd5da33f4b0a1b79936ac9`，与第三轮两份真实验收结果的 `inputs.app.sha256` 一致。旧轮摘要只标识当时产物，不用于核验当前文件。

### 2. Linux CUDA 自动双包与偏好修复

- Linux x86_64 **CPU / Vulkan / CUDA 自动更新已接通后端与 UI**。CUDA 请求支持 `auto`、`12`、`13`、完整 `major.minor`；auto 在驱动上限内优先兼容 12，再选 13。驱动能力使用系统 `/usr/bin/nvidia-smi` 或 `/bin/nvidia-smi` 的 XML，限时 5 秒，清除继承 LD 环境；未知上限、驱动不匹配或缺配对资产有明确提示，保留手动入口。
- engine 与 cudart 必须同 tag、完整 CUDA 版本、架构，严格复核官方 SHA-256、size、URL。runtime 下载专用上限 **1 GiB**；engine 与 App 共用下载仍为 **512 MiB**。两包同 cancel、累积进度、同盘 staging；归档仅接收三类 CUDA soname 与受限许可证小目录。私有 `cuda-runtime/` 配合同级链接，`managed_launch_env` 仅为托管 CUDA 子进程设置指向私有目录的 `LD_LIBRARY_PATH` 并清 `LD_PRELOAD`，CPU、手动/外部引擎不强加该环境。
- 安装健康检查包含 `--list-devices`，必须枚举真实 CUDA 设备；失败保留旧引擎与配置。用户从官方 runtime 下载到 appdata，不复制 `/opt/resolve`、不安装系统库；这不是 redistribution legal 审计结论，独立分发/打包许可仍需审阅。
- review P2 已修：`config.llamaCudaVersion` 保存用户 `auto/12/13/full` 偏好；`engine.cudaVersion` 与 `cuda_version.txt` 保存实际 full。前端传原偏好，并用 fresh `matchAsset` 的 URL/name/tag 锁定本次安装；CPU/Vulkan 安装不清 CUDA 偏好。auto 安装 12.8 后仍能选择未来 12.9。证据 `.tmp/linux-validation/cuda/preference-summary.txt`。

官方 metadata 为 `.tmp/linux-fixtures/b11534/release.json`；以下资产已下载并校验在同目录。[llama.cpp b11534](https://github.com/ggml-org/llama.cpp/releases/tag/b11534 "官方 b11534 Release 的 engine 与 cudart 配对元数据已归档并核验")

| 官方资产名 | 字节数 | SHA-256 |
| --- | ---: | --- |
| `llama-b11534-bin-ubuntu-cuda-12.8-x64.tar.gz` | 171975896 | `514a33ea11728297725086b2e8c3c01199b5706c6e8a04d56de3a7ac6f026bc0` |
| `cudart-llama-b11534-bin-ubuntu-cuda-12.8-x64.tar.gz` | 594377702 | `86caaf1a54fffa4579a71559dc6e2384108849556dd3aae1577d31da196fb632` |

合计下载 **766353598 B**；runtime 实际 4 个归档成员、展开 **868893264 B**，需另留 staging/旧版空间。ignored 真包测试复用 production prepare/merge/helper 路径：CUDA0 RTX 4070 Laptop，ctx 512、t 2、ngl 99、n 16；health/chat HTTP 200，非空 16 tokens，`offloaded 6/6 layers to GPU`，退出释放端口 36611。清 LD 环境不含 `/opt/resolve`。证据 `.tmp/linux-validation/cuda/{SUMMARY.txt,real-cuda.log,real-server.log}`。微型模型部分 Flash Attention 算子在 CPU，不是整体 CUDA fallback，也不作性能宣传。

**CUDA 13/13.4 只有 metadata/pair/selection 测试，没有实包运行。** 该 12.8 证据证明配对资产与托管准备/启动路径，不等同 GUI 中下载按钮、升级/切换及全部故障矩阵实机闭环。早期默认 verbosity 3 未输出卸载行导致断言失败，保留 `real-cuda-verbosity3.log`、`real-server-verbosity3.log`；测试用 `-lv 4` 后通过，不混作本轮最终结果。

### 3. 模型下载实现与真实 IPC（8/8）

实现入口 `src-tauri/src/linux_model_download.rs` 与 `linux_model_download/tests.rs`：

- Linux 使用一个 owned async transfer，无后台 IO worker；headers/body 等待期间 pause/cancel 每 50 ms 观察，idle 60 s、overall 3600 s。移除原 blocking 双 GET 探路。目标 lease 与 registry generation 防双 writer 和旧回调写入。
- 临时文件为完整文件名追加 `.part`（如 `model.gguf.part`），sidecar 为 `model.gguf.part.cookllm.json`；自有 `format/source/validator/total` 记录来源 URL、强 ETag 或 Last-Modified、总长，续传发送 `If-Range`。206 严校 start/end/total/length；200 忽略 Range 时重写 partial，不追加旧内容。截断/错误不发送 done、不覆盖旧目标；pause 保留缓存，cancel 清理后完成，无后续写入。
- HF token 只发送给精确 HTTPS `huggingface.co` / `hf-mirror.com`，不按域名子串放行。Windows 只统一 `.part` 命名、旧 transfer 保留，未 Windows 实机。旧 `model.part` 未知 legacy 缓存保留，不自动迁移。
- `remove_local_file` 仅删除 partial 的请求顺带清自有 sidecar；主文件/part 并行删除不会因互锁失败。未知 JSON、symlink 保留；活跃写入或模型根目录外删除返回错误。

证据 `.tmp/linux-validation/downloads-cookllm-downloads-r26ftG/results.json`：`status=passed`，**8 cases / 34 真实进度 events**。stdlib `ThreadingHTTPServer` 仅绑定 127.0.0.1，通过原生 WebKit W3C 和真实 Tauri IPC 调用，独立 HOME/XDG/无服务目录 D-Bus，无外部联网；GDK x11 配合局部 DMABUF 参数。

| 案例（results.cases） | 观察到的结果 |
| --- | --- |
| `pause_resume` | pause 稳定 131072 B；ETag If-Range + `Range: bytes=131072-`，206 完成后模型 hash 匹配 |
| `cancel_stream` / `cancel_stall` | 持续流与停滞流 cancel 约 0.305 s settled；观察期缓存未再现，旧目标 hash 保留 |
| `ignore_range_200` | 服务端忽略 Range，200 重写后 hash 正确 |
| `truncated_body` / `invalid_206` | 无 done、不覆盖旧目标 |
| `parallel_delete` | 删除暂停 partial/自有 sidecar，未知 JSON 保留 |
| `target_conflict` | 同目标第二任务明确冲突，不出现双写 |

HTTP 请求无 Authorization；`cleanup` / `outer_cleanup` 的 ports、pids、groups、HTTP threads 全清。原生输入 hash 匹配上述最终产物。**未验：真实外网 HF/TLS/token、代理路由、下载按钮 UI、Windows及其他发行版**；本地 IPC 实包不替代这些项目。

### 4. 独立 Wayland 真实交互（10/10）

证据 `.tmp/linux-validation/final-cuda/interactions/verified-results.json`：`status=passed`，**10 cases**。`scripts/test-linux-interactions.py` 使用 stdlib、WebKit W3C、私有 niri nested（不需 `--session`），独立 Wayland socket / seat、runtime 0700、无 service 目录 D-Bus；`isolation.backend=wayland`、`host_clipboard_accessed=false`。输入 app、engine、model hash 记录在结果中，app 为上述最终 SHA-256。

- `isolated_seat_clipboard`、`clipboard_and_log_copy`：独立座席中文多行剪贴板，真实 native `clipboard_write` / `wl-paste` 往返及 UI 点击日志复制。
- `ui_import`：真实 Tauri drag 事件 fixture 给候选路径，W3C 点击确认；**不是物理拖拽或原生文件选择器**。
- `preset_switch`、`ui_start`、`ui_stop`：UI 选择预设 B 后真实启动/停止。
- `webui_send`：iframe textarea W3C 输入、点击第三方 Send，页面非空 16 tokens 并核对 metrics；**没有用 fetch/chat invoke 替代 Send**。
- `case_sensitive_disk_delete`：task UI 删除 `Tiny.gguf`，磁盘与配置提交后只保留 `tiny.gguf`，原模型 hash 不变。
- `window_close_cleanup`：引擎运行时点击窗口 Close，托管组清空、端口释放。
- `logs_copy_error_visible`：复制错误故障注入仅影响新 seat，UI 错误可见，私有 selection 保留。

worker `cleanup` 时 D-Bus wrapper 仍待外层退出；最终 `outer_cleanup.members=[]`、`fixture_pids=[]`，端口全释放，不把中间状态误报残留。成功截图/日志已复制到同目录 `evidence/`（16 个文件）。

**保留一次失败**：同目录 `results.json` 为 failed，仅 `case_sensitive_disk_delete` 失败。旧脚本只等磁盘 unlink 就读配置，遇到真实异步写入中间态；修为 10 秒内等待 config 删除 commit 且另一模型保留，未改产品、未降低断言，重跑 10/10 通过。成功依据为 `verified-results.json`，不删除或混用失败证据。未覆盖文件管理器、托盘菜单、物理 picker/drag、完整 X11/Wayland/发行版矩阵；默认 AppImage Wayland GBM 局部 workaround 仍保留。

### 5. 后续主动验收入口（本次不执行）

三脚本的参数与隔离条件来自当前源码；每次使用**专属新建空 `/tmp` output**，不要清空旧证据。desktop 是既有 X11 验收入口（`--app/--engine/--model/--output`，历史命令见下文）；interactions 需父 Wayland、niri、wl-copy/wl-paste 与 WebKitWebDriver；downloads 需 X11、WebKitWebDriver 和至少 128 KiB 的本地 GGUF，脚本自行启动隔离 D-Bus。当前已归档 engine/model 路径如下，主动重验前先确认输入存在：

```bash
# 从仓库根；示例，不是本次已执行命令
APP="$PWD/src-tauri/target/release/cookllm"
ENGINE="$PWD/.tmp/linux-validation/interactions-20261011-224429/cpu/llama-b11534/llama-server"
MODEL="$PWD/.tmp/linux-fixtures/model/stories260K.gguf"
OUTPUT="$(mktemp -d /tmp/cookllm-interactions-recheck-XXXXXX)"
python3 scripts/test-linux-interactions.py \
  --app "$APP" --engine "$ENGINE" --model "$MODEL" --output "$OUTPUT"

OUTPUT="$(mktemp -d /tmp/cookllm-downloads-recheck-XXXXXX)"
python3 scripts/test-linux-downloads.py \
  --app "$APP" --model "$MODEL" --output "$OUTPUT"
```

脚本不需 tauri-driver/selenium；只操作隔离 fixture。结果与截图/日志可另归档到忽略 `.tmp/`，不能把运行缓存纳入提交。CUDA ignored 真包入口为 `linux_engine_update::cuda_tests::real_upstream_cuda_pair_health_http_chat_and_cleanup`，需绝对 `COOKLLM_LINUX_RELEASE_FIXTURE`（包含 release.json 与双包的目录）、`COOKLLM_CUDA_MODEL`、`COOKLLM_CUDA_LOG_DIR` 以及兼容真实 GPU；复用本地资产，不要求测试下载。

### 6. 剩余完整发布需求

本机核心施工基本完成；下列是原四阶段门禁的具体外部/手工需求，不因当前局部通过而移除：

| 待验项 | 所需环境/输入与记录 |
| --- | --- |
| 干净发行版 / 最低基线 | 干净 Ubuntu 22.04/24.04 与选定 RPM 桌面；记录 glibc/WebKitGTK/依赖、三包安装到首轮聊天和升级；维护者确认最低基线 |
| 桌面矩阵 | 第四轮Thunar4/4、原生tray协议5/5已通过；GTK菜单可视partial，发行版panel/physical鼠标菜单、其他桌面、有/无NVIDIA、原生picker/物理拖拽/配置重启及默认AppImage Wayland GBM仍待验 |
| 模型外网链路 | HF HTTPS/gated token、system/manual/direct 代理及 NO_PROXY；真实下载按钮、暂停/续传/取消/重试 |
| CUDA / 引擎管理 | CUDA 13/13.4 真包与兼容驱动 GPU；UI 下载/安装/升级/切换及磁盘满、权限、取消、健康失败保留旧版；不得把 12.8 微型推理外推 |
| 应用升级 | 第四轮本地fixture真实旧/新GUI跨版本与单实例/数据保留已通过；正式GitHub线上两个发布版本的更新/回退、修复后新包失败回退重测、deb/rpm系统安装/权限确认/版本核对及manual路径待验 |
| Windows / CI | Windows x64 实机旧配置、命名变化、下载/引擎/应用更新回归；授权云 CI 的双平台构建、五种包、摘要与 Release notes |
| NInfer | 当前官方锁sm120a/选定验证RTX5090，本机4070Laptop sm89不满足；官方Linux构建链/专用模型、其他硬件与fork独立验，kvmem对应分支/资产待确认 |
| 分发决策 | 维护者确认包签名/信任策略、runtime 许可及独立再分发条件、版本提升与发布授权 |

## 第二轮：历史证据（非当前状态）

以下保留 2026-10-10 的实现/验收时点、旧计数和旧产物摘要。其中“CUDA 自动关闭”“未下载 runtime”“Send/复制未验”只描述当时，当前已由第三轮结果更新；旧 `/tmp` 证据不保证仍在，历史命令不得直接当当前复验命令。CPU、独立 Vulkan 与同版本 AppImage helper 的前轮通过仍有效，不宣称本轮重验。

### 历史 1. 应用更新实现与真实包边界

- `src-tauri/src/linux_app_update.rs`、`linux_app_update/appimage.rs`、`linux_app_update/tests.rs` 已落地；真实 helper 测试位于 `appimage.rs` 内部 tests 模块。
- 根据真实安装 Context 匹配 x86_64 AppImage / deb / rpm；系统包名为 `cook-llm`，可执行文件名为 `cookllm`。重新查询官方元数据并核对 tag、URL、size、SHA-256；后端私有暂存 receipt 记录文件身份，安装前重新校验，防止 symlink 与文件改动。
- AppImage 在同目录原子替换并保留旧版备份；辅助进程等待旧进程退出才重启。React `report_startup_timing` → `notify_ready` 确认有 90 秒上限，未确认回退。启动确认不是模型健康检查。
- 实包修复：AppImage runtime daemonize 后并非 GUI 祖先进程，识别使用 FUSE mount、owner、设备/inode 与 open FD 核验；可写性使用 `faccessat`，避免 write-open 触发 `ETXTBSY`。
- deb/rpm 仅交接图形系统安装器，仍需用户确认和系统安装结果核验；“已交接”不等于升级完成。源码运行、未知安装布局、工具或资产 digest 缺失时显示 manual，引导手动安装。
- `AppUpdateDialog`、`SettingsPage`、platform、tauri、i18n 已接 `installKind/installSupported/installReason`、安装交接状态、取消/卸载后的晚回调隔离，以及 `source=app/engine` 进度互斥。Windows 原流程保持，未实机回归。

真实 AppImage helper 日志 `/tmp/cookllm-app-update-real-handshake.log`：`1 passed`、`startup-confirmed`；原件/旧备份/目标摘要一致，专属存活进程为 0，FUSE 清理已完成。**这是同版本真实副本替换，等待旧进程使用睡眠 fixture，不是官方跨版本升级或真实旧 GUI 单实例完整交接。** 正式跨版本升级、deb/rpm 图形系统安装交接仍待实跑。

历史 AppImage 单跑命令（对应第二轮包；当前同路径已是第四轮产物，不代表本轮重验。D-Bus 配置为旧 /tmp 路径，后续主动验收需先核对输入并另建隔离配置）：

```bash
cd /home/buxinzi/Projects/AI/LLM/CookLLM
env COOKLLM_APPIMAGE_FIXTURE=/home/buxinzi/Projects/AI/LLM/CookLLM/src-tauri/target/release/bundle/appimage/CookLLM_0.3.2_amd64.AppImage \
  COOKLLM_APPIMAGE_ISOLATED_DBUS=1 \
  dbus-run-session --config-file=/tmp/cookllm-app-update-isolated-dbus.conf -- \
  cargo test --locked --manifest-path src-tauri/Cargo.toml --lib \
  linux_app_update::appimage::tests::real_appimage_helper_react_handshake -- --ignored --exact --nocapture
```

前置条件：真实桌面/FUSE 环境和该包存在；上述 D-Bus 配置为独立会话、无服务激活目录的本机已验配置，使用前核对仍存在且保持隔离。`COOKLLM_APPIMAGE_ISOLATED_DBUS=1` 只是测试确认标志，不替代隔离配置；测试复制原包，不对提供的原包做替换。

### 历史 2. 最终检查、夹具与构建

以下命令均从 `/home/buxinzi/Projects/AI/LLM/CookLLM` 执行；结果是读取历史日志，不是本次复跑。

| 命令 | 已有结果 | 日志 |
| --- | --- | --- |
| `cargo check --locked --manifest-path src-tauri/Cargo.toml` | 成功，零 Rust warning | `/tmp/cookllm-app-update-real-check.log` |
| `cargo test --locked --manifest-path src-tauri/Cargo.toml --lib` | 88 passed、0 failed、2 ignored | `/tmp/cookllm-app-update-real-final-lib.log` |
| `node scripts/test-linux-paths.mjs` | 18/18；含模拟 Windows 分支，不是 Windows 实机 | `/tmp/cookllm-round2-paths.log` |
| `node scripts/test-linux-ui.mjs` | 41/41；导入 `test-linux-app-update.mjs`，是完整 UI 夹具数；平台/IPC/SSR 模拟不是桌面实测 | `/tmp/cookllm-round2-ui.log` |
| `npm run build:linux -- --ci` | beforeBuildCommand 的前端构建与三包成功 | `/tmp/cookllm-app-update-real-build.log` |

两个 ignored 分别是需真实 CPU 包的 `real_upstream_cpu_digest_extract_and_version` 和需真实 AppImage/隔离桌面的 `real_appimage_helper_react_handshake`。前者上轮单独 1 passed，日志 `/tmp/cookllm-stage3-review-real-cpu.log`；后者本轮单独 1 passed，见第 1 节。跳过项未计入 88 passed。

CPU 上轮资产 `llama-b11534-bin-ubuntu-x64.tar.gz` 约 17.8 MB，SHA-256 `d065dfb703c3dd21f09c7b4d86194938a32b27614dc635ac40cf6b4c950a782c`；版本 `0.6.0-dev (build 11534, commit 50e3e3e48)`。当时只验摘要/解压/版本，本轮真实推理见第 3 节。

最新构建日志比 `/tmp/cookllm-round2-build.log` 更新；之后只修改测试代码，无需为这些测试变动再次构建产品。Vite 有 chunk size 提示；helper 运行有 libayatana-appindicator deprecation 提示，不把“cargo check 零 warning”扩展为所有日志零提示。

| 相对 `src-tauri/target/release/bundle/` 路径 | 字节数 |
| --- | ---: |
| `deb/CookLLM_0.3.2_amd64.deb` | 2940018 |
| `rpm/CookLLM-0.3.2-1.x86_64.rpm` | 2941283 |
| `appimage/CookLLM_0.3.2_amd64.AppImage` | 112986616 |

- AppImage SHA-256：`bec062d44c55b1b892082599ecd999e97873476d85520c504d0f68b7d72308c6`。
- 原生 `src-tauri/target/release/cookllm` SHA-256：`b0e665129d5440b0f3224f4724f2a74446ae7e866e1c665c52350d8b8f0620aa`，与最终桌面结果的输入一致。
- 历史 linuxdeploy 旧 strip 的 `.relr.dyn` 错误及 `NO_STRIP=1` 局部措施继续有效；未关闭 Cargo release `strip=true`，详见 README。

### 历史 3. 最终新构建的真实 CPU 桌面验收

证据 `/tmp/cookllm-round2-desktop-rhq02xgf/results/results.json`：`status: passed`。`scripts/test-linux-desktop.py` 使用 Python 标准库、真实 `/usr/bin/WebKitWebDriver` 和 `TAURI_WEBVIEW_AUTOMATION=true`，不依赖 tauri-driver 或 selenium。脚本使用独立 XDG 目录，清除继承的 AppImage/动态库/llama 参数环境；本轮是**原生 X11 + `WEBKIT_DISABLE_DMABUF_RENDERER=1`**。

历史执行命令（归档说明，**不要直接复跑已非空的 output**）：

```bash
cd /home/buxinzi/Projects/AI/LLM/CookLLM
dbus-run-session -- python3 scripts/test-linux-desktop.py \
  --app /home/buxinzi/Projects/AI/LLM/CookLLM/src-tauri/target/release/cookllm \
  --engine /tmp/cookllm-desktop-real-acv0p6wo/engine/llama-b11534/llama-server \
  --model /tmp/cookllm-desktop-real-acv0p6wo/stories260K.gguf \
  --output /tmp/cookllm-round2-desktop-rhq02xgf/results
```

后续主动重验时，先明确提供仍存在的引擎/模型路径；以 `OUTPUT="$(mktemp -d /tmp/cookllm-desktop-recheck-XXXXXX)"` 创建全新空目录，并将上面末项换成 `--output "$OUTPUT"`。脚本要求 output 为 `/tmp` 下专属空目录，旧证据目录不清空、不覆盖。

- 真实 CPU 模型 stories260K：1,185,376 字节（约 1.185 MB），SHA-256 `270cba1bd5109f42d03350f60406024560464db173c0e387d91f0426d3bd256d`。
- `/health`、`/metrics`、聊天 HTTP 200，非空响应、16 completion tokens。
- 通过真实 `__TAURI_INTERNALS__.invoke` 导入中文/空格目录中的 `Tiny.gguf` 与 `tiny.gguf` 两条路径；两次启停释放进程组与端口，重启配置仍保留模型和引擎。
- 页面可见 `Service running`、真实日志；内嵌 iframe 加载 `llama-ui` DOM，iframe 内真实 fetch 聊天成功。不是 mock，也不是自动点击第三方 Send 控件。
- 剪贴板、文件管理器/目录打开、托盘菜单未验；本轮不声称 Wayland 完整交互通过。该 CPU 桌面结果中的 GPU unverified 仅表示此脚本未测 GPU；独立 GPU 证据见下一节。

保留上轮首屏证据：`/tmp/cookllm-final-smoke-58to4315/results.json` 中 native Wayland React 1681 ms、native X11（局部 DMABUF 参数）423 ms，观察 10 秒；默认 Wayland AppImage GBM 错误且无 React 上报。`/tmp/cookllm-appimage-compatible-lx71ud1o/results.json` 中 AppImage 局部禁用 DMABUF 后 React 591 ms、观察 12 秒。仅历史首帧冒烟，不作性能保证；README 的局部兼容命令保留。

### 历史 4. 独立 NVIDIA Vulkan / CUDA 推理

证据根 `/tmp/cookllm-gpu-verification-9h_k0vpz`：`results.json`、`summary.md`、`vulkan-verified-server.log`、`cuda-verified-server.log`。本轮读取现存证据，未再联网。

保留上轮只读采样：RTX 4070 Laptop 总显存 8188 MiB、系统 RAM 约 31247.51 MiB；Vulkan 枚举到 RTX 4070 与 AMD 610M。该历史采样本身不证明推理，本轮下述记录才是独立 NVIDIA 推理证据。

官方 b11534 资产的大小及摘要已匹配 [llama.cpp b11534](https://github.com/ggml-org/llama.cpp/releases/tag/b11534 "官方 b11534 Release 提供本轮核验的 Linux Vulkan 与 CUDA 12.8 资产")：

| 后端 | 字节数 | SHA-256 |
| --- | ---: | --- |
| Vulkan | 31761824（约 31.76 MB） | `f8d05dfc051572d3dfedff600cfce21210b39facd15801de5f286a823cb01d60` |
| CUDA 12.8 | 171975896（约 171.98 MB） | `514a33ea11728297725086b2e8c3c01199b5706c6e8a04d56de3a7ac6f026bc0` |

同一 stories260K 模型，显式选择 NVIDIA `--device Vulkan0` / `CUDA0`，`-ngl 99 -c 512 -t 2 -np 1 -n 16`；日志确认 `offloaded 6/6 layers to GPU`。health/chat HTTP 200、非空 16 tokens；两组进程全清、端口释放。CUDA 的微型模型 Flash Attention 部分算子使用 CPU，不是整个后端 CPU fallback，也不代表所有算子在 GPU。**这些微型模型数字不作性能宣传或大模型稳定性承诺。**

CUDA 运行库使用机器已有 `/opt/resolve/libs` 的 `libcudart.so.12.8.57`、`libcublas.so.12.8.3.14`、`libcublasLt.so.12.8.3.14`，仅在 `/tmp/cookllm-gpu-verification-9h_k0vpz/cuda-runtime` 链接精确 soname `libcudart.so.12`、`libcublas.so.12`、`libcublasLt.so.12` 并对子进程设置路径。**不要将整个 Resolve 目录加入 LD_LIBRARY_PATH**：早期实测其旧 `libggml-base.so.0` 覆盖官方引擎库，CUDA 启动失败；精确隔离后通过。

未安装系统库，也未下载约 600 MB runtime。CUDA 自动部署仍关闭；干净环境的 runtime 获取、分发许可和库冲突处置另验。本机手动运行结果不等于自动部署通过，也不等于 GUI 内 GPU 更新/切换链路已验。

### 历史 5. NInfer、Windows 与发行版剩余条件

- 官方 NInfer 不是 Windows 专用：README 明确 64 位 Linux 源码构建、RTX 5090 / sm120a，CMake ≥3.28、C++20，CUDA 13.1 是已验证开发工具链；CMake 锁定 120a。本机 sm89 不匹配，未编译或下载专用 v3 `.ninfer` 模型。[NInfer README](https://github.com/Neroued/ninfer/blob/HEAD/README.md "官方说明 Linux 源码构建及 RTX 5090、CMake 和 CUDA 工具链条件") [NInfer CMake](https://github.com/Neroued/ninfer/blob/HEAD/CMakeLists.txt "官方构建限制 CMAKE_CUDA_ARCHITECTURES 为 120a")
- `1314521gjy/ninfer-fusion-kvmem` 是候选而非已确认对应 CookLLM 的分支；README 描述 Windows 86/89/120a EXE，不构成 Linux 资产证明。Release API 曾受 403 限流，资产枚举不完整。[候选 README](https://github.com/1314521gjy/ninfer-fusion-kvmem/blob/HEAD/README.md "候选分支说明 Windows EXE 与对应 GPU 架构，和 CookLLM 的关联仍待确认")
- 上述源快照已保存为 GPU 证据目录中的 `ninfer-README.md`、`ninfer-CMakeLists.txt`、`ninfer-fusion-kvmem-README.md`；本次依据快照，不宣称重新核验在线最新状态。
- 本机仅有 Wine，无 Windows CookLLM 产物；Rust 为系统工具链，sysroot `/usr`、仅 Linux target，无 rustup。未把 Wine 或 Linux 编译当 Windows 实机回归。
- Docker 有 CLI，但 `/var/run/docker.sock` 缺失；未启动服务、拉取镜像或运行云 CI。干净发行版、最低 glibc/WebKitGTK、完整 X11/Wayland 桌面矩阵仍待验证。
- 仍保留的门禁：真实旧版→新版应用及配置保留、旧 GUI 单实例交接、deb/rpm 系统交接；引擎升级/切换与完整故障矩阵；模型下载取消/续传实机；复制、目录、托盘；Windows、无 NVIDIA、干净目标发行版和云发布验证；签名与分发信任策略。完整门禁未齐，四阶段不强行勾选完成。

## 第一轮：准备与核心基线（历史索引）

工程准备、平台核心施工、CPU实包摘要/解压/版本及隔离首屏冒烟为第一轮/前轮基线：native Wayland与局部DMABUF X11首屏、默认AppImage Wayland GBM失败及局部参数通过的详细时点保留在第二轮“历史3”；CPU实包独立通过的旧命令/日志索引保留在“历史2”。不将基线静态核读、版本查询或首屏冒烟写成完整GUI、桌面矩阵或线上发版通过。准备轮授权与固定源码基线见移植计划第1—2节；未新增历史运行计数。
