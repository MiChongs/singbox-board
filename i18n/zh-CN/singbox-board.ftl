# singbox-board 界面文案：简体中文。
#
# 规范：使用书面语与全角标点；中文与英文、数字之间保留一个空格；
# 状态与标签不加句号，完整的说明性语句以句号结尾；错误信息不加句号，
# 以便与上下文串联（例如“无法读取 X：权限不足”）。
# 列举使用顿号或逗号，不使用间隔号与破折号。
#
# 术语：daemon 守护进程；core 内核；release 发行版本；release source 发布源；
# build variant 构建变体；version store 版本库；component 组件；
# terminal dashboard 终端管理面板；socket group 套接字用户组。

## 通用词语与片段

list-separator = 、
clause-separator = ，
chain-separator = ：
none = 无
unknown = 未知
not-installed = 未安装
answer-yes = 是
answer-no = 否
error-line = 错误：{ $message }
up-to-date = { $name } 已是最新版本
version-prerelease = { $version }（预发布）
prerelease-marker = 预发布
no-build-for-platform = 无适用于本平台的构建
duration-days = { $days } 天 { $clock }
with-process = { $text }（PID { $pid }，已运行 { $uptime }）
with-restart-in = { $text }（{ $seconds } 秒后重启）
state-pid-uptime = PID { $pid }，已运行 { $uptime }
clash-api-not-configured = 未配置（experimental.clash_api）
ask-sub-store = 是否启用 Sub-Store？
ask-http-meta = 是否启用 http-meta？

## 进程与组件状态（状态标识）

state-stopped = 已停止
state-starting = 启动中
state-running = 运行中
state-stopping = 停止中
state-backoff = 等待重启
state-failed = 运行失败
state-disabled = 未启用
busy-installing = 正在安装
busy-updating = 正在更新

## 内核构建与发布源

variant-default = 默认
version-backend = 后端
version-frontend = 前端
checksum-sums = SHA256SUMS
checksum-digest = GitHub 摘要
checksum-pinned = 指定 SHA-256
checksum-none = 未校验
checksum-verified-sums = 已通过 SHA256SUMS 校验
checksum-verified-digest = 已通过 GitHub 摘要校验
checksum-verified-pinned = 已通过指定的 SHA-256 校验
checksum-unverified = 未经校验
source-michongs = MiChongs（xiaobaf14g）
source-michongs-description = 支持 Smart、XHTTP、EasyTier 与 eBPF
source-sagernet = SagerNet（官方）
source-sagernet-description = 上游官方发行版本
source-configured-description = 来自 daemon.toml 中的 update.repo
source-custom-description = 自定义发布源
source-custom-tag = 自定义
source-imported = 自定义导入
source-adopted = 原有安装
core-label = sing-box { $version }（{ $source }，{ $variant }）
log-source-daemon = 守护进程
entry-subscription = 单条订阅
entry-collection = 组合订阅
snippet-group-comment = 在策略组中引用这些节点，例如：

## 进程退出说明

exit-code = 退出码 { $code }
exit-signal = 被信号 { $signal } 终止
exit-unknown = 未知的退出状态
exit-wait-failed = 等待进程结束时出错：{ $error }

## 通用错误

err-create = 无法创建 { $path }
err-create-workdir = 无法创建工作目录 { $path }
err-read = 无法读取 { $path }
err-parse = 无法解析 { $path }
err-replace = 无法将新文件写入 { $path }
err-delete = 无法删除 { $path }
err-chown = 无法修改 { $path } 的所有者
err-not-found = { $path } 不存在
err-spawn = 无法启动 { $path }
err-request = 请求 { $url } 失败
err-decode = 无法解析 { $url } 返回的内容
err-download = 下载 { $url } 失败
err-too-large = { $url } 超出大小上限 { $limit }
err-checksum-mismatch = { $name } 的校验值不一致：预期为 { $expected }，实际为 { $actual }
err-open-zip = 无法打开 zip 压缩包
err-archive-missing = 压缩包中不包含 { $name }
err-task-panicked = 后台任务意外终止
err-sub-store-url = Sub-Store 地址 { $url } 无效
err-clash-api-url = Clash API 地址 { $url } 无效

## 命令行：帮助信息

cli-about = MiChongs/sing-box 的 root 守护进程与终端管理面板
cli-long-about =
    MiChongs/sing-box 的 root 守护进程与终端管理面板。

    请以 root 身份运行 `singbox-board daemon` 以托管 sing-box，随后以 root 或套接字用户组成员的身份使用 `singbox-board`（终端管理面板）或下列命令。
cli-help-usage = 用法：
cli-help-commands = 命令：
cli-help-arguments = 参数：
cli-help-options = 选项：
cli-help = 显示帮助信息
cli-version = 显示版本信息
cli-socket = 守护进程的控制套接字。默认使用 daemon.toml 中的设置，未设置时为 /run/singbox-board/daemon.sock。也可通过环境变量 SINGBOX_BOARD_SOCKET 指定。
cli-lang = 界面语言，可选 en 或 zh-CN，默认跟随系统区域设置。也可通过环境变量 SINGBOX_BOARD_LANG 指定。
cli-lang-invalid = 不支持语言 { $value }，可选语言为 en、zh-CN
cli-tui = 打开终端管理面板（默认）
cli-daemon = 运行托管 sing-box 的 root 守护进程
cli-daemon-config = 守护进程配置文件
cli-daemon-allow-non-root = 允许以非 root 身份运行（仅用于开发）
cli-daemon-print-default-config = 输出带注释的默认配置后退出
cli-status = 显示守护进程与 sing-box 的状态
cli-status-json = 以 JSON 格式输出状态
cli-start = 启动 sing-box
cli-stop = 停止 sing-box
cli-restart = 重启 sing-box
cli-reload = 校验配置并热重载 sing-box（SIGHUP）
cli-check = 校验 sing-box 配置（sing-box check）
cli-logs = 输出 sing-box 与守护进程的日志
cli-logs-tail = 首先输出的缓存日志行数（默认为 200）
cli-logs-follow = 持续输出新产生的日志
cli-update = 从 GitHub 发行版本安装或更新 sing-box，沿用当前内核的发布源与构建变体
cli-update-check = 仅检查是否有可用更新
cli-update-tag = 安装指定的发布标签，例如 v1.14.1-xiaobaf14g.1
cli-update-force = 即使版本未变化也重新安装
cli-setup = 选择可选组件（Sub-Store、http-meta），首次运行时会询问此项
cli-setup-sub-store = 是否启用 Sub-Store，取值为 yes 或 no；省略时以交互方式询问
cli-setup-http-meta = 是否启用 http-meta，取值为 yes 或 no；省略时以交互方式询问
cli-component = 管理可选组件；未指定操作时显示组件详情与访问地址
cli-component-name = 组件：sub-store（带 Web 界面的订阅管理工具）或 http-meta（供 Sub-Store 脚本检测节点可用性）
cli-component-action = 操作：start（启动）、stop（停止）、restart（重启）、enable（按需安装并立即启动，此后随守护进程自动启动）、disable（停止并保持停止）或 update（下载最新版本并重启）
cli-core = 管理内核版本：列出发行版本、安装、切换及导入自定义构建
cli-core-sources = 列出发布源（MiChongs、SagerNet 及自定义仓库）
cli-core-source = 添加或移除自定义 GitHub 发布源（仅限 root）
cli-core-source-option = 发布源，格式为 owner/repo（默认为 update 命令所使用的发布源）
cli-core-list = 列出发布源的发行版本及适用于本机的构建
cli-core-list-page = 页码，从 1 开始
cli-core-list-stable = 隐藏预发布版本
cli-core-list-refresh = 忽略 10 分钟缓存，重新获取
cli-core-installed = 列出本地版本库中的内核
cli-core-install = 将发行版本下载到版本库并切换至该版本
cli-core-install-tag = 发布标签，例如 v1.14.1-xiaobaf14g.1
cli-core-install-variant = 构建变体，例如 ebpf、glibc 或 musl（默认为标准构建）
cli-core-no-switch = 仅存入版本库，不切换
cli-core-force = 即使新内核未通过当前配置校验，也执行切换
cli-core-use = 切换至版本库中的内核
cli-core-id = 已存储内核的 ID、版本号或发布标签
cli-core-remove = 删除版本库中的内核
cli-core-import = 从本地文件或 HTTP(S) 地址导入自定义内核（仅限 root）
cli-core-import-location = 二进制文件、.tar.gz、.zip 或 .gz 文件的绝对路径或 URL
cli-core-import-sha256 = 文件的预期 SHA-256 校验值
cli-source-add = 添加发布 sing-box-<version>-linux-<arch> 压缩包的 GitHub 仓库
cli-source-add-repo = 仓库，格式为 owner/repo
cli-source-add-name = 显示名称（默认为仓库名）
cli-source-remove = 移除自定义发布源
cli-source-remove-repo = 发布源所在仓库，格式为 owner/repo

## 命令行：参数错误

cli-err-unknown-argument = 无法识别的参数“{ $arg }”
cli-err-invalid-subcommand = 无法识别的命令“{ $name }”
cli-err-invalid-value = 参数 { $arg } 的取值“{ $value }”无效
cli-err-possible-values = 可选值：{ $values }
cli-err-validation = 参数 { $arg } 的取值“{ $value }”无效：{ $reason }
cli-err-missing = 缺少以下必需参数：{ $args }
cli-err-conflict = 参数 { $arg } 不能与 { $other } 同时使用
cli-err-missing-subcommand = 命令“{ $name }”需要指定子命令
cli-err-no-equals = 为 { $arg } 赋值时须使用等号
cli-err-wrong-values = 参数 { $arg } 的取值数量不正确
cli-err-generic = 命令行参数无效：{ $detail }
cli-err-suggestion = 提示：您是否要使用“{ $suggestion }”？
cli-err-suggestions = { $kind ->
    [command] 提示：存在相近的命令：{ $suggestions }
    [argument] 提示：存在相近的参数：{ $suggestions }
   *[value] 提示：存在相近的取值：{ $suggestions }
    }
cli-quoted = “{ $text }”
cli-err-help-hint = 如需了解更多信息，请使用 --help 选项。

## 命令行：输出

client-daemon-not-running = 守护进程未运行（{ $socket } 处没有套接字），请执行 `systemctl start singbox-board` 或 `sudo singbox-board daemon` 启动守护进程
client-permission-stale-group = 无权访问 { $socket }：您已是 `{ $group }` 用户组的成员，但当前登录会话建立于加入该用户组之前。请执行 `newgrp { $group }`（或 `sg { $group } -c singbox-board`），或注销后重新登录
client-permission-denied = 无权访问 { $socket }，请以 root 身份运行，或加入套接字用户组（执行 `sudo usermod -aG singbox-board $USER` 后重新登录）
client-connect-failed = 无法连接 { $socket }：{ $error }
client-connection-closed = 守护进程已关闭连接
client-timeout = 守护进程未在 { $seconds } 秒内响应
client-unexpected-response = 守护进程返回了无法识别的响应：{ $response }
ctl-label-daemon = 守护进程
ctl-label-version = 版本
ctl-label-binary = 程序路径
ctl-label-args = 启动参数
ctl-label-restarts = 重启次数
ctl-label-last-exit = 上次退出
ctl-label-update = 更新
ctl-label-installed = 已安装
ctl-label-web-ui = Web 界面
ctl-label-endpoint = 服务地址
ctl-label-current = 当前版本
ctl-label-latest = 最新版本
ctl-label-asset = 发行文件
ctl-clash-api-secret = { $url }（已设置密钥）
ctl-update-in-progress = 正在进行
ctl-setup-hint = 首次运行：请执行 `singbox-board setup` 或在终端管理面板中选择可选组件（Sub-Store、http-meta）。
ctl-components-intro =
    singbox-board 还可以安装并托管以下两个可选组件：
      Sub-Store   带 Web 界面的订阅管理工具，可将订阅转换为 sing-box 格式，
                  供 `providers` 使用（sub-store-org/Sub-Store）。
      http-meta   按需启动 mihomo，供 Sub-Store 脚本检测节点是否可用
                  （xream/http-meta）。
    两个组件均以非特权用户身份运行。如系统未安装 Node.js，将自动下载。
ctl-ask-no-terminal = 当前没有可交互的终端，无法询问“{ $question }”，请使用 --sub-store 与 --http-meta 选项指定 yes 或 no
ctl-ask-no-answer = 未收到回答
ctl-ask-retry = 请输入 y 或 n。
ctl-setup-installing = 正在安装并启动所选组件，可能需要一些时间。
ctl-may-download = 此操作可能需要下载文件，耗时可能较长。
ctl-component-unknown = 守护进程未报告 { $component } 的状态
ctl-subscriptions-error = 无法获取订阅列表：{ $error }
ctl-subscriptions-empty = 暂无订阅，请在 Web 界面中添加。
ctl-subscriptions-title = sing-box 订阅链接：
ctl-provider-title = sing-box 的 provider 配置（请添加到配置文件中）：
ctl-latest = { $version }（发布日期：{ $date }）
ctl-latest-prerelease = { $version }（预发布，发布日期：{ $date }）
ctl-update-available = 有可用更新，请执行 `singbox-board update` 进行安装。
ctl-up-to-date = sing-box 已是最新版本。
ctl-update-downloading = 正在下载并校验发行版本，可能需要一些时间。
ctl-importing-core = 正在存储自定义内核，可能需要一些时间。
ctl-core-unmanaged = sing-box { $version }，位于 { $binary }（不受版本库管理）
ctl-core-none = 尚未安装 sing-box 内核。
ctl-core-hint-list = 列出发行版本：singbox-board core list
ctl-core-hint-switch = 切换内核：singbox-board core install <tag> 或 singbox-board core use <id>
ctl-releases-title = { $source }，{ $platform }，第 { $page } 页
ctl-releases-title-more = { $source }，{ $platform }，第 { $page } 页（下一页：--page { $next }）
ctl-col-version = 版本
ctl-col-published = 发布日期
ctl-col-variants = 构建变体（● 当前使用，✓ 已存储）
ctl-prerelease-marker = 预发布
ctl-no-build = 无适用于本平台的构建
ctl-install-hint = 安装：singbox-board core install <tag> --source { $source } [--variant <name>]
ctl-store-empty = 版本库中暂无内核。
ctl-store-title = 已安装的内核：
ctl-core-not-found = 版本库中没有与“{ $query }”匹配的内核，请执行 `singbox-board core installed` 查看已存储的内核
ctl-core-ambiguous = “{ $query }”匹配到多个内核，请指定以下 ID 之一：
ctl-core-downloading = 正在下载 { $source } { $tag }（{ $variant }），版本库中已有时将直接使用。

## 终端管理面板：标签页、面板与表头

tab-overview = 概览
tab-proxies = 代理
tab-connections = 连接
tab-logs = 日志
tab-core = 内核
tui-panel-recent-logs = 最近日志
tui-panel-traffic = 流量
tui-panel-groups = 代理组
tui-panel-groups-count = 代理组（{ $count }）
tui-panel-connections = 连接（{ $count }）
tui-panel-logs = 日志（{ $count }）
tui-panel-components = 组件
tui-panel-subscriptions = sing-box 订阅
tui-panel-subscriptions-version = sing-box 订阅（Sub-Store { $version }）
tui-panel-confirm = 操作确认
tui-panel-active-core = 当前内核
tui-panel-sources = 发布源
tui-panel-releases-empty = 发行版本
tui-panel-releases = { $source } 的发行版本（{ $platform }）
tui-panel-releases-stable = { $source } 的发行版本（{ $platform }，仅正式版）
tui-panel-installed = 已安装（{ $count }）
field-state = 状态
field-core = 内核
field-binary = 程序路径
field-restarts = 重启次数
field-last-exit = 上次退出
field-daemon = 守护进程
field-error = 错误
field-mode = 模式
field-connections = 连接数
field-memory = 内存
col-name = 名称
col-type = 类型
col-delay = 延迟
col-destination = 目标地址
col-network = 网络
col-chain = 链路
col-rule = 规则
col-upload = 上传
col-download = 下载
col-age = 时长
col-component = 组件
col-state = 状态
col-versions = 版本
col-url = 访问地址
col-source = 来源
col-singbox-url = sing-box 订阅链接
col-version = 版本
col-published = 发布日期
col-variant = 构建变体
col-size = 大小
col-checksum = 校验
col-installed = 安装日期

## 终端管理面板：按键提示

key-focus = 切换焦点
key-select = 选择
key-test = 测速
key-test-one = 单节点测速
key-move = 移动
key-close = 关闭
key-close-all = 全部关闭
key-scroll = 滚动
key-page = 翻页
key-follow = 跟随
key-actions = 操作
key-copy-url = 复制链接
key-web-ui = Web 界面
key-provider-snippet = provider 配置
key-start = 启动
key-stop = 停止
key-restart = 重启
key-reload = 重载
key-update = 更新
key-mode = 模式
key-help = 帮助
key-pane = 切换窗格
key-source = 发布源
key-add = 添加
key-remove = 移除
key-import = 导入
key-switch = 切换
key-variant = 构建变体
key-download = 下载
key-stable = 正式版
key-more = 更多
key-refresh = 刷新
key-delete = 删除
key-next = 下一个
key-confirm = 确认
key-cancel = 取消
key-submit = 提交
key-clear = 清空
key-yes = 是
key-no = 否
key-ask-later = 稍后询问

## 终端管理面板：状态文本

tui-connecting = 正在连接守护进程
tui-waiting-daemon = 正在等待守护进程
tui-header-daemon = 守护进程 { $version }，PID { $pid }
tui-uptime = 已运行 { $uptime }
tui-restart-in = { $seconds } 秒后重启
tui-core-not-installed = 未安装，请前往“内核”标签页安装
tui-downloading = 正在下载
tui-daemon-detail = { $version }，已运行 { $uptime }，{ $socket }
tui-traffic-total = 累计 { $bytes }
tui-testing = 测速中
tui-delay-timeout = 超时
tui-logs-following = 跟随中
tui-logs-scrolled = 已向上滚动 { $lines } 行，按 End 键恢复跟随
tui-logs-disconnected = 已断开
tui-sub-store-disabled = Sub-Store 未启用。请在上方选中该组件，按 Enter 键后选择“启用”。
tui-sub-store-stopped = Sub-Store 未运行。
tui-no-subscriptions = 暂无订阅。请在 Web 界面中添加，按 w 键可复制其地址。
tui-loading = 正在加载
tui-active = 当前使用
tui-stored = 已存储
tui-no-build = 无构建
tui-unmanaged = 未纳入管理
tui-unmanaged-note = { $binary } 为普通文件，切换内核时将以“{ $name }”的名义保存到版本库中。
tui-no-core = 未安装内核
tui-no-core-hint = 请在下方选择发行版本，按 Enter 键安装并切换。
tui-installed-on = 安装于 { $date }
tui-file-count = 共 { $count } 个文件
tui-linked-at = 链接位置 { $path }
tui-releases-shown = 已显示 { $count } 个
tui-releases-shown-more = 已显示 { $count } 个，按 n 键加载更多
tui-variants = 构建变体
tui-loading-releases = 正在加载发行版本
tui-no-releases = 暂无发行版本
tui-on-disk = 占用 { $size }
tui-store-empty = 版本库为空。切换或通过 i 键下载的发行版本将显示在此处。

## 终端管理面板：通知与进度

tui-title-error = 错误
tui-title-result = 执行结果
tui-hint-close = 按 Esc 关闭
tui-hint-copy-close = 按 y 复制，按 Esc 关闭
tui-confirm-stop = 是否停止 sing-box？
tui-confirm-restart = 是否重启 sing-box？
tui-confirm-close-all = 是否关闭全部连接？
tui-confirm-update =
    是否安装 sing-box { $version }？
    当前版本：{ $current }
tui-confirm-switch = 是否切换至 sing-box { $version }？
tui-confirm-download = 是否下载 sing-box { $version }？
tui-confirm-delete = 是否删除已存储的内核 sing-box { $version }？
tui-confirm-remove-source = 是否移除内核发布源 { $source }？
tui-detail-build = { $source }，构建变体：{ $variant }
tui-detail-download = 下载大小：{ $size }；校验方式：{ $checksum }
tui-detail-stored = 已在版本库中；校验方式：{ $checksum }
tui-detail-frees = 删除后将释放 { $size } 空间。
tui-switch-note = 将先校验当前配置，随后 sing-box 将使用新内核重启。
tui-remove-source-note = 已从该发布源存储的内核将予以保留。
tui-no-build-for = { $version } 没有适用于 { $platform } 的构建
tui-already-active = { $version } 已是当前内核
tui-active-not-deletable = 无法删除当前内核，请先切换至其他内核
tui-builtin-source = { $name } 为内置发布源，无法移除
tui-add-source-title = 添加内核发布源
tui-add-source-hint = 发布 sing-box-<version>-linux-<arch>.tar.gz 的 GitHub 仓库（仅限 root）
tui-import-title = 导入自定义内核
tui-import-hint = 二进制文件、.tar.gz、.zip 或 .gz 文件的绝对路径或 http(s) 地址，可在其后附加 SHA-256 校验值（仅限 root）
tui-copied-snippet = 已复制 provider 配置片段
tui-copied-subscription-url = 已复制 sing-box 订阅链接
tui-copied-url = 已复制访问地址
tui-copied-web-ui = 已复制 Sub-Store Web 界面地址
tui-no-url = 暂无访问地址，请先启用该组件
tui-sub-store-not-set-up = Sub-Store 尚未配置
tui-status-unavailable = 暂时无法获取守护进程状态
tui-no-subscription-selected = 未选择订阅
tui-snippet-title = { $name } 的 sing-box provider 配置
tui-connections-closed = 已关闭全部连接
tui-connection-closed = 已关闭与 { $target } 的连接
tui-clash-api-missing = 未配置 Clash API（experimental.clash_api）
tui-mode-list-unavailable = 暂时无法获取模式列表
tui-single-mode = 当前仅配置了一种 Clash 模式
tui-mode-set = 已切换至 { $mode } 模式
tui-not-selectable = 代理组 { $group } 不支持手动选择节点
tui-node-selected = 代理组 { $group } 已切换至 { $node }
busy-starting = 正在启动 sing-box
busy-stopping = 正在停止 sing-box
busy-restarting = 正在重启 sing-box
busy-reloading = 正在重载配置
busy-checking = 正在校验配置
busy-downloading = 正在下载 sing-box
busy-checking-updates = 正在检查更新
busy-setup = 正在配置组件
busy-component = { $action ->
    [start] 正在启动 { $component }
    [stop] 正在停止 { $component }
    [restart] 正在重启 { $component }
    [enable] 正在安装 { $component }
    [disable] 正在禁用 { $component }
   *[update] 正在更新 { $component }
    }
busy-closing-connections = 正在关闭全部连接
busy-closing-connection = 正在关闭连接
busy-switching-mode = 正在切换模式
busy-selecting-node = 正在切换节点
busy-adding-source = 正在添加内核发布源
busy-importing-core = 正在导入内核
busy-switching-core = 正在切换内核
busy-downloading-core = 正在下载内核
busy-deleting-core = 正在删除内核
busy-removing-source = 正在移除发布源

## 终端管理面板：弹出窗口

help-title = 快捷键
help-close-hint = 按任意键关闭
help-global = 全局
help-switch-tab = 切换标签页
help-start-stop-restart = 启动、停止或重启
help-reload = 校验配置并热重载
help-check = 执行 sing-box check
help-update = 更新当前内核
help-mode = 循环切换 Clash 模式
help-quit = 退出
help-proxies = 代理
help-groups-nodes = 在代理组与节点间移动
help-select-node = 选择节点
help-delay-test = 测试代理组或单个节点延迟
help-connections-logs = 连接与日志
help-close-connections = 关闭单个或全部连接
help-scroll = 滚动；End 键跟随最新内容
help-store-panes = 在组件与订阅间切换
help-store-enter = 组件操作或 provider 配置
help-store-copy = 复制链接、Web 界面地址或配置
help-core = 内核
help-core-panes = 切换发布源、发行版本、已安装
help-core-switch = 切换至所选内核
help-core-variant = 下一个或上一个构建变体
help-core-download = 仅下载，不切换
help-core-delete = 删除内核或发布源
help-core-list = 仅正式版、加载更多、刷新
help-core-add-source = 添加 GitHub 发布源（需 root）
help-core-import = 导入二进制文件或 URL（需 root）
setup-title = 首次运行：可选组件
setup-intro = singbox-board 可安装并托管以下可选组件。
setup-sub-store-about =
    带 Web 界面的订阅管理工具（sub-store-org/Sub-Store）。
    可转换并合并订阅，以 sing-box 格式输出，供 sing-box 的 providers 导入节点。
setup-http-meta-about =
    按需启动 mihomo，供 Sub-Store 脚本检测节点是否可用
    （xream/http-meta 与 MetaCubeX/mihomo）。仅在配合 Sub-Store 节点检测脚本时需要。
setup-sub-store-yes = 将启用 Sub-Store。
setup-sub-store-no = 不启用 Sub-Store。
menu-start = 启动
menu-stop = 停止
menu-restart = 重启
menu-enable = 启用（下载、安装并启动）
menu-disable = 禁用（停止并保持停止）
menu-update = 更新至最新版本

## 守护进程：响应与日志

daemon-needs-root = 守护进程必须以 root 身份运行，因为 TUN、auto_route、tproxy 与 eBPF 均需要 root 权限；开发时可使用 --allow-non-root 选项
daemon-non-root = 正在以非 root 身份运行，TUN 与路由相关功能将无法使用
daemon-language-invalid = daemon.toml 中的语言设置“{ $value }”不受支持，将跟随系统区域设置
daemon-config-loaded = 已加载 { $path }
daemon-config-missing = 未找到 { $path }，将使用默认配置
daemon-listening = singbox-board 守护进程 { $version }（PID { $pid }）正在监听 { $socket }
daemon-accept-failed = 接受连接失败：{ $error }
daemon-shutdown-signal = 收到 { $signal } 信号，正在关闭
daemon-reload-failed = 热重载失败：{ $error }
daemon-already-running = 已有其他守护进程正在监听 { $socket }
daemon-stale-socket = 无法删除残留的套接字 { $socket }
daemon-bind-failed = 无法监听 { $socket }
daemon-socket-chown-failed = 无法将 { $socket } 的所属用户组设为 GID { $gid }：{ $error }
daemon-client-rejected = 已拒绝客户端连接（UID { $uid }，GID { $gid }，PID { $pid }）
daemon-permission-denied = 权限不足，请以 root 身份运行，或加入守护进程的套接字用户组
daemon-bad-request = 无效的请求：{ $error }
daemon-request = UID { $uid } 请求执行 { $request }
daemon-root-only = { $action ->
    [add-source] 添加内核发布源
    [remove-source] 移除内核发布源
   *[import] 导入自定义内核
    }需要 root 权限，因为该操作决定守护进程以 root 身份运行的程序，请使用 sudo 执行
daemon-core-stored = 已将 sing-box { $version } 存储为 { $id }（{ $checksum }）
daemon-core-deleted = 已删除 { $id }
daemon-source-added = 已添加内核发布源 { $id }（{ $name }）
daemon-source-removed = 已移除内核发布源 { $id }
daemon-unexpected-request = 无法处理的请求 { $name }
daemon-core-already-active = sing-box { $version }（{ $id }）已是当前内核
daemon-adopt-failed = 切换前无法保留当前内核：{ $error }
daemon-logs-skipped = 客户端处理速度不足，已跳过 { $count } 行日志
daemon-shutting-down = 守护进程正在关闭
auth-group-missing = 套接字用户组“{ $group }”不存在，仅 root 可以连接
auth-group-lookup-failed = 无法查询套接字用户组“{ $group }”：{ $error }

## 守护进程：sing-box 托管

supervisor-binary-missing = 未安装 { $path }，请执行 `singbox-board update` 进行安装
supervisor-config-missing = 配置文件 { $path } 不存在
supervisor-not-starting = 未启动 sing-box：{ $reason }
supervisor-waiting-components = 正在等待组件就绪，随后启动 sing-box
supervisor-auto-start-failed = 自动启动失败：{ $error }
supervisor-config-valid = 配置校验通过
supervisor-already-running = sing-box 已在运行（PID { $pid }）
supervisor-started = sing-box 已启动（PID { $pid }）
supervisor-stopped = sing-box 已停止（{ $exit }）
supervisor-restart-cancelled = 已取消自动重启
supervisor-not-running = sing-box 未运行
supervisor-check-failed-restart = 配置校验未通过，未重启 sing-box：
supervisor-check-failed-reload = 配置校验未通过，未重载 sing-box：
supervisor-check-failed-start = 配置校验未通过：
supervisor-sighup-failed = 无法向 PID { $pid } 发送 SIGHUP：{ $error }
supervisor-reloaded = 配置已重载
supervisor-reloaded-log = 配置已重载（SIGHUP）
supervisor-binary-not-found = 未找到 { $path }，请执行 `singbox-board update` 进行安装
supervisor-check-timeout = sing-box check 执行超时
supervisor-check-run-failed = 无法执行 { $path } check
supervisor-check-failed = sing-box check 执行失败（{ $exit }）
supervisor-exited-startup = sing-box 在启动过程中退出（{ $exit }）
supervisor-kill = sing-box 未在 { $seconds } 秒内退出，正在发送 SIGKILL
supervisor-exited = sing-box 意外退出（{ $exit }）
supervisor-restarting-in = 将在 { $seconds } 秒后重启 sing-box
supervisor-restart-failed = 重启失败：{ $error }
supervisor-rejects-config = { $label } 未通过当前配置校验，未执行切换；如需强制切换，请使用 --force 选项：
supervisor-switch-failed = 无法切换 { $path }：{ $error }
supervisor-switched-log = 内核已切换至 { $label }（原为 { $previous }）
supervisor-switched = 已切换至 { $label }
supervisor-switched-restarted = 已切换至 { $label }，并已重启 sing-box（PID { $pid }）
supervisor-rolled-back-log = { $label } 启动失败，已回滚至 { $previous }
supervisor-previous-running = { $label } 已恢复运行（PID { $pid }）
supervisor-previous-failed = { $label } 同样未能启动
supervisor-rolled-back = { $label } 启动失败，已恢复原内核：{ $restored }
supervisor-switched-start-failed = 已切换至 { $label }，但 sing-box 启动失败：{ $error }
supervisor-switch-back-hint = 可执行 `singbox-board core use <id>` 切换回原内核。

## 守护进程：可选组件

service-restart-failed = 重启 { $name } 失败：{ $error }
service-no-spec = { $name } 缺少启动参数
service-started = { $name } 已启动（PID { $pid }）
service-exited-startup = { $name } 在启动过程中退出（{ $exit }）
service-kill = { $name } 未在 { $seconds } 秒内退出，正在发送 SIGKILL
service-stopped = { $name } 已停止（{ $exit }）
service-exited = { $name } 意外退出（{ $exit }）
service-restarting-in = 将在 { $seconds } 秒后重启 { $name }
service-reap = 正在终止残留进程 { $path }（PID { $pid }）
components-state-reset = { $error }；将以空白的组件状态启动
components-setup-pending = 尚未配置可选组件，请在终端管理面板中回答首次运行的询问，或执行 `singbox-board setup`
components-start-failed = { $name } 启动失败：{ $error }
components-setup-log = 首次运行配置：Sub-Store { $sub_store ->
    [yes] 已启用
   *[no] 未启用
    }，http-meta { $http_meta ->
    [yes] 已启用
   *[no] 未启用
    }
components-line-error = { $name }：{ $error }
components-line-disabled = { $name }：未启用
components-disabled = { $name } 未启用，请执行 `singbox-board component { $id } enable` 启用
components-stopped = { $name } 已停止（{ $exit }）
components-not-running = { $name } 未运行
components-disabled-done = 已禁用 { $name }
components-updated = { $name } 已更新
components-install-failed = 无法安装 { $name }
components-not-ready = { $name } 已在运行，但 { $address } 未在 { $seconds } 秒内接受连接
components-user-missing = 用户“{ $user }”不存在，请设置 components.run_as
components-running = { $name } 正在运行（PID { $pid }），访问地址为 { $url }
components-state = { $state ->
    [starting] { $name } 正在启动
    [stopping] { $name } 正在停止
    [backoff] { $name } 正在等待重启
    [failed] { $name } 运行失败
    [running] { $name } 正在运行
   *[stopped] { $name } 已停止
    }
install-node-configured = 无法使用 components.node 指定的 Node.js（{ $path }）
install-node-old = { $path } 的 Node.js 版本为 { $version }，Sub-Store 已在 v24 上通过测试
install-node-index = 无法解析 Node.js 版本索引
install-node-no-lts = 没有适用于 { $platform } 的 Node.js LTS 构建，请自行安装 Node.js 并设置 components.node
install-node-downloading = 正在下载 Node.js { $version }（{ $platform }）
install-no-checksum-entry = SHASUMS256.txt 中没有 { $name } 的条目
install-node-not-runnable = 下载的 Node.js 无法在本系统上运行
install-node-installed = 已安装 Node.js { $version }
install-downloading = 正在下载 { $part ->
    [backend] Sub-Store 后端
    [frontend] Sub-Store Web 界面
    [http-meta] http-meta
   *[mihomo] mihomo
    } { $version }
install-mihomo-no-build = mihomo { $version } 没有适用于 { $arch } 的构建，请设置 http_meta.mihomo_arch
install-mihomo-decompress = 无法解压 mihomo
install-mihomo-not-runnable = 下载的 mihomo 无法在本系统上运行，请设置 http_meta.mihomo_arch
install-node-version-timeout = { $path } --version 执行超时
install-node-version-failed = { $path } --version 执行失败
install-node-version-unexpected = 无法识别的 Node.js 版本“{ $version }”
install-node-no-official = Node.js 未提供适用于 { $arch } 的官方构建，请自行安装 Node.js 并设置 components.node
install-webui-no-index = Web 界面压缩包中缺少 index.html

## 守护进程：内核版本库

cores-sources-ignored = 已忽略 { $path }：{ $error }
cores-unknown-source = 未知的内核发布源 { $id }，root 用户可执行 `singbox-board core source add { $id }` 添加
cores-invalid-repo = GitHub 仓库的格式应为 owner/name，实际为“{ $repo }”
cores-source-exists = { $repo } 已是内核发布源
cores-lookup-failed = 无法查询 { $repo }
cores-source-added-log = 已添加内核发布源 { $repo }
cores-not-custom = { $id } 不是自定义发布源，内置发布源无法移除
cores-source-removed-log = 已移除内核发布源 { $id }
cores-no-release = { $source } 中不存在发行版本 { $tag }，可使用 --source owner/repo 选择其他发布源
cores-invalid-id = 无效的内核 ID“{ $id }”
cores-no-variant = { $source } { $tag } 没有适用于 { $platform } 的 { $variant } 构建变体，可用变体：{ $available }
cores-downloading = 正在从 { $source } 下载 { $asset }（{ $size }）
cores-no-checksum = { $source } 未发布 { $asset } 的校验值，仅依赖 TLS 保证传输安全
cores-import-downloading = 正在下载自定义内核 { $location }
cores-import-location = 请指定绝对路径或 http(s) 地址
cores-adopted = 已将原有的 { $path }（{ $version }）保存为内核 { $id }
cores-not-runnable = { $name } 无法在本系统上运行，请检查架构与构建变体
cores-move-failed = 无法将内核移动至 { $path }
cores-stored-log = 已将 sing-box { $version } 存储为 { $id }（{ $size }，{ $checksum }）
cores-not-stored = 版本库中不存在内核 { $id }
cores-remove-active = { $id } 是当前内核，请先切换至其他内核
cores-deleted-log = 已删除内核 { $id }
cores-sums-not-text = { $name } 不是文本文件
cores-version-timeout = `sing-box version` 执行超时
cores-version-failed = `sing-box version` 执行失败：{ $error }
archive-too-large = { $name } 解压后超过 { $limit }
archive-read-failed = 无法读取压缩包
archive-no-binary = { $name } 中不包含 sing-box 可执行文件

## 守护进程：GitHub

github-no-asset = 发行版本 { $tag } 中没有名为 { $name } 的文件
github-invalid-proxy = update.proxy 设置无效
github-http-error = 请求 { $url } 失败，HTTP 状态为 { $status }：{ $body }
github-no-releases = { $repo } 没有发行版本
github-no-digest = GitHub 未提供 { $name } 的摘要
