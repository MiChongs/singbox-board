# singbox-board

为 [MiChongs/sing-box](https://github.com/MiChongs/sing-box)（xiaobaf14g 分支）实现的 root 守护进程与终端面板，使用 Rust + [ratatui](https://ratatui.rs) 编写。

- **root daemon**：以 root 运行并托管 `sing-box run` 子进程（TUN、`auto_route`、tproxy、eBPF 入站都需要 root），负责启停、崩溃后指数退避重启、配置校验与热重载，并从 GitHub Releases 安装或更新内核。
- **核心版本管理**：内置 MiChongs（xiaobaf14g）与 SagerNet 官方两个发布源，可添加任意 GitHub 源或导入自编译核心。可列出某个源的全部版本及本机可用的构建变体（ebpf、easytier、glibc、musl 等），一键下载、校验、切换或回退，多个版本并存，切换瞬间完成，启动失败会自动回滚。
- **配置管理**：多份 sing-box 配置并存，可从文件或订阅地址导入（订阅按间隔自动更新，并显示服务商返回的流量与到期时间），也可用模板新建。切换前先用 sing-box 校验，启动失败自动回滚；内置全屏编辑器：语法高亮、输入时校验并标出错误位置、查找替换、按行号或 JSON 路径跳转，sing-box 校验失败时直接跳到出错的字段，并可随时切换到带模板的树形视图。
- **可选组件**：[Sub-Store](https://github.com/sub-store-org/Sub-Store)（订阅管理，带 Web 界面）与 [http-meta](https://github.com/xream/http-meta)（按需启动 mihomo 供 Sub-Store 脚本检测节点）。首次运行时会询问是否需要，选择后由守护进程下载、校验、以非特权用户运行并托管。
- **容器**：接入 [kurumi-containerd](https://github.com/Tools-cx-app/kurumi-containerd)（轻量 Linux 系统容器运行时）。守护进程自动下载并校验运行时，可从 Debian、Ubuntu、Alpine、Arch 等发行版镜像一键创建容器，启停、进入终端、执行命令、编辑 TOML 配置（保存前由 kurumi-containerd 严格校验），实时显示每个容器的 CPU、内存与进程数；容器在守护进程重启后继续运行，开机可自动启动。
- **系统托盘**：`singbox-board tray` 在 KDE Plasma、GNOME（AppIndicator 扩展）、Waybar 等桌面托盘中显示 sing-box 状态，可启停 sing-box、切换配置与 Clash 模式，并可启停容器，Wayland 与 X11 下均可使用；在 Windows 上是原生的通知区域图标。
- **Windows**：支持 Windows 10/11（amd64、arm64）。守护进程作为 Windows 服务运行，通过命名管道与 TUI、命令行、托盘通信；内核、配置、Sub-Store 与 http-meta 的功能与 Linux 相同，容器除外（kurumi-containerd 只支持 Linux）。见 [Windows](#windows)。
- **TUI 面板**：通过 Unix socket 连接守护进程，通过 Clash API 连接 sing-box，可查看状态、流量、代理组、连接、日志，以及 Sub-Store 订阅和对应的 sing-box 订阅链接；在「配置」页管理和编辑配置，在「容器」页管理容器。
- **命令行**：`status / start / stop / restart / reload / check / logs / update / setup / component / core / profile / container`，便于脚本调用。

```
            ┌──────────────── singbox-board daemon (root) ─────────────────┐
 TUI / CLI ─┤ /run/singbox-board/daemon.sock  (NDJSON, SO_PEERCRED)          │
  (用户)     │   supervisor ── spawn ──► sing-box run                          │
            │   components ── spawn ──► node sub-store.bundle.js   (nobody)  │
            │                └ spawn ──► node http-meta.bundle.js  (nobody)  │
            │                              └► mihomo (按需)                   │
            │   containers ── exec ──► kurumi-containerd start|stop|install|run│
            │                └► 容器监控进程（systemd scope，machine.slice）  │
            │   log ring  ◄── 各子进程 stdout/stderr                          │
            │   updater   ──► GitHub Releases（sha256 校验）/ nodejs.org      │
            │             ──► 根文件系统镜像（images.linuxcontainers.org）    │
            └──────────────────────────────────────────────────────────────┘
 TUI ── HTTP ──► Clash API (experimental.clash_api)
 TUI ── HTTP ──► Sub-Store API (127.0.0.1:3001/<密钥路径>)
 sing-box providers ── HTTP ──► Sub-Store /download/<订阅>?target=sing-box
```

## 安装

支持 amd64、arm64、armv7、386、riscv64、loong64，均为静态链接的 musl 二进制，可在任意发行版上运行。服务管理支持 systemd 和 OpenRC。

### 一键安装

```bash
sudo sh -c "$(curl -fsSL https://raw.githubusercontent.com/MiChongs/singbox-board/main/install.sh)"
```

也可以用 `curl -fsSL …/install.sh | sudo sh`，但这种方式的标准输入是管道，脚本不会提问，Sub-Store / http-meta 的选择留到之后执行 `sudo singbox-board setup` 或打开面板时再做。之所以要这样处理：Ubuntu 25.10 起默认的 sudo-rs 会把命令放在新的伪终端里运行，用管道调用时键盘输入根本到不了脚本，脚本若提问就会卡住，连 Ctrl-C 也无效。

脚本会依次完成以下步骤：

1. 从最新 Release 下载对应架构的程序包，并用 `SHA256SUMS` 校验；
2. 安装到 `/usr/local/bin`，生成 `/etc/singbox-board/daemon.toml`（已有配置会保留）；
3. 创建 `singbox-board` 用户组，并把执行 `sudo` 的用户加入该组；
4. 安装并启动 systemd 或 OpenRC 服务；
5. 安装 sing-box 内核（MiChongs/sing-box）；
6. 询问是否启用 Sub-Store 和 http-meta。

重复执行即为升级。

访问 GitHub 较慢时可以使用下载镜像。镜像地址同时会写入 `daemon.toml`，之后 sing-box 和各组件的下载也会经过它：

```bash
sudo sh -c "$(curl -fsSL https://ghfast.top/https://raw.githubusercontent.com/MiChongs/singbox-board/main/install.sh)" \
  install.sh --mirror https://ghfast.top/
```

常用参数（完整列表见 `sh install.sh --help`）：

| 参数 | 说明 |
|---|---|
| `--version v0.1.0` | 安装指定版本 |
| `--sub-store yes\|no` `--http-meta yes\|no` | 直接回答首次询问，适合无人值守安装 |
| `--no-core` / `--no-start` | 不安装 sing-box 内核 / 只安装文件，不启动服务 |
| `--uninstall [--purge]` | 卸载；加 `--purge` 同时删除配置、组件数据和 sing-box 程序（保留 `/etc/sing-box` 与 `/var/lib/sing-box`） |

### 安装包

每个 Release 都附带 `.deb`、`.rpm`、`.apk` 和 Arch Linux 的 `.pkg.tar.zst`。程序安装到 `/usr/bin`，配置文件 `/etc/singbox-board/daemon.toml` 按配置文件处理，升级时不会覆盖。安装后会自动创建用户组，并启用、启动服务：

```bash
sudo apt install ./singbox-board_0.1.0-1_amd64.deb          # Debian / Ubuntu
sudo dnf install ./singbox-board-0.1.0-1.x86_64.rpm         # Fedora / RHEL / openSUSE (zypper)
sudo apk add --allow-untrusted ./singbox-board_0.1.0-r1_x86_64.apk   # Alpine (OpenRC)
sudo pacman -U ./singbox-board-0.1.0-1-x86_64.pkg.tar.zst   # Arch Linux
```

安装后执行 `sudo singbox-board update` 安装 sing-box 内核，再打开 `singbox-board` 完成首次询问。

### 手动安装 / 从源码构建

```bash
cargo build --release            # 产物：target/release/singbox-board（TLS 使用 rustls + ring，不依赖 OpenSSL）
```

从 Release 的程序包手动安装时，解压后执行 `sudo sh install.sh --local .`，效果与一键安装相同，只是不再联网下载程序本身。

### Windows

在**以管理员身份打开**的 PowerShell 中执行：

```powershell
irm https://raw.githubusercontent.com/MiChongs/singbox-board/main/install.ps1 | iex
```

需要传参数时（例如使用下载镜像，镜像地址同样会写入 `daemon.toml`）改用脚本块形式：

```powershell
& ([scriptblock]::Create((irm https://ghfast.top/https://raw.githubusercontent.com/MiChongs/singbox-board/main/install.ps1))) -Mirror https://ghfast.top/
```

脚本会依次完成以下步骤，重复执行即为升级：

1. 下载 `singbox-board-windows-<amd64|arm64>.zip` 并用 `SHA256SUMS` 校验，安装到 `C:\Program Files\singbox-board` 并加入 `PATH`；
2. 生成 `C:\ProgramData\singbox-board\daemon.toml`（已有配置会保留），把 `C:\ProgramData\singbox-board` 与 `C:\ProgramData\sing-box` 设为仅 SYSTEM 和管理员可写；
3. 创建本地用户组 `singbox-board`，把当前用户加入该组（**注销并重新登录后**，不用管理员权限也能使用 TUI、命令行和托盘），并允许该组启动服务；
4. 注册并启动 `singbox-board` 服务（开机自动启动，崩溃后自动重启）；
5. 在开始菜单添加「singbox-board」（托盘）和「singbox-board dashboard」（终端管理面板）；
6. 安装 sing-box 内核，并询问是否启用 Sub-Store 和 http-meta。

常用参数：`-Version v0.2.0`、`-Mirror <URL>`、`-SubStore yes|no`、`-HttpMeta yes|no`、`-NoCore`、`-NoStart`、`-Local <解压目录或 zip>`、`-User <要加入用户组的账户>`、`-Uninstall [-Purge]`。用 `irm | iex` 时也可以通过环境变量 `SBB_VERSION`、`SBB_MIRROR`、`SBB_SUB_STORE`、`SBB_HTTP_META` 传入。

| | Windows 上的位置 |
|---|---|
| 程序 | `C:\Program Files\singbox-board\singbox-board.exe`、`singbox-board-tray.exe` |
| 守护进程配置 | `C:\ProgramData\singbox-board\daemon.toml`（模板见 [`contrib/daemon.windows.toml`](contrib/daemon.windows.toml)） |
| 数据（内核、配置库、组件） | `C:\ProgramData\singbox-board\data` |
| 守护进程日志 | `C:\ProgramData\singbox-board\logs\daemon.log`（超过 8 MiB 时轮转为 `daemon.log.1`） |
| sing-box | `C:\ProgramData\sing-box\sing-box.exe`（指向当前内核的符号链接）、`config.json`，同时也是工作目录 |
| 控制通道 | 命名管道 `\\.\pipe\singbox-board` |

服务管理（管理员 PowerShell）：`Get-Service singbox-board`、`Restart-Service singbox-board`（修改 `daemon.toml` 后）、`Stop-Service singbox-board`。`sc control singbox-board paramchange` 相当于 Linux 上的 `systemctl reload`：校验后重启 sing-box（Windows 版 sing-box 不支持原地重载）。也可以不注册服务，在管理员终端中直接运行 `singbox-board daemon` 调试。

与 Linux 版的差异：

- 守护进程以 SYSTEM 身份运行，Sub-Store 与 http-meta 也以该身份运行（Windows 没有无需密码即可切换的低权限账户，`components.run_as` 不生效）。Node.js 与 mihomo 自动下载 Windows 构建。
- 停止 sing-box 时向其进程组发送 CTRL_BREAK（Go 程序会像收到 SIGINT 一样正常退出），超时后强制结束；所有子进程都放在 Job 对象中，服务意外退出时会被系统一并结束。
- 核心版本切换依赖符号链接，Windows 只允许管理员（或开启开发人员模式的用户）创建，服务本身满足这一条件。
- 容器功能不可用，TUI 不显示「容器」页。

### 手动安装（Windows）

```powershell
cargo build --release   # 产物：target\release\singbox-board.exe 与 singbox-board-tray.exe
```

把 Release 中的 zip 解压后，在管理员 PowerShell 中执行 `.\install.ps1 -Local .`，效果与一键安装相同。

### sing-box 配置

sing-box 配置默认读取 `/etc/sing-box/config.json`，工作目录为 `/var/lib/sing-box`。要使用面板的流量、代理组和连接功能，需要在配置中启用 Clash API：

```json
{
  "experimental": {
    "clash_api": { "external_controller": "127.0.0.1:9090", "secret": "change-me" }
  }
}
```

守护进程会自动从配置（包括 `-C` 目录中合并的文件，支持注释）中读取地址和 secret，并转交给已授权的客户端。

配置不必手写：可以在 TUI 的「配置」页或用 `singbox-board profile` 导入文件、订阅地址，或用内置模板新建，详见[配置管理](#配置管理)。

## 使用

```bash
singbox-board                 # 打开 TUI（默认）
singbox-board tray            # 系统托盘图标（也可从应用菜单启动）
singbox-board status          # 状态；--json 输出原始 JSON
singbox-board reload          # 先 sing-box check，再发送 SIGHUP
singbox-board logs -f -n 100  # 跟随日志
singbox-board update --check  # 只检查是否有新版本
singbox-board update --tag v1.14.1-xiaobaf14g.1 --force
singbox-board core                          # 当前核心与已安装版本
singbox-board core list --source SagerNet/sing-box
singbox-board core install v1.14.2 --source SagerNet/sing-box --variant glibc
singbox-board profile                       # 列出配置（别名 config）
singbox-board profile add https://example.com/sub --use   # 导入订阅配置并切换
singbox-board profile add ./config.json --name 家里       # 导入文件（- 表示标准输入）
singbox-board profile new 测试 --edit         # 用模板新建并在编辑器中打开
singbox-board profile use 家里                # 校验、切换并重启 sing-box
singbox-board profile edit 家里               # 在内置编辑器中编辑（--external 改用 $EDITOR），正在使用的配置保存后自动重载
singbox-board setup                         # 选择可选组件
singbox-board component sub-store           # 状态、Web 地址、订阅的 sing-box 链接
singbox-board component http-meta update    # start|stop|restart|enable|disable|update
singbox-board container                     # 列出容器（别名 ct）
sudo singbox-board container new 开发机 --image debian/trixie --start   # 新建、安装镜像并启动
singbox-board container images              # 本机架构可用的根文件系统镜像
sudo singbox-board container enter 开发机    # 在容器中打开终端
sudo singbox-board container exec 开发机 -- apt update   # 执行命令（不经过 shell）
singbox-board container stop 开发机          # start|stop|restart；show 查看详情
sudo singbox-board container edit 开发机     # 在内置编辑器中编辑 TOML 配置
```

`systemctl reload singbox-board` 与 `singbox-board reload` 等价。

### 界面语言

命令行、TUI 与守护进程日志均提供简体中文与英文两种语言，基于 Mozilla Project Fluent（通过 `i18n-embed` 加载）实现，文案位于 `i18n/<语言>/singbox-board.ftl`，编译时嵌入程序。

- 客户端（TUI 与命令行）依次读取 `--lang` 参数、环境变量 `SINGBOX_BOARD_LANG` 与系统区域设置（`LANGUAGE`、`LC_ALL`、`LC_MESSAGES`、`LANG`）确定语言，可选值为 `zh-CN` 与 `en`，其他区域设置使用英文。
- 守护进程按发起请求的客户端所用语言返回结果，同一守护进程可同时服务使用不同语言的用户。
- 守护进程自身的日志（journald 与 TUI 日志页）使用 `daemon.toml` 中的 `language` 设置，默认值 `auto` 表示跟随服务的区域设置。

```bash
singbox-board --lang zh-CN status
SINGBOX_BOARD_LANG=en singbox-board
```

如需新增语言，请将 `i18n/en/singbox-board.ftl` 复制到新的语言目录后翻译，并在 `src/i18n.rs` 的 `Lang` 中登记。`cargo test` 会核对各语言的消息 ID 与参数是否与英文一致，并拒绝间隔号、破折号等装饰性符号。

### TUI 按键

| 按键 | 功能 |
|---|---|
| `1`-`8` / `Tab` | 切换：概览 / 代理 / 连接 / 日志 / Sub-Store / Core / 配置 / 容器 |
| `s` `x` `r` | 启动 / 停止 / 重启 sing-box（停止和重启需要确认） |
| `R` | 校验配置并热重载 |
| `c` | 运行 `sing-box check` |
| `u` | 检查更新，确认后下载安装 |
| `m` | 切换 Clash 模式 |
| 代理页 `←→` `Enter` | 在组与节点间切换焦点 / 选择节点（Selector、URLTest、Smart） |
| 代理页 `t` / `T` | 测试整组 / 单个节点的延迟 |
| 连接页 `Enter` `/` `o` `O` `p` | 显示或隐藏详情 / 即时筛选（`Esc` 清除）/ 切换排序（最新、速度、流量、目标）/ 反转顺序 / 暂停刷新 |
| 连接页 `y` `d` `D` | 复制目标地址 / 关闭选中连接 / 关闭全部连接（筛选时为匹配的连接） |
| 日志页 `↑↓` `PgUp/PgDn` `End` | 滚动；按 `End` 恢复跟随 |
| Sub-Store 页 `←→` `Enter` | 在组件与订阅间切换焦点 / 组件操作菜单（启动、停止、更新、启用、禁用）或 provider 配置片段 |
| Sub-Store 页 `y` `w` `p` | 复制 sing-box 订阅链接 / 复制 Web 界面地址 / 显示 provider 配置片段 |
| Core 页 `←→` `Enter` `v` `i` `d` | 在来源、版本、已安装之间切换焦点 / 下载并切换 / 切换变体 / 仅下载 / 删除 |
| Core 页 `p` `n` `f` `a` `I` | 只看正式版 / 下一页 / 刷新 / 添加源 / 导入核心 |
| 配置页 `Enter` | 操作菜单：使用、编辑、更新、重命名、订阅地址与更新间隔、复制、校验、导出、删除 |
| 配置页 `e` `E` `n` `i` | 内置编辑器 / 用 `$EDITOR` 编辑（设置了 `$VISUAL` 或 `$EDITOR` 时）/ 用模板新建 / 导入文件或订阅地址（支持粘贴） |
| 配置页 `f` `F` `d` `A` | 更新所选订阅 / 更新全部订阅 / 删除 / 将当前配置文件收入配置库 |
| 容器页 `Enter` `n` | 操作菜单 / 新建容器（输入名称，选择网络，再从镜像列表选择根文件系统） |
| 容器页 `t` `o` `!` | 启动或停止 / 打开终端（非 root 时通过 sudo）/ 执行命令并查看输出 |
| 容器页 `e` `E` `i` | 在内置编辑器中编辑 TOML 配置 / 用 `$EDITOR` 编辑 / 安装或替换根文件系统 |
| 容器页 `a` `d` `y` | 开机自动启动开关 / 删除（可选是否同时删除文件）/ 复制根文件系统路径 |
| 容器页 `C` `U` `A` `f` | 检查主机能力 / 安装或更新 kurumi-containerd / 登记 root 已有的 kurumi-containerd 容器 / 刷新 |
| `M` | 关闭或重新开启鼠标 |
| `?` / `q` | 帮助 / 退出 |

鼠标可以完成上面的大部分操作：

- **按钮**：标签页、底栏和对话框中的按键提示、窗格边框上的提示都可以点击，效果与按下对应按键相同；确认框的「确认 / 取消」也是按钮。
- **列表**：单击选中一行并切换焦点到该窗格，双击打开（相当于 `Enter`，例如选择节点、切换内核），右键打开配置、容器和组件的操作菜单；菜单中单击即可执行。
- **滚动**：滚轮滚动鼠标下方的列表或日志，拖动滚动条快速跳转；日志页点击「已向上滚动」恢复跟随。
- **其他**：连接页点击表头按该列排序，再点一次反转顺序，点击右上角的暂停、筛选和排序标记可直接切换；Core 页点击边框上的构建变体即可选用；树形视图中点击 `▸` / `▾` 展开或折叠；在对话框外单击会关闭它（首次运行的询问除外）。
- **选择文字**：开启鼠标后，多数终端按住 `Shift` 拖动仍可使用终端自带的选择；也可以按 `M` 暂时把鼠标交还给终端。

### 系统托盘

`singbox-board tray` 在桌面的系统托盘中显示 sing-box 状态并提供常用操作。托盘实现 freedesktop StatusNotifierItem 协议，通过 D-Bus 与桌面通信（使用纯 Rust 的 [ksni](https://github.com/iovxw/ksni) 与 [zbus](https://github.com/dbus2/zbus)，不依赖 GTK 或 libdbus，静态 musl 程序同样可用），与显示服务器无关，Wayland 与 X11 下均可使用：

| 桌面 | 支持情况 |
|---|---|
| KDE Plasma 5 / 6 | 原生支持 |
| GNOME | 需要启用 AppIndicator 扩展（Ubuntu 默认已启用） |
| Cinnamon、XFCE、LXQt | 面板托盘原生支持 |
| Sway、Hyprland、niri 等 | 使用带托盘模块的状态栏，例如 Waybar |

- **图标**：绿色表示运行中，橙色表示正在启动、停止或有操作在进行，灰色表示已停止，红色表示运行失败或无法连接守护进程。悬停提示显示当前配置、内核版本、Clash 模式与失败原因。
- **菜单**：启动、停止、重启 sing-box；切换配置、更新全部订阅配置；切换 Clash 模式（需启用 Clash API）；在「容器」子菜单中勾选即可启动或停止容器；打开终端管理面板（左键单击图标效果相同）；打开 Sub-Store Web 界面；登录时启动；退出。
- **通知**：操作结果、失败原因以及 sing-box 意外退出会以桌面通知提示。
- **终端**：打开管理面板时依次尝试 `xdg-terminal-exec`、`$TERMINAL`、当前桌面自带的终端（Konsole、Ptyxis、GNOME 控制台等）及其他常见终端。
- **登录启动**：勾选「登录时启动」会写入 `~/.config/autostart/singbox-board.desktop`。安装包与一键安装脚本还会在应用菜单中添加「singbox-board」入口。

托盘以普通用户身份运行，与 TUI 一样需要是套接字用户组成员；同一桌面会话中只运行一个托盘。登录时若面板尚未就绪，托盘会等待其出现后再显示图标。

**Windows**：托盘是通知区域（Shell_NotifyIcon）中的原生图标，从开始菜单的「singbox-board」启动（即 `singbox-board-tray.exe`，它在后台启动 `singbox-board tray`，不会弹出控制台窗口）。

- 左键单击打开终端管理面板（在新的控制台窗口中，Windows 11 上为默认终端应用），右键单击或在图标上按菜单键弹出菜单；菜单内容与 Linux 相同，单选与勾选标记为系统原生样式，并跟随系统的深色/浅色模式。
- 守护进程无法连接时，菜单中提供「启动 singbox-board 服务」：安装脚本已允许 `singbox-board` 组启动服务，否则会弹出 UAC 提示。
- 通知以通知区域气泡显示（Windows 10/11 上即 Toast 通知），遵循专注助手/免打扰；点击通知打开管理面板。
- 「登录时启动」写入 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，并尊重「设置 → 应用 → 启动」中的开关。
- 资源管理器重启或登录时任务栏尚未就绪，图标都会在任务栏出现后自动恢复；图标按系统 DPI 绘制，高分屏下保持清晰。

## 核心版本管理

所有安装过的核心都保存在版本仓库 `/var/lib/singbox-board/cores/<来源>/<版本>/<变体>/` 中，包括发布包附带的全部文件（例如官方构建的 `libcronet.so`）。`/usr/local/bin/sing-box` 是指向当前核心的软链接，因此切换只是一次原子替换；sing-box 按真实路径查找附带库，也能正常找到。

| 操作 | TUI（第 6 页 Core） | 命令行 |
|---|---|---|
| 查看来源 | 左栏 Sources | `singbox-board core sources` |
| 列出全部版本 | 中栏 Releases，`n` 加载下一页，`p` 只看正式版，`f` 刷新 | `singbox-board core list [--source SagerNet/sing-box] [--page 2] [--stable]` |
| 选择变体 | `v` / `V` 切换（底部显示可选变体） | `--variant ebpf` |
| 下载并切换 | `Enter` | `singbox-board core install v1.14.1-xiaobaf14g.1 [--source …]` |
| 仅下载 | `i` | `core install … --no-switch` |
| 切换到已存版本 | 下栏 Installed，`Enter` | `singbox-board core use 1.14.2` |
| 删除已存版本 | 下栏 `d` | `singbox-board core remove <id>` |
| 添加自定义源（root） | `a` | `sudo singbox-board core source add owner/repo` |
| 导入自编译核心（root） | `I` | `sudo singbox-board core import /path/sing-box [--sha256 …]` 或 URL |
| 升级当前核心 | `u` | `singbox-board update` |

- **识别构建**：发布资源按 `sing-box-<版本>-linux-<架构>[-<变体>]` 识别，兼容 `armv7` / `arm-v7` 等不同写法，支持 `.tar.gz`、`.zip`、`.gz` 和裸二进制。
- **校验**：依次使用发布中的 `SHA256SUMS`、GitHub 提供的资源摘要、导入时指定的 sha256 校验。都没有时（2025 年以前的旧版本）仅依赖 TLS，并在界面上标记为 unverified。下载后会实际运行一次 `sing-box version`，确认能在本机执行。
- **切换前**：先用新核心对当前配置执行 `sing-box check`，配置不兼容就拒绝切换。例如官方核心不认识 MiChongs 特有的 `providers` 字段，这时可以用 `--force` 强制切换。原来手动放置的二进制会先作为「Previously installed」收进仓库，不会丢失。
- **切换后**：如果新核心启动失败（有些问题只在运行时才出现，例如新版本把弃用提示改成了致命错误），会自动回滚到之前的核心并重新启动。
- **`update` 的规则**：沿用当前核心的来源和变体，`daemon.toml` 的 `[update]` 只在还没有托管核心时生效。
- **权限**：添加自定义源和导入二进制只允许 root。核心以 root 身份运行，这两个操作等同于决定以 root 执行什么程序；`singbox-board` 组的成员只能在内置源和 root 添加的源之间安装、切换。

## 配置管理

所有配置都保存在配置库 `/var/lib/singbox-board/profiles/<id>.json` 中（目录仅 root 可读写，文件权限 `0600`），`index.json` 记录名称、来源和更新时间。`core.config` 的第一个文件（默认 `/etc/sing-box/config.json`）是指向当前配置的软链接，sing-box 始终读取同一路径，切换只是一次原子替换。`-C` 目录（`core.config_dir`）中的文件仍会在当前配置之上合并，适合放各配置共用的覆盖项。

| 操作 | TUI（第 7 页 配置） | 命令行 |
|---|---|---|
| 列出配置 | 上栏列表，下栏显示所选配置的概要（入站、出站、代理组、DNS、路由、Clash API）与详情 | `singbox-board profile` |
| 导入文件 | `i`，输入或粘贴路径 | `singbox-board profile add ./config.json` |
| 导入订阅 | `i`，输入或粘贴 http(s) 地址 | `singbox-board profile add <url> [--interval 分钟]` |
| 用模板新建 | `n` | `singbox-board profile new <名称> [--edit]` |
| 切换 | `Enter` → 使用此配置 | `singbox-board profile use <名称或 ID>` |
| 编辑 | `e`（内置编辑器，`F2` 切换树形视图） | `singbox-board profile edit <名称>`（同一个编辑器） |
| 用外部编辑器编辑 | `E`（需设置 `$VISUAL` 或 `$EDITOR`） | `singbox-board profile edit <名称> --external` |
| 更新订阅 | `f`（全部为 `F`） | `singbox-board profile update [<名称>]` |
| 改名、订阅地址、更新间隔 | `Enter` 菜单 | `singbox-board profile set <名称> --name … --url … --interval …`，`--local` 转为本地配置 |
| 校验 | `Enter` → 用 sing-box 校验 | `singbox-board profile check <名称>` |
| 导出 | `Enter` → 导出到文件 | `singbox-board profile show <名称> > file.json` |
| 删除 | `d` | `singbox-board profile remove <名称>` |
| 收入原有配置 | `A` | `singbox-board profile adopt` |

- **切换**：先用当前内核对新配置执行 `sing-box check`，不通过就不切换（`--force` 可强制）。切换后重启 sing-box；若新配置启动失败（例如端口被占用这类 check 发现不了的问题），会自动切回之前的配置并重新启动。全新安装时 sing-box 因缺少配置而未启动，切换到第一个配置后会自动启动。
- **原有配置**：`/etc/sing-box/config.json` 若是手写的普通文件，第一次切换前会先作为「原有配置」收入配置库，不会丢失；也可以随时用 `A` / `profile adopt` 收入，内容不变，sing-box 无需重启。
- **订阅配置**：从提供 sing-box 完整配置的地址下载，默认 User-Agent 为 `sing-box/<内核版本>`（`profiles.user_agent` 可改），服务商据此返回 sing-box 格式；如果返回的是节点列表，会提示改用 sing-box 格式或用 Sub-Store 转换。会读取 `subscription-userinfo`（流量与到期时间）、`profile-update-interval`（建议的更新间隔）和 `content-disposition`（默认名称）。之后按间隔自动更新（默认 24 小时，0 表示仅手动），正在使用的订阅配置只有在新内容通过校验后才会替换并重载，否则保留原配置并在列表中显示错误。日志与错误信息只显示订阅地址的主机名，不会泄露其中的令牌。
- **保存**：正在使用的配置保存前会先校验，通过后写入并重载 sing-box；未使用的配置直接保存，并附带校验结果供参考。

### 内置编辑器

在配置页按 `e`（或运行 `singbox-board profile edit <名称>`）打开全屏文本编辑器，不依赖 nano、vim 等外部程序。编辑器打开时所有按键都交给编辑器，数字键和 `Tab` 也会作为文字输入，不会误操作 sing-box。

- **校验**：每次输入后都按 sing-box 的规则（允许注释和尾随逗号）检查 JSON，在行号旁标出出错的行，底部写明原因（例如“此值后面缺少逗号”），`F8` 跳到出错位置；重复的键会给出警告。
- **定位**：边框底部显示光标所在的路径（如 `outbounds › 3 › server`），`Ctrl+G` 可输入行号、`行:列` 或路径（如 `outbounds[0].server`）跳转。
- **保存**：`Ctrl+S` 先在本地检查语法，再交给守护进程用 sing-box 校验。校验失败时编辑器会根据 sing-box 报告的路径（如 `route.rules[1].outbound`）定位并标出对应字段。
- **注释**：文件中的注释和格式原样保留；只有格式化文档或在树形视图中修改时才会按标准 JSON 重新排版，这两种操作都可以用 `Ctrl+Z` 撤销。
- **实现**：JSON（含注释）的解析、校验、高亮和定位使用 [jsonc-parser](https://crates.io/crates/jsonc-parser)，注释（包括 sing-box 接受的 `#` 注释）由 [json_comments](https://crates.io/crates/json_comments) 处理；文本、光标、选区和撤销历史由 [ratatui-textarea](https://crates.io/crates/ratatui-textarea) 管理，查找使用 [regex](https://crates.io/crates/regex)。
- **撤销**：`Ctrl+Z` 按编辑步骤撤销：输入的文字逐字撤销，格式化、树形视图中的修改、移动行、粘贴、全部替换等命令一次撤销。
- **超长行**：编辑内核只能直接跳到第 65535 列以内，单行超过这个长度时（压缩成一行的订阅配置），跳转和点击只能到达该行开头附近；先按 `Alt+F` 格式化即可。
- **剪贴板与鼠标**：复制会同时通过终端（OSC 52）和 `wl-copy` / `xclip` / `xsel` 写入系统剪贴板；终端自带的粘贴（通常是 `Ctrl+Shift+V`）直接插入文字。编辑器支持点击定位、拖动选择、双击选词和滚轮；按住 `Shift` 拖动仍可使用终端自带的选择。

| 按键 | 功能 |
|---|---|
| `Ctrl+S` / `Esc` `Ctrl+Q` | 校验并保存 / 关闭（有未保存修改时询问：保存并关闭、放弃修改或继续编辑） |
| `F2` / `F10` `Ctrl+P` / `F1` | 树形视图 / 全部命令菜单 / 快捷键帮助 |
| `Ctrl+F` / `Ctrl+R` / `F3` `Shift+F3` | 查找（`Alt+C` 区分大小写）/ 替换（`Alt+A` 全部替换）/ 下一个、上一个 |
| `Ctrl+G` / `F8` / `Alt+F` | 跳转到行或路径 / 跳到问题所在 / 格式化文档 |
| `Ctrl+Z` `Ctrl+Y` | 撤销 / 重做 |
| `Ctrl+C` `Ctrl+X` `Ctrl+V` `Ctrl+A` | 复制 / 剪切 / 粘贴（没有选区时作用于整行）/ 全选 |
| `Ctrl+D` `Ctrl+K` `Alt+↑↓` | 复制行 / 删除行 / 移动行 |
| `Tab` `Shift+Tab` `Ctrl+/` | 缩进 / 减少缩进 / 用 `//` 注释或取消注释 |
| `Shift+方向键` `Ctrl+←→` `Ctrl+Home/End` | 选择 / 按词移动 / 文档开头或结尾 |

括号和引号会自动补全，回车会保持缩进，在 `{}` 或 `[]` 之间回车会自动展开。

### 树形视图

在编辑器中按 `F2` 切换到树形视图：左侧为 JSON 树并定位到光标所在的节点，右侧显示所选项的完整内容。在这里的修改会在返回文本时合并进编辑器，作为一步可撤销的操作。树形视图中 `Tab` 和数字键仍可切换标签页，编辑状态会保留。

| 按键 | 功能 |
|---|---|
| `↑↓` `←→` `Space` `*` `-` | 移动 / 折叠或展开 / 切换 / 全部展开 / 全部折叠 |
| `Enter` / `e` | 编辑值：开关直接切换；`outbound`、`detour`、`final`、DNS `server`、规则中的 `rule_set` 等引用字段从现有标签中选择；对象和列表回到文本中编辑 |
| `:` / `E` | 回到文本编辑器并选中所选项 |
| `a` / `A` | 在之后添加（所选为展开的容器时添加到其中）/ 在所选容器内添加：在 `inbounds`、`outbounds`、`endpoints`、`route.rules`、`route.rule_set`、`dns.servers`、`dns.rules`、`providers` 中提供常用模板（mixed、tun、selector、urltest、VLESS REALITY、Hysteria2、规则集、DoH 等，均已用 sing-box 校验），`providers` 中还会列出 Sub-Store 的订阅；向分组成员列表添加时列出现有出站 |
| `r` `d` `c` `K` `J` | 重命名键 / 删除 / 复制一份（自动避开重复的 tag）/ 上移 / 下移 |
| `u` `U` `/` `n` `N` `y` | 撤销 / 重做 / 搜索 / 下一个 / 上一个 / 复制为 JSON |
| `s` / `q` `F2` | 保存 / 返回文本编辑器 |

树形视图中的修改会让全文按标准 JSON 重新排版（保留键的顺序和原有缩进宽度），注释会被移除；返回文本后按 `Ctrl+Z` 即可恢复。

## Sub-Store 与 http-meta

### 首次询问

守护进程首次启动时不会安装任何可选组件，只把“尚未选择”记录在 `/var/lib/singbox-board/state.json` 中。之后：

- 首次打开 TUI 会弹出向导，依次询问 **是否启用 Sub-Store**、**是否启用 http-meta**。选 `y` 立即下载并启动；按 `Esc` 跳过，下次打开时再问。
- 命令行可以用 `singbox-board setup` 交互回答，也可以直接带参数：`singbox-board setup --sub-store yes --http-meta no`。
- 之后随时可以调整：`singbox-board component <sub-store|http-meta> enable|disable`。

### 安装内容与运行方式

| 组件 | 来源 | 校验方式 | 运行方式 |
|---|---|---|---|
| Sub-Store 后端 | `sub-store-org/Sub-Store` 的 `sub-store.bundle.js` | GitHub 资源 sha256 摘要 | `node sub-store.bundle.js` |
| Sub-Store 前端 | `sub-store-org/Sub-Store-Front-End` 的 `dist.zip` | GitHub 资源 sha256 摘要 | 由后端托管（合并模式，单端口） |
| http-meta | `xream/http-meta` 的 `http-meta.bundle.js`、`tpl.yaml` | GitHub 资源 sha256 摘要 | `node http-meta.bundle.js` |
| mihomo | `MetaCubeX/mihomo`，按架构选择 `.gz` | GitHub 资源 sha256 摘要 | 由 http-meta 按需启动 |
| Node.js | 系统 `node`（v22 及以上）；没有时下载官方 LTS | `SHASUMS256.txt` | 无 |

- 文件位于 `/var/lib/singbox-board/{sub-store,http-meta,runtime}`。组件以 `components.run_as` 指定的用户运行（默认 `nobody`），只对各自的数据目录有写权限。
- Sub-Store 采用合并模式：前端和后端共用 `127.0.0.1:3001`，后端 API 位于首次启用时生成的随机路径下（相当于访问密钥，因为 Sub-Store 本身没有鉴权），不带该路径访问会返回 404。Web 界面地址形如 `http://127.0.0.1:3001/?api=http://127.0.0.1:3001/<密钥>`，可在 `singbox-board component sub-store` 或 TUI 中查看。
- 守护进程会先启动已启用的组件，等它们的端口可连接后再自动启动 sing-box，这样指向 Sub-Store 的 provider 在首次启动时就能拉取成功。
- 组件崩溃后按 `[restart]` 策略自动重启；停止 http-meta 时，它以脱离父进程方式启动的 mihomo 也会一并清理。
- 组件日志统一汇入日志面板，分别标记为 `sub-store`、`http-meta`。

### 与 sing-box 打通

Sub-Store 可以把任意订阅或组合订阅转换为 sing-box 格式，地址为 `<API>/download/<名称>?target=sing-box`（组合订阅为 `/download/collection/<名称>`）。在 TUI 的 **5 Sub-Store** 页选中订阅：

- `y`：复制该订阅的 sing-box 链接（通过 OSC 52 写入终端剪贴板）
- `p` / `Enter`：显示可直接粘贴的 provider 配置片段，再按 `y` 复制
- `w`：复制 Web 界面地址

`singbox-board component sub-store` 也会列出全部链接和配置片段，例如：

```json
{
  "providers": [
    { "type": "remote", "tag": "my-sub", "url": "http://127.0.0.1:3001/<密钥>/download/my-sub?target=sing-box", "update_interval": "1h" }
  ],
  "outbounds": [
    { "type": "urltest", "tag": "auto", "providers": ["my-sub"] }
  ]
}
```

### 与 TUN 共存

sing-box 开启 TUN + `auto_route` 时，http-meta 启动的 mihomo 发出的检测流量也会被 TUN 接管。如果希望节点检测直连，可以在 TUN 入站中排除组件用户（默认 `nobody`，uid 65534）：`"exclude_uid": [65534]`；也可以添加路由规则 `{"process_path": ["/var/lib/singbox-board/http-meta/meta/http-meta"], "outbound": "direct"}`。

## 容器（kurumi-containerd）

> 仅 Linux：Windows 上 `container` 命令会直接报告不支持，TUI 也不显示「容器」页。

[kurumi-containerd](https://github.com/Tools-cx-app/kurumi-containerd) 是用 Rust 编写的特权 Linux 系统容器运行时：mount、PID、UTS、IPC 与可选的网络命名空间，cgroup v1/v2 资源限制，绑定挂载与易失 OverlayFS，以及按 init 类型（systemd、OpenRC、runit 等）正确关机。singbox-board 把它完整接入守护进程、命令行、TUI 与托盘：

- **运行时**：首次需要时，守护进程从 `Tools-cx-app/kurumi-containerd` 的 Release 下载适合本机的构建（x86_64、aarch64 为静态 musl 构建，armv7、riscv64 为 glibc 构建），用 `SHA256SUMS` 与 GitHub 摘要校验，确认 `--version` 可运行后原子替换。也可以用 `container runtime update` 主动安装或更新，用 `container runtime import` 导入自编译构建，或在 `daemon.toml` 中用 `containers.runtime` 指定系统已安装的程序。更新运行时不影响正在运行的容器。
- **独立进程**：kurumi-containerd 会 fork 监控进程，并要求调用方是单线程进程，因此守护进程不链接它的库，而是始终调用官方程序（也不受其 GPL-3.0 许可证影响）。每个操作都会把输出与错误原因转交给客户端，诊断信息汇入日志面板（标记为 `kurumi`）。
- **实时状态**：容器的运行状态、CPU 负载、常驻内存与进程数由守护进程直接从 kurumi-containerd 的状态文件和 procfs 读取，并像运行时本身一样校验进程身份（启动 ID、init 与监控进程的启动时间、PID 命名空间以及容器内的身份标记），不会被复用的 PID 误导。

### 快速开始

```bash
sudo singbox-board container new 开发机 --image debian/trixie --start
sudo singbox-board container enter 开发机
```

这会用模板生成配置，从镜像服务器下载 `rootfs.tar.xz` 并按其 `SHA256SUMS` 校验，交给 kurumi-containerd 安装，然后在后台启动。在 TUI 的「容器」页按 `n` 可完成同样的流程：输入名称，选择网络，再从镜像列表中选择发行版。

### 目录与配置

所有数据位于 `<components.data_dir>/containers/`（默认 `/var/lib/singbox-board/containers/`，仅 root 可访问）：

| 路径 | 内容 |
|---|---|
| `registry.json` | 登记的容器：名称、配置文件位置、是否开机启动 |
| `<ID>/container.toml`、`<ID>/rootfs/` | 由面板新建的容器的配置与根文件系统 |
| `.kurumi-containerd/config.json` | 根据登记表生成的 kurumi-containerd 配置索引，以容器 ID 为条目名 |
| `runtime/kurumi-containerd`、`runtime/meta.json` | 下载的运行时及其版本与校验信息 |

容器配置就是 kurumi-containerd 的 TOML（[配置说明](https://github.com/Tools-cx-app/kurumi-containerd/blob/master/docs/configuration.md)）。模板带有注释，并预先写入 `uuid`，避免运行时首次使用时重写文件而丢失注释。已有的配置可以原地登记（`container add <文件> --link`，或用 `container adopt` 一次登记 root 的 `~/.kurumi-containerd/config.json` 中的全部容器），也可以保存一份副本（`container add <文件>`）。

内置编辑器（`container edit` 或容器页的 `e`）提供 TOML 语法高亮、输入时校验（语法以及每份配置必需的项目）、撤销、注释切换与鼠标操作。根文件系统安装后，每次保存前守护进程都会让 kurumi-containerd 按其严格的模式校验一份副本，不接受时显示运行时给出的原因并跳到出错的行，可以选择继续编辑或强制保存。容器运行时修改配置，重启后生效。

### 根文件系统

`container install <容器> <来源>` 接受三种来源：

- **镜像**：`debian/trixie`、`alpine/3.22` 之类，来自 `containers.image_server`（默认 images.linuxcontainers.org，国内可改用 `https://mirrors.tuna.tsinghua.edu.cn/lxc-images`）。`container images` 列出本机架构可用的镜像，下载后按镜像目录中的 `SHA256SUMS` 校验。
- **网址**：http(s) 地址，可用 `--sha256` 指定校验值；未指定时会在日志中提示未经校验。
- **本地压缩包**：tar、tar.gz、tar.xz、tar.zst 或 ZIP，由客户端解析为绝对路径。

下载过程中容器列表显示进度。使用 ext4 镜像（`rootfs_image`）的容器需要用 `--size 8G` 指定大小；已有根文件系统时需要 `--force` 才会替换。kurumi-containerd 只把根文件系统安装到 root 所有、仅 root 可写的目录中，且上级目录不能被其他用户写入（带粘滞位的目录除外）；不满足时守护进程会在下载前说明是哪一个目录。

### 网络与 sing-box

- `host`（模板默认）：容器与主机共用网络，sing-box 的 TUN 与 `auto_route` 同样接管容器的流量。
- `nat`：容器在网桥 `kurumi-br0` 后获得独立地址，模板会为每个新容器分配一个未被占用的地址（`172.28.0.2`、`172.28.0.3` 等），可用 `network_options.ports` 发布端口。转发流量在 TUN 开启时也会进入 sing-box。
- `none`：只有回环接口。
- `gateway` / `dhcp`：接入已有网桥，在配置中填写 `network_options.gateway_bridge`。

发行版镜像通常默认在 `eth0` 上运行 DHCP 客户端，使用 `host` 网络前请确认容器内的网络服务不会改动主机网卡，必要时改用 `nat` 或 `none`。

### 生命周期

- 在 systemd 系统上，每次启动都放在 `machine.slice` 中独立的临时 scope（`singbox-board-container-<ID>-*.scope`）里，关机时排在守护进程服务之后停止。因此停止、重启或升级 singbox-board 都不会中断容器，而系统关机时守护进程会先按各自的 init 类型正常关闭容器。OpenRC 下监控进程本身就与服务脱离，效果相同。
- `containers.stop_on_shutdown = true` 时，守护进程停止的同时也停止全部容器。
- 设置为开机启动的容器（`container autostart <容器> on` 或容器页的 `a`）在每次开机后守护进程首次启动时启动一次；同一次开机内重启守护进程不会再次启动被手动停止的容器。
- 以前台模式配置的容器（`container.foreground = true`）只能直接用 kurumi-containerd 运行，守护进程会拒绝启动并说明原因。
- kurumi-containerd 0.2.3 会把 Alpine 等使用 busybox init 且装有 OpenRC 的系统识别为 OpenRC 并发送 `SIGPWR`，而 busybox init 不处理该信号，所以停止这类容器要等满 `runtime.stop_timeout_seconds`（默认 15 秒）后才会强制结束。

### 权限

启动、停止与查看容器对所有可连接守护进程的用户开放，与启停 sing-box 一致。新建、导入、编辑、删除容器，安装根文件系统，在容器中执行命令，以及导入运行时都决定了以 root 身份运行的内容，只允许 root（例如 `sudo singbox-board`）。以普通用户打开的 TUI 会在容器页提示这一点，打开终端时会通过 `sudo singbox-board container enter` 切换到 root。

## 配置

完整的带注释配置见 [`contrib/daemon.toml`](contrib/daemon.toml)（Windows：[`contrib/daemon.windows.toml`](contrib/daemon.windows.toml)），也可以用 `singbox-board daemon --print-default-config` 输出本平台的版本。常用项：

| 键 | 说明 |
|---|---|
| `language` | 守护进程日志的语言：`auto`（跟随区域设置）、`zh-CN` 或 `en`；对客户端请求的回复使用客户端自身的语言 |
| `socket_group` | 允许使用控制 socket 的用户组。组存在时 socket 为 `0660 root:<组>`，否则为 `0600` |
| `core.binary` / `core.config` / `core.config_dir` / `core.working_dir` | 对应 sing-box 的二进制路径以及 `-c` / `-C` / `-D` 参数 |
| `core.check_before_start` | 每次启动、重启、重载前先运行 `sing-box check`；校验失败时保留正在运行的实例 |
| `restart.policy` | `always` / `on-failure` / `never`，重启间隔按指数退避，上限为 `max_backoff_secs` |
| `update.repo` / `update.variant` | 尚未有托管核心时的默认来源和变体；之后 `update` 沿用当前核心的设置 |
| `core.env` | 传给 sing-box 的环境变量，例如 `ENABLE_DEPRECATED_IMPLICIT_DEFAULT_HTTP_CLIENT = "true"` |
| `update.proxy` / `update.mirror` | 访问 GitHub 时使用的代理 / 下载镜像前缀（组件下载同样使用） |
| `profiles.user_agent` / `profiles.proxy` | 下载订阅配置时使用的 User-Agent（默认 `sing-box/<内核版本>`）/ 代理 |
| `components.data_dir` / `components.run_as` | 组件安装目录（含 `state.json`、`profiles/`、`cores/`）/ 组件运行用户 |
| `components.node` / `components.node_mirror` | 指定 Node.js 路径 / Node.js 下载源（国内可用 `https://npmmirror.com/mirrors/node`） |
| `sub_store.host` / `sub_store.port` | Sub-Store 监听地址，默认 `127.0.0.1:3001` |
| `sub_store.sync_cron` / `produce_cron` / `default_proxy` / `env` | 对应 `SUB_STORE_*` 环境变量 |
| `sub_store.env.SUB_STORE_CORS_ALLOWED_ORIGINS` | Sub-Store 允许的跨域来源。默认在上游列表之外自动加入前端自身的地址（按 `host`/`port` 推出）；通过域名或反向代理访问 Web 界面时需在此补上对应来源，否则保存会返回 `403 CORS origin not allowed` |
| `http_meta.host` / `http_meta.port` / `authorization` | http-meta 监听地址（默认 `127.0.0.1:9876`）与访问凭据 |
| `http_meta.mihomo_arch` | 指定 mihomo 构建，例如 `amd64-v3` |
| `containers.runtime` / `containers.repo` | 指定 kurumi-containerd 程序路径 / 下载运行时的 GitHub 仓库（默认 `Tools-cx-app/kurumi-containerd`） |
| `containers.image_server` | 根文件系统镜像服务器，默认 `https://images.linuxcontainers.org` |
| `containers.autostart` / `containers.stop_on_shutdown` | 是否在开机后启动标记为开机启动的容器 / 守护进程停止时是否同时停止容器 |
| `containers.systemd_scope` | 是否把容器放在独立的 systemd scope 中，使其不随守护进程服务停止 |

## 安全设计

- 守护进程默认拒绝以非 root 身份运行；`--allow-non-root` 仅供开发调试。
- 每个连接都会通过 `SO_PEERCRED` 校验对端：仅允许 root、守护进程自身的 uid、`allowed_uids` 中的用户以及 `socket_group` 组成员。socket 文件权限另外受内核约束。
- 更新时必须用 Release 自带的 `SHA256SUMS` 校验通过，并且新二进制 `version` 能正常执行，才会通过原子 `rename` 替换旧文件，旧版本保留为 `<binary>.bak`。
- sing-box 子进程运行在独立的进程组，并设置了 `PR_SET_PDEATHSIG`，守护进程退出后不会留下无人托管的 sing-box。
- **`singbox-board` 组等同于 root**：组成员可以导入和编辑配置，而 sing-box 以 root 运行，配置能决定它读写哪些文件（例如 `log.output`、`external_ui`、本地规则集路径）。请只把可信用户加入该组。导入本地文件时由客户端以调用者自身的权限读取，守护进程不会替客户端读取任意路径。
- 配置库目录仅 root 可访问，配置文件权限为 `0600`；订阅地址中的令牌不会写入日志。
- **Windows**：控制通道是只接受本机连接的命名管道，DACL 只允许 SYSTEM、管理员与 `socket_group` 组成员打开，且客户端只被授予读写数据的权限（没有 `FILE_CREATE_PIPE_INSTANCE`），无法冒充守护进程创建同名管道。守护进程读取请求后模拟客户端令牌确认身份；添加内核源、导入内核等「决定以 SYSTEM 运行什么」的操作只接受已提升权限的管理员。守护进程启动时会把 `C:\ProgramData` 下自己使用的目录设为仅 SYSTEM 与管理员可写（ProgramData 默认允许所有用户创建文件），配置库与含密钥的文件仅 SYSTEM、管理员与所有者可读。
- 容器的配置、根文件系统与运行时目录仅 root 可访问。决定容器内以 root 身份运行内容的请求（新建、编辑、安装、执行命令、导入运行时）只接受 root；客户端以 root 身份打开容器终端前，会确认守护进程报告的运行时程序属于 root 且其他用户不可写。删除容器的文件前会确认其下没有仍处于挂载状态的路径，原地登记的配置文件永远不会被删除。

## 发布

- `.github/workflows/ci.yml`：每次 push 或 PR 时执行 `cargo fmt --check`、`clippy -D warnings`、`cargo test` 和 shellcheck，并在 Windows 上执行 `clippy`、`cargo test` 与 `install.ps1` 的语法检查。
- `.github/workflows/release.yml`：推送 `v*` 标签时，用 cargo-zigbuild 交叉编译 6 个 musl 目标，并完成以下工作：
  - 每个架构都用 qemu-user 实际运行一次；
  - amd64 额外实测脚本安装和 `.deb` 的安装与卸载；
  - 生成程序包、四种安装包和 `SHA256SUMS`，发布到 GitHub Release，同时附带 `install.sh`。
  - 在 Windows 上构建 `x86_64-pc-windows-msvc` 与 `aarch64-pc-windows-msvc`，打包为 `singbox-board-windows-<arch>.zip`，amd64 额外实测 `install.ps1` 安装服务、连接命名管道与卸载，Release 同时附带 `install.ps1`；
  - 修改打包相关文件的 PR 也会触发构建（只构建，不发布）。

发布新版本：

```bash
# 1. 修改 Cargo.toml 中的 version，例如 0.2.0，并提交
# 2. 打标签并推送（标签必须与 Cargo.toml 版本一致，否则流水线会失败）
git tag v0.2.0 && git push origin v0.2.0
```

标签中带 `-`（如 `v0.2.0-rc.1`）的版本会标记为预发布。本地打包方法：

```bash
cargo zigbuild --release --target aarch64-unknown-linux-musl
packaging/package.sh aarch64-unknown-linux-musl arm64 arm64 0.1.0 dist   # 需要 nfpm
```

## 开发

```bash
cargo test
# 非 root 调试：使用自定义配置，socket 放在用户可写的路径
singbox-board daemon -c ./dev.toml --allow-non-root
SINGBOX_BOARD_SOCKET=/path/to/daemon.sock singbox-board
```

在 Linux 上也可以检查和测试 Windows 版：

```bash
rustup target add x86_64-pc-windows-gnu    # 另需 mingw-w64（ring 的 C 代码）
cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine cargo test --target x86_64-pc-windows-gnu
```

Wine 不解析符号链接、也不强制 `FILE_FLAG_FIRST_PIPE_INSTANCE`，相关断言在 Wine 下会跳过，以 Windows 上的 CI 为准。

控制协议为单连接单请求的 NDJSON，例如 `{"cmd":"status"}`、`{"cmd":"logs","tail":100,"follow":true}`、`{"cmd":"profile_list"}`、`{"cmd":"container_control","id":"<ID>","action":"start"}`，单行请求上限 16 MiB（配置内容随请求传输），定义见 `src/protocol.rs`。
