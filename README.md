# 🦙 CookLLM

> **llama.cpp 轻量管理器** —— 面向 `llama.cpp` / GGUF 模型打造的 Windows 桌面管理程序。
> 内置模型库、运行参数模板、一键启停、显卡状态监控、实时运行日志以及内置对话网页，本地大模型日常调试管理一套搞定。

---

## ✅ 更新日志

### v0.3.2

#### 🚀 新增功能与体验优化

- **全面支持 NInfer / NInfer-KVMem 原生高性能引擎**
  - **引擎托管与智能识别**：支持登记并一键切换 NInfer 与 NInfer-KVMem 引擎分支，自动检测运行时架构与硬件加速后端。
  - **原生 .ninfer 模型解析**：支持直接导入并解析 .ninfer 格式模型，自动提取架构、参数量与量化信息，并自动生成对齐官方推荐的最佳实践预设。
  - **预设参数动态适配**：预设编辑器智能联动当前引擎，提供 KV 缓存量化（rk8v4 / int8 / bf16 / nvfp4）、预填充分块、快速预热内核、CUDA 显存策略、硬件 Profile 路由、KVMem 分层配额等专属调优参数。
  - **原生 MTP 投机解码**：支持自适应 MTP 投机采样与 N-gram 模式匹配加速，大幅提升长上下文推理吞吐。
  - **实时性能精准监控**：适配 NInfer 原生监控端点，实时统计瞬时 TPS、Prefill 吞吐速率与槽位占用。

- **双轨制模型格式视觉体系**
  - **全新专属格式徽标**：设计高辨识度的矢量标识体系——GGUF与 NINFER。
  - **差异化指标呈现**：模型卡片与列表动态适配，GGUF 模型展示显存卸载层数与线程，NInfer 模型直观呈现 KV 缓存量化类型与并发槽位数。

- **预设管理与编辑交互升级**
  - **格式分类标签页**：预设列表新增「全部 / GGUF / NINFER」多维筛选。
  - **双层字段布局**：中文名称与对应英文 CLI 参数纵向双层排布，彻底杜绝横向挤压与省略号截断；右上角新增快捷收藏与设为默认预设。

- **社区探索与模型导入**
  - **Hugging Face 格式筛选**：社区探索热门与搜索支持按模型格式（全部 / 仅 GGUF / 仅 NINFER）精确过滤并展示格式徽章。
  - **双格式统一导入**：模型导入窗口与拖拽区域全面兼容 .gguf 与 .ninfer 文件及文件夹。

- **控制台与实用工具**
  - **一键复制日志**：底部日志坞新增复制按钮，支持一键导出格式化时间戳与日志级别的纯文本内容到剪贴板。

### v0.3.1

- 新增 GGUF 文件头读取：模型架构、参数量、量化直接来自文件头，不再依赖文件名猜测
- 新增配置备份导出/导入：导入按路径合并、不覆盖本机已有模型，导出备份不含下载 Token
- 设置页「配置存储」改为「管理」下拉菜单（导出备份 / 导入备份 / 打开文件夹）
- 修复任务列表条目被压缩的问题

### v0.3.0

- 上线全新推理性能分析页 “日志-分析”
- 支持 Decode/Prefill 双轨监控与多维指标洞察
- 支持轮次锁定与多类型数据筛选
- 修复模型自定义标签与标签池重启后未持久化保存的问题

### v0.2.9

- 运行预设新增思考预算（--reasoning-budget）配置
- 视觉模型新增「挂载至内存」开关，不占显存更轻量
- 新增参数去重与防冲突处理

### v0.2.8

- 新增 CUDA 版本识别
- 客户端启动优化


## 📸 界面预览

### 🏠 主页・模型仓库

统一管理电脑上所有 GGUF 模型，直接拖拽文件就能导入，支持搜索、改名、排序以及批量操作。

![主界面 · 模型仓库](docs/light.png)

### 🎛 运行预设

给不同模型保存独立的启动参数，手动精细调参、复制一套新配置，也可以自定义额外命令行参数。

![运行预设 1](docs/profile1.png)

![运行预设 2](docs/profile2.png)

### 🌍 社区探索

嵌入Hugging Face 每周模型榜单，支持筛选、搜索、下载及自动化入库

![社区探索](docs/explore.png)

### 📟 实时日志

实时读取 `llama-server` 的输出数据、报错日志、全量轮次明细，既可在底部抽屉查看，也能切换到全屏日志页面。

![实时日志](docs/log1.png)

![日志分析](docs/log2.png)

### 💬 对话・内置网页对话界面

集成 llama.cpp 官方Webui，本地同时提供兼容 OpenAI 格式的 `/v1` 接口，可以对接任意第三方客户端。

![对话 · WebUI](docs/webui.png)

---

## ✨ 核心功能

### 🗂 模型仓库

- **快速导入**：拖拽文件或文件夹，也可以手动选择路径，程序自动扫描目录，只识别 GGUF 格式文件
- **自动识别信息**：从文件名读取模型架构、参数量、量化等级（例如 `Q4_K_M`）
- **灵活管理**：按名称、路径、架构筛选搜索，支持重命名、拖拽排序、快速删除，还能设置默认启动模型
- **批量处理**：多选批量删除，左侧面板会直观展示模型数量

### 🎛 运行预设配置

- **完整可调参数**：GPU 分层加载显存、上下文窗口大小、批处理参数、Flash Attention、KV 缓存方案、Jinja 提示词模板、推理思考强度、内存映射加载以及采样相关配置全部支持自定义
- **自由扩展**：新建、复制、删除配置模板，额外补充自定义命令行启动参数

### 🚀 一键启停与进程管理

- 使用 Rust 托管 `llama-server` 子进程，自动检测进程 PID 和端口状态，顶部按钮一键开启关闭
- 停止服务时完整杀掉整个进程树，彻底解决显存占用无法释放的问题

### 📈 显卡实时监控

- 调用 `nvidia-smi` 定时读取显存占用与显卡负载，每 2 秒刷新一次数据
- 侧边小卡片 + 顶部性能条展示状态，附带三分钟负载波动小曲线图
- **开关可控**：程序会自动识别显卡类型，如果是 AMD 或者不支持读取状态的显卡，会给出提示，你可以关闭监控避免无用轮询

### 📟 实时日志面板

- 实时捕获程序输出和报错，按照 **ERR / WRN / SYS / OUT** 不同级别用不同颜色区分展示
- 底部可拉伸收起的控制台，还有独立全屏日志页面，支持一键清空日志

### 💬 对话能力与 OpenAI 兼容接口

- 服务启动后内置官方对话页面，外部链接调用系统浏览器打开跳转
- 本机提供标准的 `/v1/chat/completions` 接口，可以对接各类第三方对话工具

### ⚙️ 完全本地运行，保护隐私

- **不上传任何数据**：模型列表、参数配置、对话记录全部保存在你的电脑本地
- **安全写入配置**：采用临时文件替换写入机制，避免异常退出导致配置文件损坏
- **明暗双主题**：一键切换浅色 / 深色模式，设置自动保存生效

---
### 🧭 llama.cpp 引擎管理

- **自动识别**：启动检测 NVIDIA、Vulkan 环境，自动选用 CUDA/Vulkan/CPU 最优加速方案并显示显卡
- **一键更新**：拉取 llama.cpp 官方 Windows 版本，自动匹配后端与 CUDA 版本
- **断点续传**：下载解压实时显示进度，支持随时取消
- **安全更新**：临时目录预解压 + 备份回滚，升级失败不破坏原有程序，无需额外配置
- **强制修复**：同版本文件损坏时可重新下载安装

### 🌐 网络与代理

- **网络模式**：跟随系统 / 手动 HTTP/SOCKS5 代理 / 直连
- **手动代理**：填入如 `http://127.0.0.1:7890` 等代理地址，通过 `reqwest::Proxy::all` 动态注入所有请求
- **自动检测系统代理**：Windows 下读取注册表 `HKCU` 代理设置，代理失效 / 网络异常时多通道设下载可自动切换直连或本地代理重试
- **连接测试**：一键测试与 GitHub API 的连通性，显示状态码与延迟

### 🌍 社区探索（HuggingFace）

- **热门榜单**：展示当周 HuggingFace Trending GGUF 模型，支持刷新与收起
- **全量搜索**：支持搜索模型 / 组织 / 关键词，快速筛选标签（Qwen / Llama / DeepSeek / GLM / Gemma / 代码 / 视觉），可仅显示 GGUF、按热度 / 时间排序；列表支持触底分页加载，无限向下浏览
- **通用文件下载弹窗**：单击卡片即弹出文件列表（量化 Badge、文件大小、显存适配提示、一键加入下载队列），提示存储目录与剩余空间可随时更改
- **托管下载**：支持断点续传（`.part` 临时文件）、实时速率 / 进度 / 剩余时间显示，异常任务可重试
- **自动入库**：下载完成后自动扫描解析 GGUF 并入库，在模型仓库第一位显示「刚刚下载」标识

## 🛠 技术架构

| 模块 | 技术方案 | 说明 |
| :--- | :--- | :--- |
| **桌面程序框架** | [Tauri 2](https://tauri.app/)（Rust） | 原生轻量内核 |
| **前端页面** | React 19 · TypeScript · Vite 6 | 现代化响应式单页界面 |
| **后端核心** | Rust Core | 负责进程管控、日志转发、文件扫描与安全保存配置 |
| **推理底层** | llama.cpp（`llama-server.exe`） | 本地高速运行 GGUF 大模型 |
| **打包安装程序** | NSIS / WiX | 提供 Windows 安装包 |

---

## 📁 项目目录说明

```text
├── index.html              # 启动闪屏页与页面入口
├── src/                    # 前端代码（React + TypeScript）
│   ├── App.tsx             # 全局状态管理与事件分发
│   ├── tauri.ts            # Tauri 接口与前后端事件通信
│   ├── data.ts             # 默认参数模板、版本迁移脚本、示例数据
│   ├── components/         # 模型卡片、参数编辑器、日志面板、对话界面等组件
│   └── types.ts            # 全局 TypeScript 类型声明
└── src-tauri/              # Rust 后端工程（Tauri 2）
    ├── src/lib.rs          # 进程控制、日志监听、文件扫描、配置读写逻辑
    ├── tauri.conf.json     # Tauri 基础配置文件
    └── capabilities/       # 安全权限、窗口访问策略定义
```

---

## 🚀 快速上手使用

1. 打开**设置页面**，填写你电脑里 `llama-server.exe` 的完整路径
2. 把 GGUF 模型文件拖拽到**模型仓库**
3. 选择一套合适的**运行预设**，点击**启动服务**
4. 直接切换到**对话**面板开始聊天；也可以通过本地地址 `http://127.0.0.1:9931/v1` 对接其他软件

---

## 💻 本地开发编译

**环境依赖**

- Windows 10 / 11（x64 系统）
- Node.js 18+ 以及带 MSVC 编译工具的 Rust 开发环境
- 提前编译好的 `llama-server.exe` 和 GGUF 模型文件

**开发调试**

```bash
# 安装前端依赖
npm install

# 启动桌面端热更新调试模式
npm run tauri dev
```

**打包正式版本**

```bash
# 编译生成安装包，输出目录 src-tauri/target/release/
npm run tauri build
```

---

## 🐧 Linux（x86_64，移植验证阶段）

本节补充 Linux 使用方式；上文 Windows 的 `.exe` 引擎、MSI/NSIS 安装与自动应用更新说明不适用于 Linux。

### 支持范围与当前证据

- Linux 打包目标为 **deb / rpm / AppImage**，由 `src-tauri/tauri.linux.conf.json` 自动合并到基础配置；当前保持三种格式，尚未完成跨发行版安装验证。
- 开发机 CachyOS x86_64（Rust 1.99、Node 26 / npm 12、GTK 3.24、WebKitGTK 4.1 2.54、OpenSSL 3.6）已完成三种包的本地构建。该结果不是最低 glibc / 发行版兼容承诺。前轮已有真实 CPU 桌面与独立 NVIDIA Vulkan/CUDA 微型模型推理证据，完整桌面/升级矩阵仍待验收。
- CI 配置采用 Ubuntu 22.04 x86_64 / Node 22；云端执行与 Ubuntu 24.04 等运行环境仍待验证。无显示器 CI 的编译和测试不替代 X11 / Wayland 实机检查。
- Linux 应用更新已按真实安装方式提供能力提示：**AppImage** 校验下载后同目录原子替换、保留旧备份，等待旧进程退出再重启，90 秒内未收到 React 启动确认则回退；**deb/rpm** 仅打开图形系统安装器，需用户确认，交接状态不代表升级完成。源码/未知安装布局、缺少所需工具或官方资产 digest 时显示手动安装原因，通过发布页下载对应包。
- 第四轮已通过**本地模拟发布元数据＋真实旧 GUI `0.3.2` → 新 GUI `0.3.3` 跨版本升级**：真实 check/download/install IPC、helper 启动与 attached WebKit 原生版本读取、ready/startup-confirmed、旧 GUI 退出无交接重叠、升级前后重复启动单实例、CPU 停止及配置/模型/旧备份完整性均有证据；修复前的真实 90 秒回退证据保留。修复只将 FUSE 挂载唯一性限定在当前 GUI 的 exact APPDIR，仍校验只读/UID/source/fstype、runtime 设备/inode/open FD，未以环境变量替代身份核验。**`0.3.3` 仅为 `.tmp` 隔离测试快照，主仓库及当前三包仍是 `0.3.2`；正式 GitHub 线上发布与 deb/rpm 系统安装尚未验证。**
- Linux x86_64 llama.cpp **CPU / Vulkan / CUDA 自动下载更新已实现并接通 UI**：从含所选后端资产的最高 `b` nightly 选择安装包，校验 SHA-256、大小与 URL，安装到独立托管目录，失败保留旧版及配置，不覆盖外部引擎。CUDA 支持 `auto / 12 / 13 / major.minor`；auto 按驱动上限优先兼容 12，再选 13，系统 `nvidia-smi` XML 探测限时 5 秒，驱动/配对资产缺失时有明确错误提示并保留手动入口。用户偏好与实际安装版本分开保存，CPU/Vulkan 安装不清 CUDA 偏好。
- CUDA 下载同 tag、完整版本和架构的 **engine + 官方 cudart runtime 双包**；b11534 CUDA 12.8 两包合计约 **766 MB**，runtime 单包约 **594 MB**、展开约 **869 MB**，安装还需暂存/旧版空间。runtime 下载专用上限 1 GiB，不放宽 engine/App 的 512 MiB 上限。仅三类运行库及受限许可证内容安装到 appdata 私有 `cuda-runtime`，只给托管 CUDA 子进程设置私有 LD 路径并清 `LD_PRELOAD`；不复制 `/opt/resolve`、不装系统库，不强加到 CPU/手动外部引擎。用户直接下载官方 runtime，不代表独立再分发许可审计已完成。
- 第三轮官方 12.8 双包通过校验、production 准备/合并/启动健康路径及真实 CUDA0 推理（`offloaded 6/6 layers`、health/chat 200、非空 16 tokens、端口释放）。**CUDA 13/13.4 只有 metadata/pair/selection 测试，未实包运行**；微型模型不作性能宣传。
- **NInfer 当前官方版**要求 64 位 Linux，锁定 **sm120a**、选定/验证 **RTX 5090**；CUDA 13.1 是已验证工具链，CMake 未设置 CUDA 版本下限；当前设计为单 GPU、单常驻模型、无 CPU offload。**不是所有 50 系通用保证，更不是所有社区 fork 都要求 50 系**；本机 RTX 4070 Laptop sm89 不满足官方版，fork 需独立验证，Linux NInfer 运行仍未验。[NInfer README](https://github.com/Neroued/ninfer/blob/HEAD/README.md "官方说明 64 位 Linux、RTX 5090、sm120a 构建限制及单 GPU 常驻模型") [NInfer CMake](https://github.com/Neroued/ninfer/blob/HEAD/CMakeLists.txt "官方构建锁定 120a，未设置 CUDA 工具链版本下限")
- 第三轮原生包的独立 Wayland seat **10/10** 真实交互通过，包含 iframe textarea 输入并点击 Send（非 fetch 替代）、中文多行 native 剪贴板/日志复制、预设选择与启停、task UI 大小写文件删除、窗口关闭清理。导入使用 Tauri drag 事件 fixture，非物理拖拽/原生文件选择器。真实本地下载 IPC **8/8** 覆盖暂停续传、流/停滞取消、Range 失败和目标冲突；未替代外网 HF/代理与下载按钮 UI。
- 第四轮桌面服务汇总 **9 项功能通过＋1 项渲染 partial**：真实 Thunar 定位 **4/4**（打开文件 parent/直接目录，不声称选中文件），GI/GTK 测试 consumer 下原生 SNI/DBusMenu **5/5**（Close 隐藏但 app/CPU 继续、show 恢复、重复启动单实例恢复、quit 清理）。图标截图可见，GTK 菜单标签截图被裁，物理鼠标菜单/真实发行版 panel 未验；测试 host 不是发行版 panel。这组使用第三轮 native 包，不标为最新 root 包重验。
- 当前主仓库 `0.3.2` 的 check 零 Rust warning、Rust 库 **125 passed / 0 failed / 3 ignored**、三包构建通过；路径 **18** / UI **61** 沿用第三轮，本轮未重新执行 Node tests，打包内 `tsc/Vite` 实际通过。完整 SHA、按轮日志及保留的失败/工具诊断见 [Linux 累计验收记录](docs/Linux验收记录.md)。

### 依赖与源码构建

需要 Node.js 22、Rust stable 工具链和 Linux 桌面运行环境。Ubuntu 22.04 启用 universe 软件源后，可安装以下构建依赖；WebKitGTK 4.1 的开发包名为 `libwebkit2gtk-4.1-dev`，并非 4.0 包。[Ubuntu 包目录](https://packages.ubuntu.com/jammy/libwebkit2gtk-4.1-dev "Ubuntu 22.04 提供 WebKitGTK 4.1 开发包及 GTK 3 依赖")

```bash
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential curl wget file pkg-config libssl-dev libgtk-3-dev \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
  patchelf rpm libfuse2

# 仓库根目录：依赖准备、桌面调试、正式打包
npm ci
npm run tauri dev
npm run build:linux

# 可选：仅打 deb；-- 后的参数原样传给本地 Tauri CLI
npm run build:linux -- --bundles deb
```

当前锁文件含 `registry.npmmirror.com` 下载地址。本机 npm 12 的安装验证使用 `npm ci --registry=https://registry.npmmirror.com`；按网络情况选择命令，该参数仅作用于本次安装，不修改全局 npm 配置，也不改锁文件中的来源。

`build:linux` 仅在 Linux 运行，默认给子进程设置 `NO_STRIP=1`（保留用户已设置的值）。这是针对 linuxdeploy 内置旧 `strip` 解析较新系统库 `.relr.dyn` 失败的兼容措施，**不改变本应用 Cargo release 的 `strip=true` 和编译优化**，也不影响 Windows 的 `npm run tauri build`。不要将该变量写入系统全局环境。

包输出位置：

- `src-tauri/target/release/bundle/deb/*.deb`
- `src-tauri/target/release/bundle/rpm/*.rpm`
- `src-tauri/target/release/bundle/appimage/*.AppImage`

### 安装、引擎与桌面排查

- deb：在对应 Ubuntu / Debian 桌面上通过系统软件安装器或 `sudo apt install ./实际文件名.deb` 安装，让包管理器处理依赖。
- rpm：在对应 RPM 系桌面上通过系统软件安装器或 `sudo dnf install ./实际文件名.rpm` 安装，发行版适配仍需实测。
- AppImage：对下载文件添加执行权限（`chmod +x ./实际文件名.AppImage`）后启动。目标系统仍需兼容的 glibc / 图形栈；若提示 FUSE 依赖缺失，按发行版安装 FUSE 2 兼容库（Ubuntu 22.04 为 `libfuse2`）。
- 在引擎管理/设置中手动选择 **Linux x86_64 的 `llama-server`**，保留配套 `.so` 与目录布局，确认可执行权限、CPU 指令集和 GPU 运行库匹配；Windows `.exe` 引擎应重新定位。导入 GGUF 后配置预设并验证启停、日志和聊天。
- 文件与模型路径区分大小写；配置和模型继续使用 Tauri 平台用户目录或用户自选目录。升级前备份配置，保留模型和外部引擎目录。
- Linux 模型下载支持异步暂停/续传/取消，临时文件用完整文件名追加 `.part`（`model.gguf.part`）及自有 `.part.cookllm.json`，以 ETag/Last-Modified 的 If-Range 校验来源；206 严格校验、200 忽略 Range 时重写，错误不完成/不覆盖旧文件。暂停保留缓存、取消清理后停止写入；删除暂停 partial 会清自有 sidecar，未知 JSON/符号链接保留。旧 `model.part` 缓存不自动迁移；Windows transfer 保留，命名变化仍待实机回归。
- NVIDIA 监控依赖驱动提供的 `nvidia-smi`，可先在终端运行该命令；没有 NVIDIA 的机器按界面状态使用其他后端。剪贴板/目录定位还依赖桌面会话和对应工具（如 Wayland 的 `wl-copy`、X11 的 `xclip` / `xsel`、`xdg-open`）。

若 NVIDIA / WebKitGTK 出现空白或渲染异常，可从仓库根目录尝试以下**仅当前进程有效**的诊断命令；正常渲染时无需设置：

```bash
GDK_BACKEND=x11 WEBKIT_DISABLE_DMABUF_RENDERER=1 ./src-tauri/target/release/cookllm
# Wayland 会话下的可选对照（需已有有效 Wayland 桌面会话）
GDK_BACKEND=wayland ./src-tauri/target/release/cookllm
# 本地 0.3.2 AppImage：局部禁用 DMABUF 渲染，不写入全局环境
WEBKIT_DISABLE_DMABUF_RENDERER=1 ./src-tauri/target/release/bundle/appimage/CookLLM_0.3.2_amd64.AppImage
```

本机曾在独立 XDG 目录和 `dbus-run-session` 下进行一次隔离启动冒烟：native Wayland 上报 React 首屏 1681 ms；native X11 配合 `WEBKIT_DISABLE_DMABUF_RENDERER=1` 为 423 ms，二者观察 10 秒未提前退出或 panic。**AppImage 默认 Wayland 出现 GBM 错误且未上报 React，首屏冒烟失败**；局部设置上述变量后上报 React 591 ms，观察 12 秒未提前退出或 panic。这是单次环境记录，不是启动性能保证，也不代表 AppImage 无条件运行通过。

这些启动参数不修改用户系统环境。Linux 首屏已采用先显示窗口再等待 rAF，解决隐藏窗口等待约 8 秒的首帧停顿；Windows 保留双帧等待，运行回归仍待执行。Linux 托盘默认关闭，托盘故障时窗口保持可见；前轮 X11 CPU 聊天/配置恢复、第三轮独立 Wayland 交互，以及第四轮 Thunar 定位/原生 tray 协议已有分项实测。GTK 菜单可视确认仅 partial，发行版 panel/物理鼠标菜单、picker/drag、有/无 NVIDIA 与完整桌面矩阵仍待验。维护者 CI 与发布边界见 [Linux 发布说明](docs/发布指引.md#七linux-构建与发布补充)。

---

## 👥 社区与支持

访问 [LINUX DO](https://linux.do)，与开发者和技术爱好者交流。

---

## 📄 开源许可

本项目遵循 [MIT License](LICENSE) 开源协议分发。
