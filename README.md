# singbox-board

为 [MiChongs/sing-box](https://github.com/MiChongs/sing-box)（xiaobaf14g 分支）实现的 root 守护进程与终端面板，使用 Rust + [ratatui](https://ratatui.rs) 编写。

- **root daemon**：以 root 运行并托管 `sing-box run` 子进程（TUN、`auto_route`、tproxy、eBPF 入站都需要 root），负责启停、崩溃后指数退避重启、配置校验与热重载，并从 GitHub Releases 安装或更新内核。
- **可选组件**：[Sub-Store](https://github.com/sub-store-org/Sub-Store)（订阅管理，带 Web 界面）与 [http-meta](https://github.com/xream/http-meta)（按需启动 mihomo 供 Sub-Store 脚本检测节点）。首次运行时会询问是否需要，选择后由守护进程下载、校验、以非特权用户运行并托管。
- **TUI 面板**：通过 Unix socket 连接守护进程，通过 Clash API 连接 sing-box，可查看状态、流量、代理组、连接、日志，以及 Sub-Store 订阅和对应的 sing-box 订阅链接。
- **命令行**：`status / start / stop / restart / reload / check / logs / update / setup / component`，便于脚本调用。

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

## 使用

```bash
singbox-board                 # 打开 TUI（默认）
singbox-board status          # 状态；--json 输出原始 JSON
singbox-board reload          # 先 sing-box check，再发送 SIGHUP
singbox-board logs -f -n 100  # 跟随日志
singbox-board update --check  # 只检查是否有新版本
singbox-board update --tag v1.14.1-xiaobaf14g.1 --force
singbox-board setup                         # 选择可选组件
singbox-board component sub-store           # 状态、Web 地址、订阅的 sing-box 链接
singbox-board component http-meta update    # start|stop|restart|enable|disable|update
```

`systemctl reload singbox-board` 与 `singbox-board reload` 等价。

### TUI 按键

| 按键 | 功能 |
|---|---|
| `1`-`5` / `Tab` | 切换：概览 / 代理 / 连接 / 日志 / Sub-Store |
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
| `?` / `q` | 帮助 / 退出 |

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
| Node.js | 系统 `node`（v22 及以上）；没有时下载官方 LTS | `SHASUMS256.txt` | — |

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
| `socket_group` | 允许使用控制 socket 的用户组。组存在时 socket 为 `0660 root:<组>`，否则为 `0600` |
| `core.binary` / `core.config` / `core.config_dir` / `core.working_dir` | 对应 sing-box 的二进制路径以及 `-c` / `-C` / `-D` 参数 |
| `core.check_before_start` | 每次启动、重启、重载前先运行 `sing-box check`；校验失败时保留正在运行的实例 |
| `restart.policy` | `always` / `on-failure` / `never`，重启间隔按指数退避，上限为 `max_backoff_secs` |
| `update.variant` | 下载的 Release 变体，例如 `ebpf`、`v3-ebpf`、`easytier` |
| `update.proxy` / `update.mirror` | 访问 GitHub 时使用的代理 / 下载镜像前缀（组件下载同样使用） |
| `components.data_dir` / `components.run_as` | 组件安装目录（含 `state.json`）/ 组件运行用户 |
| `components.node` / `components.node_mirror` | 指定 Node.js 路径 / Node.js 下载源（国内可用 `https://npmmirror.com/mirrors/node`） |
| `sub_store.host` / `sub_store.port` | Sub-Store 监听地址，默认 `127.0.0.1:3001` |
| `sub_store.sync_cron` / `produce_cron` / `default_proxy` / `env` | 对应 `SUB_STORE_*` 环境变量 |
| `http_meta.host` / `http_meta.port` / `authorization` | http-meta 监听地址（默认 `127.0.0.1:9876`）与访问凭据 |
| `http_meta.mihomo_arch` | 指定 mihomo 构建，例如 `amd64-v3` |

## 安全设计

- 守护进程默认拒绝以非 root 身份运行；`--allow-non-root` 仅供开发调试。
- 每个连接都会通过 `SO_PEERCRED` 校验对端：仅允许 root、守护进程自身的 uid、`allowed_uids` 中的用户以及 `socket_group` 组成员。socket 文件权限另外受内核约束。
- 更新时必须用 Release 自带的 `SHA256SUMS` 校验通过，并且新二进制 `version` 能正常执行，才会通过原子 `rename` 替换旧文件，旧版本保留为 `<binary>.bak`。
- sing-box 子进程运行在独立的进程组，并设置了 `PR_SET_PDEATHSIG`，守护进程退出后不会留下无人托管的 sing-box。

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

控制协议为单连接单请求的 NDJSON，例如 `{"cmd":"status"}`、`{"cmd":"logs","tail":100,"follow":true}`，定义见 `src/protocol.rs`。
