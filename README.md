# singbox-board

为 [MiChongs/sing-box](https://github.com/MiChongs/sing-box)（xiaobaf14g 分支）实现的 root 守护进程与终端面板，使用 Rust + [ratatui](https://ratatui.rs) 编写。

- **root daemon**：以 root 运行并托管 `sing-box run` 子进程（TUN、`auto_route`、tproxy、eBPF 入站都需要 root），负责启停、崩溃后指数退避重启、配置校验与热重载，并从 GitHub Releases 安装或更新内核。
- **核心版本管理**：内置 MiChongs（xiaobaf14g）与 SagerNet 官方两个发布源，可添加任意 GitHub 源或导入自编译核心。可列出某个源的全部版本及本机可用的构建变体（ebpf、easytier、glibc、musl 等），一键下载、校验、切换或回退，多个版本并存，切换瞬间完成，启动失败会自动回滚。
- **配置管理**：多份 sing-box 配置并存，可从文件或订阅地址导入（订阅按间隔自动更新，并显示服务商返回的流量与到期时间），也可用模板新建。切换前先用 sing-box 校验，启动失败自动回滚；在 TUI 中可用树形编辑器可视化修改，或交给 `$EDITOR` 编辑原文。
- **可选组件**：[Sub-Store](https://github.com/sub-store-org/Sub-Store)（订阅管理，带 Web 界面）与 [http-meta](https://github.com/xream/http-meta)（按需启动 mihomo 供 Sub-Store 脚本检测节点）。首次运行时会询问是否需要，选择后由守护进程下载、校验、以非特权用户运行并托管。
- **TUI 面板**：通过 Unix socket 连接守护进程，通过 Clash API 连接 sing-box，可查看状态、流量、代理组、连接、日志，以及 Sub-Store 订阅和对应的 sing-box 订阅链接；在「配置」页管理和编辑配置。
- **命令行**：`status / start / stop / restart / reload / check / logs / update / setup / component / core / profile`，便于脚本调用。

```
            ┌──────────────── singbox-board daemon (root) ─────────────────┐
 TUI / CLI ─┤ /run/singbox-board/daemon.sock  (NDJSON, SO_PEERCRED)          │
  (用户)     │   supervisor ── spawn ──► sing-box run                          │
            │   components ── spawn ──► node sub-store.bundle.js   (nobody)  │
            │                └ spawn ──► node http-meta.bundle.js  (nobody)  │
            │                              └► mihomo (按需)                   │
            │   log ring  ◄── 各子进程 stdout/stderr                          │
            │   updater   ──► GitHub Releases（sha256 校验）/ nodejs.org      │
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
singbox-board profile edit 家里               # 用 $EDITOR 编辑，正在使用的配置保存后自动重载
singbox-board setup                         # 选择可选组件
singbox-board component sub-store           # 状态、Web 地址、订阅的 sing-box 链接
singbox-board component http-meta update    # start|stop|restart|enable|disable|update
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
| `1`-`7` / `Tab` | 切换：概览 / 代理 / 连接 / 日志 / Sub-Store / Core / 配置 |
| `s` `x` `r` | 启动 / 停止 / 重启 sing-box（停止和重启需要确认） |
| `R` | 校验配置并热重载 |
| `c` | 运行 `sing-box check` |
| `u` | 检查更新，确认后下载安装 |
| `m` | 切换 Clash 模式 |
| 代理页 `←→` `Enter` | 在组与节点间切换焦点 / 选择节点（Selector、URLTest、Smart） |
| 代理页 `t` / `T` | 测试整组 / 单个节点的延迟 |
| 连接页 `d` / `D` | 关闭选中 / 全部连接 |
| 日志页 `↑↓` `PgUp/PgDn` `End` | 滚动；按 `End` 恢复跟随 |
| Sub-Store 页 `←→` `Enter` | 在组件与订阅间切换焦点 / 组件操作菜单（启动、停止、更新、启用、禁用）或 provider 配置片段 |
| Sub-Store 页 `y` `w` `p` | 复制 sing-box 订阅链接 / 复制 Web 界面地址 / 显示 provider 配置片段 |
| Core 页 `←→` `Enter` `v` `i` `d` | 在来源、版本、已安装之间切换焦点 / 下载并切换 / 切换变体 / 仅下载 / 删除 |
| Core 页 `p` `n` `f` `a` `I` | 只看正式版 / 下一页 / 刷新 / 添加源 / 导入核心 |
| 配置页 `Enter` | 操作菜单：使用、编辑、更新、重命名、订阅地址与更新间隔、复制、校验、导出、删除 |
| 配置页 `e` `E` `n` `i` | 树形编辑 / 用 `$EDITOR` 编辑 / 用模板新建 / 导入文件或订阅地址（支持粘贴） |
| 配置页 `f` `F` `d` `A` | 更新所选订阅 / 更新全部订阅 / 删除 / 将当前配置文件收入配置库 |
| `?` / `q` | 帮助 / 退出 |

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
| 可视化编辑 | `e` | 无 |
| 编辑原文 | `E` | `singbox-board profile edit <名称>` |
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

### 可视化编辑

在配置页按 `e` 打开树形编辑器，左侧为 JSON 树，右侧显示所选项的完整内容。编辑器打开时，`s`、`r` 等键只作用于编辑器，不会误操作 sing-box；`Tab` 和数字键仍可切换标签页，编辑状态会保留。

| 按键 | 功能 |
|---|---|
| `↑↓` `←→` `Space` `*` `-` | 移动 / 折叠或展开 / 切换 / 全部展开 / 全部折叠 |
| `Enter` / `e` | 编辑值：开关直接切换；`outbound`、`detour`、`final`、DNS `server`、规则中的 `rule_set` 等引用字段从现有标签中选择 |
| `:` / `E` | 以 JSON 编辑所选项 / 在 `$EDITOR` 中编辑所选项 |
| `a` / `A` | 在之后添加（所选为展开的容器时添加到其中）/ 在所选容器内添加：在 `inbounds`、`outbounds`、`endpoints`、`route.rules`、`route.rule_set`、`dns.servers`、`dns.rules`、`providers` 中提供常用模板（mixed、tun、selector、urltest、VLESS REALITY、Hysteria2、规则集、DoH 等，均已用 sing-box 校验），`providers` 中还会列出 Sub-Store 的订阅；向分组成员列表添加时列出现有出站 |
| `r` `d` `c` `K` `J` | 重命名键 / 删除 / 复制一份（自动避开重复的 tag）/ 上移 / 下移 |
| `u` `U` `/` `n` `N` `y` | 撤销 / 重做 / 搜索 / 下一个 / 上一个 / 复制为 JSON |
| `s` / `q` | 校验并保存 / 关闭（有未保存修改时会确认） |

树形编辑器保存时会按标准 JSON 重新排版（保留键的顺序），文件中的注释会被移除；需要保留注释时请用 `E` 编辑原文。

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

## 配置

完整的带注释配置见 [`contrib/daemon.toml`](contrib/daemon.toml)，也可以用 `singbox-board daemon --print-default-config` 输出。常用项：

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

## 安全设计

- 守护进程默认拒绝以非 root 身份运行；`--allow-non-root` 仅供开发调试。
- 每个连接都会通过 `SO_PEERCRED` 校验对端：仅允许 root、守护进程自身的 uid、`allowed_uids` 中的用户以及 `socket_group` 组成员。socket 文件权限另外受内核约束。
- 更新时必须用 Release 自带的 `SHA256SUMS` 校验通过，并且新二进制 `version` 能正常执行，才会通过原子 `rename` 替换旧文件，旧版本保留为 `<binary>.bak`。
- sing-box 子进程运行在独立的进程组，并设置了 `PR_SET_PDEATHSIG`，守护进程退出后不会留下无人托管的 sing-box。
- **`singbox-board` 组等同于 root**：组成员可以导入和编辑配置，而 sing-box 以 root 运行，配置能决定它读写哪些文件（例如 `log.output`、`external_ui`、本地规则集路径）。请只把可信用户加入该组。导入本地文件时由客户端以调用者自身的权限读取，守护进程不会替客户端读取任意路径。
- 配置库目录仅 root 可访问，配置文件权限为 `0600`；订阅地址中的令牌不会写入日志。

## 发布

- `.github/workflows/ci.yml`：每次 push 或 PR 时执行 `cargo fmt --check`、`clippy -D warnings`、`cargo test` 和 shellcheck。
- `.github/workflows/release.yml`：推送 `v*` 标签时，用 cargo-zigbuild 交叉编译 6 个 musl 目标，并完成以下工作：
  - 每个架构都用 qemu-user 实际运行一次；
  - amd64 额外实测脚本安装和 `.deb` 的安装与卸载；
  - 生成程序包、四种安装包和 `SHA256SUMS`，发布到 GitHub Release，同时附带 `install.sh`。
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

控制协议为单连接单请求的 NDJSON，例如 `{"cmd":"status"}`、`{"cmd":"logs","tail":100,"follow":true}`、`{"cmd":"profile_list"}`，单行请求上限 16 MiB（配置内容随请求传输），定义见 `src/protocol.rs`。
