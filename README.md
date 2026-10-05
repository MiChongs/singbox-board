# singbox-board

为 [MiChongs/sing-box](https://github.com/MiChongs/sing-box)（xiaobaf14g 分支）实现的 root 守护进程与终端面板，使用 Rust + [ratatui](https://ratatui.rs) 编写。

- **root daemon**：以 root 运行并托管 `sing-box run` 子进程（TUN、`auto_route`、tproxy、eBPF 入站都需要 root），负责启停、崩溃后指数退避重启、配置校验与热重载，并从 GitHub Releases 安装或更新内核。
- **TUI 面板**：通过 Unix socket 连接守护进程，通过 Clash API 连接 sing-box，可查看状态、流量、代理组、连接和日志。
- **命令行**：`status / start / stop / restart / reload / check / logs / update`，便于脚本调用。

```
            ┌──────────── singbox-board daemon (root) ────────────┐
 TUI / CLI ─┤ /run/singbox-board/daemon.sock  (NDJSON, SO_PEERCRED) │
  (用户)     │   supervisor ── spawn ──► sing-box run (子进程)        │
            │   log ring  ◄── stdout/stderr                         │
            │   updater   ──► GitHub Releases + SHA256SUMS          │
            └──────────────────────────────────────────────────────┘
 TUI ──────────────── HTTP ────────────────► Clash API (experimental.clash_api)
```

## 构建

```bash
cargo build --release
# 产物：target/release/singbox-board
```

TLS 使用 rustls + ring，不依赖 OpenSSL。

## 安装（systemd）

```bash
sudo install -m 0755 target/release/singbox-board /usr/local/bin/
sudo install -d /etc/singbox-board
sudo singbox-board daemon --print-default-config | sudo tee /etc/singbox-board/daemon.toml
sudo install -m 0644 contrib/singbox-board.service /etc/systemd/system/

# 允许非 root 用户使用面板：加入 socket 组
sudo groupadd --system singbox-board
sudo usermod -aG singbox-board "$USER"     # 重新登录后生效

sudo systemctl daemon-reload
sudo systemctl enable --now singbox-board

# 首次安装 sing-box 内核（下载 MiChongs/sing-box 最新 Release 并校验 SHA256）
singbox-board update
singbox-board start
```

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
```

`systemctl reload singbox-board` 与 `singbox-board reload` 等价。

### TUI 按键

| 按键 | 功能 |
|---|---|
| `1`-`4` / `Tab` | 切换：概览 / 代理 / 连接 / 日志 |
| `s` `x` `r` | 启动 / 停止 / 重启 sing-box（停止和重启需要确认） |
| `R` | 校验配置并热重载 |
| `c` | 运行 `sing-box check` |
| `u` | 检查更新，确认后下载安装 |
| `m` | 切换 Clash 模式 |
| 代理页 `←→` `Enter` | 在组与节点间切换焦点 / 选择节点（Selector、URLTest、Smart） |
| 代理页 `t` / `T` | 测试整组 / 单个节点的延迟 |
| 连接页 `d` / `D` | 关闭选中 / 全部连接 |
| 日志页 `↑↓` `PgUp/PgDn` `End` | 滚动；按 `End` 恢复跟随 |
| `?` / `q` | 帮助 / 退出 |

## 配置

完整的带注释配置见 [`contrib/daemon.toml`](contrib/daemon.toml)，也可以用 `singbox-board daemon --print-default-config` 输出。常用项：

| 键 | 说明 |
|---|---|
| `socket_group` | 允许使用控制 socket 的用户组。组存在时 socket 为 `0660 root:<组>`，否则为 `0600` |
| `core.binary` / `core.config` / `core.config_dir` / `core.working_dir` | 对应 sing-box 的二进制路径以及 `-c` / `-C` / `-D` 参数 |
| `core.check_before_start` | 每次启动、重启、重载前先运行 `sing-box check`；校验失败时保留正在运行的实例 |
| `restart.policy` | `always` / `on-failure` / `never`，重启间隔按指数退避，上限为 `max_backoff_secs` |
| `update.variant` | 下载的 Release 变体，例如 `ebpf`、`v3-ebpf`、`easytier` |
| `update.proxy` / `update.mirror` | 访问 GitHub 时使用的代理 / 下载镜像前缀 |

## 安全设计

- 守护进程默认拒绝以非 root 身份运行；`--allow-non-root` 仅供开发调试。
- 每个连接都会通过 `SO_PEERCRED` 校验对端：仅允许 root、守护进程自身的 uid、`allowed_uids` 中的用户以及 `socket_group` 组成员。socket 文件权限另外受内核约束。
- 更新时必须用 Release 自带的 `SHA256SUMS` 校验通过，并且新二进制 `version` 能正常执行，才会通过原子 `rename` 替换旧文件，旧版本保留为 `<binary>.bak`。
- sing-box 子进程运行在独立的进程组，并设置了 `PR_SET_PDEATHSIG`，守护进程退出后不会留下无人托管的 sing-box。

## 开发

```bash
cargo test
# 非 root 调试：使用自定义配置，socket 放在用户可写的路径
singbox-board daemon -c ./dev.toml --allow-non-root
SINGBOX_BOARD_SOCKET=/path/to/daemon.sock singbox-board
```

控制协议为单连接单请求的 NDJSON，例如 `{"cmd":"status"}`、`{"cmd":"logs","tail":100,"follow":true}`，定义见 `src/protocol.rs`。
