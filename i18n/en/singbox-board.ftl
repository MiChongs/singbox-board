# singbox-board messages: English (reference language).
#
# Style: sentence case; complete sentences end with a period, labels and
# status lines do not. Error messages start in lower case and carry no final
# period, because they are chained ("failed to read X: permission denied").
# Lists use commas; the middle dot and the em dash are not used.

## Shared words and fragments

list-separator = {", "}
clause-separator = {", "}
chain-separator = {": "}
none = none
unknown = unknown
not-installed = not installed
answer-yes = yes
answer-no = no
error-line = error: { $message }
up-to-date = { $name } is up to date
version-prerelease = { $version } (pre-release)
prerelease-marker = pre
no-build-for-platform = no build for this platform
duration-days = { $days }d { $clock }
with-process = { $text } (PID { $pid }, running for { $uptime })
with-restart-in = { $text } (restarting in { $seconds } s)
state-pid-uptime = PID { $pid }, running for { $uptime }
clash-api-not-configured = not configured (experimental.clash_api)
ask-sub-store = Enable Sub-Store?
ask-http-meta = Enable http-meta?

## Process and component states (badges)

state-stopped = STOPPED
state-starting = STARTING
state-running = RUNNING
state-stopping = STOPPING
state-backoff = RESTART PENDING
state-failed = FAILED
state-disabled = DISABLED
busy-installing = Installing
busy-updating = Updating

## Core builds and release sources

variant-default = default
version-backend = backend
version-frontend = frontend
checksum-sums = SHA256SUMS
checksum-digest = GitHub digest
checksum-pinned = Pinned SHA-256
checksum-none = Unverified
checksum-verified-sums = verified with SHA256SUMS
checksum-verified-digest = verified with the GitHub digest
checksum-verified-pinned = verified with the given SHA-256
checksum-unverified = not verified
source-michongs = MiChongs (xiaobaf14g)
source-michongs-description = Smart, XHTTP, EasyTier and eBPF support
source-sagernet = SagerNet (official)
source-sagernet-description = Upstream releases
source-configured-description = update.repo in daemon.toml
source-custom-description = Custom source
source-custom-tag = custom
source-imported = Custom import
source-adopted = Previously installed
core-label = sing-box { $version } ({ $source }, { $variant })
log-source-daemon = daemon
entry-subscription = Subscription
entry-collection = Collection
snippet-group-comment = Reference the nodes in a group, for example:
profile-kind-local = local
profile-kind-remote = remote
profile-interval-manual = manual updates
profile-interval-hours = every { $hours } h
profile-interval-minutes = every { $minutes } min
profile-usage = { $used } of { $total } used
profile-usage-unlimited = { $used } used
profile-expires = expires { $date }
editor-none = no external editor; set $VISUAL or $EDITOR, or leave out --external to use the built-in editor
editor-start-failed = failed to start the editor { $editor }
editor-failed = the editor { $editor } exited with { $status }

## Process exit descriptions

exit-code = exit code { $code }
exit-signal = terminated by { $signal }
exit-unknown = unknown exit status
exit-wait-failed = failed to wait for the process: { $error }

## Generic errors

err-create = failed to create { $path }
err-create-workdir = failed to create the working directory { $path }
err-read = failed to read { $path }
err-parse = failed to parse { $path }
err-replace = failed to move the new file into place at { $path }
err-delete = failed to delete { $path }
err-chown = failed to change the owner of { $path }
err-not-found = { $path } does not exist
err-spawn = failed to start { $path }
err-request = the request to { $url } failed
err-decode = failed to decode the response from { $url }
err-download = failed to download { $url }
err-too-large = { $url } exceeds the size limit of { $limit }
err-checksum-mismatch = checksum mismatch for { $name }: expected { $expected }, received { $actual }
err-open-zip = failed to open the zip archive
err-archive-missing = the archive does not contain { $name }
err-task-panicked = a background task ended unexpectedly
err-sub-store-url = invalid Sub-Store address { $url }
err-clash-api-url = invalid Clash API address { $url }

## Command line: help

cli-about = Root daemon and terminal dashboard for MiChongs/sing-box
cli-long-about =
    Root daemon and terminal dashboard for MiChongs/sing-box.

    Run `singbox-board daemon` as root to supervise sing-box. Then use `singbox-board` (the terminal dashboard) or the commands below, either as root or as a member of the socket group.
cli-help-usage = Usage:
cli-help-commands = Commands:
cli-help-arguments = Arguments:
cli-help-options = Options:
cli-help = Print help
cli-version = Print version
cli-socket = Control socket of the daemon. Default: the socket set in daemon.toml, otherwise { $default }. The SINGBOX_BOARD_SOCKET environment variable has the same effect.
cli-lang = Interface language: en or zh-CN. Default: the system locale. The SINGBOX_BOARD_LANG environment variable has the same effect.
cli-lang-invalid = unsupported language { $value }; supported languages: en, zh-CN
cli-tui = Open the terminal dashboard (default)
cli-tray = Show sing-box in the system tray (KDE Plasma, GNOME with the AppIndicator extension, Waybar and other StatusNotifierItem hosts, on Wayland and X11)
cli-daemon = Run the root daemon that supervises sing-box
cli-daemon-config = Daemon configuration file
cli-daemon-allow-non-root = Allow running without root privileges (for development only)
cli-daemon-print-default-config = Print the default configuration with comments and exit
cli-status = Show the status of the daemon and sing-box
cli-status-json = Print the status as JSON
cli-start = Start sing-box
cli-stop = Stop sing-box
cli-restart = Restart sing-box
cli-reload = Validate the configuration and reload sing-box without restarting it (SIGHUP)
cli-check = Validate the sing-box configuration (sing-box check)
cli-logs = Print the output of sing-box and the daemon
cli-logs-tail = Number of buffered lines to print first (default: 200)
cli-logs-follow = Keep printing new lines
cli-update = Install or update sing-box from GitHub releases, following the source and build variant of the active core
cli-update-check = Only report whether an update is available
cli-update-tag = Install a specific release tag, for example v1.14.1-xiaobaf14g.1
cli-update-force = Reinstall even if the version is unchanged
cli-setup = Choose the optional components (Sub-Store, http-meta). This question is asked on first run.
cli-setup-sub-store = Enable Sub-Store: yes or no. Asked interactively when omitted.
cli-setup-http-meta = Enable http-meta: yes or no. Asked interactively when omitted.
cli-component = Manage an optional component. Without an action, show its details and addresses.
cli-component-name = Component: sub-store (subscription manager with a web interface) or http-meta (node availability checks for Sub-Store scripts)
cli-component-action = Action: start, stop, restart, enable (install if needed, start now and with every daemon start), disable (stop and keep stopped) or update (download the latest releases and restart)
cli-core = Manage core versions: list releases, install, switch and import custom builds
cli-core-sources = List the release sources (MiChongs, SagerNet and custom repositories)
cli-core-source = Add or remove a custom GitHub source (root only)
cli-core-source-option = Release source as owner/repo (default: the source that update follows)
cli-core-list = List the releases of a source with the builds available for this machine
cli-core-list-page = Page number, starting at 1
cli-core-list-stable = Hide pre-releases
cli-core-list-refresh = Bypass the 10-minute cache
cli-core-installed = List the cores in the local version store
cli-core-install = Download a release into the version store and switch to it
cli-core-install-tag = Release tag, for example v1.14.1-xiaobaf14g.1
cli-core-install-variant = Build variant, for example ebpf, glibc or musl (default: the standard build)
cli-core-no-switch = Only store the core; do not switch to it
cli-core-force = Switch even if the new core rejects the current configuration
cli-core-use = Switch to a stored core
cli-core-id = ID, version or release tag of a stored core
cli-core-remove = Delete a stored core
cli-core-import = Store a custom core from a local file or an HTTP(S) address (root only)
cli-core-import-location = Absolute path or URL of a binary, .tar.gz, .zip or .gz file
cli-core-import-sha256 = Expected SHA-256 checksum of the file
cli-source-add = Add a GitHub repository that publishes sing-box-<version>-linux-<arch> archives
cli-source-add-repo = Repository as owner/repo
cli-source-add-name = Display name (default: the repository)
cli-source-remove = Remove a custom source
cli-source-remove-repo = Repository of the source as owner/repo
cli-profile = Manage configuration profiles: import files or subscription URLs, create, edit and switch
cli-profile-list = List the profiles (the default)
cli-profile-add = Add a profile from a file, from standard input (-) or from a subscription URL
cli-profile-add-source = Configuration file, - for standard input, or an http(s) URL that serves a sing-box configuration
cli-profile-name = Profile name
cli-profile-interval = Minutes between automatic downloads of a remote profile; 0 downloads only on request (default: what the provider suggests, else 24 hours)
cli-profile-use-now = Switch to the profile right away
cli-profile-new = Create a profile from the built-in template
cli-profile-new-edit = Open the new profile in the editor
cli-profile-use = Switch sing-box to a profile; it is checked first and sing-box is restarted onto it
cli-profile-id = Profile ID or name
cli-profile-force-use = Switch even if sing-box rejects the profile
cli-profile-show = Print the content of a profile
cli-profile-edit = Edit a profile in the built-in editor; the active profile is checked before it is saved and sing-box is reloaded
cli-profile-edit-external = Use $VISUAL or $EDITOR instead of the built-in editor
cli-profile-force-save = Save even if sing-box rejects the configuration
cli-profile-update = Download remote profiles again
cli-profile-update-id = Profile ID or name (default: every remote profile)
cli-profile-set = Rename a profile or change its subscription URL and update interval
cli-profile-rename = New name
cli-profile-url = Subscription URL, which makes the profile a remote one
cli-profile-local = Stop downloading the profile and keep it as a local one
cli-profile-check = Run sing-box check against a profile
cli-profile-remove = Delete a profile that is not in use
cli-profile-adopt = Move the configuration file sing-box uses now into the profile store

## Command line: parse errors

cli-err-unknown-argument = unexpected argument '{ $arg }'
cli-err-invalid-subcommand = unrecognized command '{ $name }'
cli-err-invalid-value = invalid value '{ $value }' for { $arg }
cli-err-possible-values = Possible values: { $values }
cli-err-validation = invalid value '{ $value }' for { $arg }: { $reason }
cli-err-missing = the following required arguments were not provided: { $args }
cli-err-conflict = { $arg } cannot be used together with { $other }
cli-err-missing-subcommand = the command '{ $name }' requires a subcommand
cli-err-no-equals = an equals sign is required when assigning a value to { $arg }
cli-err-wrong-values = wrong number of values for { $arg }
cli-err-generic = invalid command line: { $detail }
cli-err-suggestion = Tip: did you mean '{ $suggestion }'?
cli-err-suggestions = { $kind ->
    [command] Tip: similar commands exist: { $suggestions }
    [argument] Tip: similar arguments exist: { $suggestions }
   *[value] Tip: similar values exist: { $suggestions }
    }
cli-quoted = '{ $text }'
cli-err-help-hint = For more information, use the --help option.

## Command line: output

client-daemon-not-running = the daemon is not running (no socket at { $socket }); start it with `systemctl start singbox-board` or `sudo singbox-board daemon`
client-permission-stale-group = permission denied on { $socket }: you are a member of the `{ $group }` group, but this login session started before you were added; run `newgrp { $group }` (or `sg { $group } -c singbox-board`), or log out and log in again
client-permission-denied = permission denied on { $socket }; run as root or join the socket group (`sudo usermod -aG singbox-board $USER`, then log in again)
client-connect-failed = failed to connect to { $socket }: { $error }
client-connection-closed = the daemon closed the connection
client-timeout = the daemon did not answer within { $seconds } seconds
client-unexpected-response = unexpected response from the daemon: { $response }
ctl-label-daemon = Daemon
ctl-label-version = Version
ctl-label-binary = Binary
ctl-label-args = Arguments
ctl-label-restarts = Restarts
ctl-label-last-exit = Last exit
ctl-label-update = Update
ctl-label-installed = Installed
ctl-label-web-ui = Web UI
ctl-label-endpoint = Endpoint
ctl-label-current = Installed version
ctl-label-latest = Latest version
ctl-label-asset = Release file
ctl-clash-api-secret = { $url } (secret set)
ctl-update-in-progress = in progress
ctl-setup-hint = First run: choose the optional components (Sub-Store, http-meta) with `singbox-board setup` or in the terminal dashboard.
ctl-components-intro =
    singbox-board can also install and supervise two optional components:
      Sub-Store   Subscription manager with a web interface. Converts subscriptions
                  into the sing-box format for use in `providers` (sub-store-org/Sub-Store).
      http-meta   Starts mihomo on demand so that Sub-Store scripts can test whether
                  nodes are reachable (xream/http-meta).
    Both components run as an unprivileged user. Node.js is downloaded automatically if it is not installed.
ctl-ask-no-terminal = cannot ask "{ $question }" without a terminal; pass --sub-store and --http-meta with yes or no
ctl-ask-no-answer = no answer was given
ctl-ask-retry = Please answer y or n.
ctl-setup-installing = Installing and starting the selected components. This may take a while.
ctl-may-download = This may download files and take a while.
ctl-component-unknown = the daemon does not report the status of { $component }
ctl-subscriptions-error = The subscriptions cannot be listed: { $error }
ctl-subscriptions-empty = There are no subscriptions yet. Add them in the web UI.
ctl-subscriptions-title = sing-box subscription URLs:
ctl-provider-title = Provider for sing-box (add it to your configuration):
ctl-latest = { $version } (release date: { $date })
ctl-latest-prerelease = { $version } (pre-release, release date: { $date })
ctl-update-available = An update is available. Run `singbox-board update` to install it.
ctl-up-to-date = sing-box is up to date.
ctl-update-downloading = Downloading and verifying the release. This may take a while.
ctl-importing-core = Storing the custom core. This may take a while.
ctl-core-unmanaged = sing-box { $version } at { $binary } (not managed by the version store)
ctl-core-none = No sing-box core is installed.
ctl-core-hint-list = List releases: singbox-board core list
ctl-core-hint-switch = Switch cores: singbox-board core install <tag>, or singbox-board core use <id>
ctl-releases-title = { $source }, { $platform }, page { $page }
ctl-releases-title-more = { $source }, { $platform }, page { $page } (next page: --page { $next })
ctl-col-version = VERSION
ctl-col-published = PUBLISHED
ctl-col-variants = VARIANTS (● active, ✓ stored)
ctl-prerelease-marker = pre
ctl-no-build = no build for this platform
ctl-install-hint = Install: singbox-board core install <tag> --source { $source } [--variant <name>]
ctl-store-empty = The version store is empty.
ctl-store-title = Installed cores:
ctl-core-not-found = no stored core matches "{ $query }"; run `singbox-board core installed` to list the stored cores
ctl-core-ambiguous = "{ $query }" matches more than one core; specify one of the following IDs:
ctl-core-downloading = Downloading { $source } { $tag } ({ $variant }) unless it is already stored.
ctl-label-profile = Profile
ctl-profile-unmanaged-short = not managed by the profile store
ctl-profile-unmanaged = sing-box uses { $path }, which is not in the profile store yet. Run `singbox-board profile adopt` to move it in; switching to a profile does this as well.
ctl-profiles-empty = There are no profiles yet.
ctl-profile-last-error = Last download failed: { $error }
ctl-profile-hint-add = Add: singbox-board profile add <file|url>, or singbox-board profile new <name>
ctl-profile-hint-use = Switch: singbox-board profile use <name>; edit: singbox-board profile edit <name>
ctl-profile-downloading = Downloading. This may take a while.
ctl-profile-interval-local = --interval only applies to subscription URLs
ctl-profile-no-changes = No changes were made.
ctl-profile-edit-again = Edit again?
ctl-profile-discarded = the changes were discarded
ctl-profile-edit-needs-terminal = the built-in editor needs a terminal; use --external with $VISUAL or $EDITOR
ctl-profile-nothing-to-set = nothing to change; pass --name, --url, --interval or --local

## Terminal dashboard: tabs, panels and columns

tab-overview = Overview
tab-proxies = Proxies
tab-connections = Connections
tab-logs = Logs
tab-core = Core
tui-panel-recent-logs = Recent logs
tui-panel-traffic = Traffic
tui-panel-groups = Groups
tui-panel-groups-count = Groups ({ $count })
tui-panel-connections = Connections ({ $count })
tui-panel-connections-filtered = Connections ({ $shown } of { $count })
tui-panel-connection-details = Connection details
tui-panel-logs = Logs ({ $count })
tui-panel-components = Components
tui-panel-subscriptions = sing-box subscriptions
tui-panel-subscriptions-version = sing-box subscriptions (Sub-Store { $version })
tui-panel-confirm = Confirm
tui-panel-active-core = Active core
tui-panel-sources = Sources
tui-panel-releases-empty = Releases
tui-panel-releases = Releases of { $source } ({ $platform })
tui-panel-releases-stable = Releases of { $source } ({ $platform }, stable only)
tui-panel-installed = Installed ({ $count })
field-state = State
field-core = Core
field-binary = Binary
field-restarts = Restarts
field-last-exit = Last exit
field-daemon = Daemon
field-error = Error
field-mode = Mode
field-connections = Conns
field-memory = Memory
col-name = Name
col-type = Type
col-delay = Delay
col-destination = Destination
col-network = Net
col-chain = Chain
col-rule = Rule
col-age = Age
col-process = Process
col-rate = Rate
col-traffic = Total
col-component = Component
col-state = State
col-versions = Versions
col-url = URL
col-source = Source
col-singbox-url = sing-box URL
col-version = Version
col-published = Published
col-variant = Variant
col-size = Size
col-checksum = Checksum
col-installed = Installed

## Terminal dashboard: key hints

key-focus = Focus
key-select = Select
key-test = Test
key-test-one = Test node
key-move = Move
key-close = Close
key-close-all = Close all
key-close-shown = Close shown
key-details = Details
key-hide = Hide details
key-filter = Filter
key-sort = Sort
key-pause = Pause
key-resume = Resume
key-copy-target = Copy target
key-copy = Copy
key-scroll = Scroll
key-page = Page
key-follow = Follow
key-actions = Actions
key-copy-url = Copy URL
key-web-ui = Web UI
key-provider-snippet = Provider snippet
key-start = Start
key-stop = Stop
key-restart = Restart
key-reload = Reload
key-update = Update
key-mode = Mode
key-help = Help
key-pane = Pane
key-source = Source
key-add = Add
key-remove = Remove
key-import = Import
key-switch = Switch
key-variant = Variant
key-download = Download
key-stable = Stable
key-more = More
key-refresh = Refresh
key-delete = Delete
key-next = Next
key-confirm = Confirm
key-cancel = Cancel
key-submit = Submit
key-clear = Clear
key-yes = Yes
key-no = No
key-ask-later = Ask later

## Terminal dashboard: status text

tui-connecting = Connecting to the daemon
tui-waiting-daemon = Waiting for the daemon
tui-header-daemon = Daemon { $version }, PID { $pid }
tui-uptime = running for { $uptime }
tui-restart-in = Restarting in { $seconds } s
tui-core-not-installed = Not installed; see the Core tab
tui-downloading = Downloading
tui-daemon-detail = { $version }, running for { $uptime }, { $socket }
tui-traffic-total = total { $bytes }
tui-chart-now = now
tui-chart-collecting = Collecting traffic samples…
tui-testing = Testing
tui-delay-timeout = Timeout
tui-logs-following = Following
tui-logs-scrolled = { $lines ->
    [one] Scrolled up 1 line; press End to follow
   *[other] Scrolled up { $lines } lines; press End to follow
    }
tui-logs-disconnected = Disconnected
tui-sub-store-disabled = Sub-Store is disabled. Select it above, press Enter and choose Enable.
tui-sub-store-stopped = Sub-Store is not running.
tui-no-subscriptions = There are no subscriptions yet. Add them in the web UI; press w to copy its address.
tui-loading = Loading
tui-active = Active
tui-stored = Stored
tui-no-build = No build
tui-unmanaged = Unmanaged
tui-unmanaged-note = { $binary } is a regular file. Switching keeps it in the version store as "{ $name }".
tui-no-core = NO CORE
tui-no-core-hint = Select a release below and press Enter to install it and switch to it.
tui-installed-on = installed on { $date }
tui-file-count = { $count ->
    [one] 1 file
   *[other] { $count } files
    }
tui-linked-at = linked at { $path }
tui-releases-shown = { $count } shown
tui-releases-shown-more = { $count } shown; press n for more
tui-variants = Variants
tui-loading-releases = Loading releases
tui-no-releases = No releases
tui-on-disk = { $size } on disk
tui-store-empty = The version store is empty. Releases that you switch to or download with i appear here.

## Terminal dashboard: the Connections tab

conn-card-connections = Connections
conn-card-download = Download
conn-card-upload = Upload
conn-card-outbounds = Outbounds
conn-card-busiest = Busiest
conn-memory = memory { $bytes }
conn-active = { $count ->
    [one] 1 transferring
   *[other] { $count } transferring
    }
conn-idle = All idle
conn-sort-label = Sort: { $key }
conn-sort-newest = newest
conn-sort-rate = rate
conn-sort-traffic = traffic
conn-sort-host = host
conn-filter-label = Filter: { $filter }
conn-paused = PAUSED
conn-sniffed = sniffed
conn-inbound = inbound
conn-lasted = open for { $age }
conn-field-target = Target
conn-field-address = Address
conn-field-source = Source
conn-field-process = Process
conn-field-chain = Chain
conn-field-rule = Rule
conn-field-started = Started
conn-field-traffic = Traffic
conn-empty = No active connections
conn-empty-hint = Traffic that passes through sing-box shows up here as it happens.
conn-no-match = No connection matches "{ $filter }"
conn-no-match-hint = Press / to change the filter or Esc to clear it.
conn-no-api = The Clash API is not enabled
conn-no-api-hint =
    Connections are read from the Clash API. Add experimental.clash_api to the profile,
    for example "external_controller": "127.0.0.1:9090", then press R to reload.
conn-not-running = sing-box is not running
conn-not-running-hint = Press s to start it; its connections show up here.
conn-api-error = The Clash API cannot be reached
conn-api-error-hint = Retrying every second.
conn-filter-title = Filter connections
conn-filter-hint = Matches the target, addresses, process, chain, rule and inbound as you type. Separate words to require all of them; leave it empty to show everything.
conn-filter-placeholder = e.g. google udp

## Terminal dashboard: notifications and progress

tui-title-error = Error
tui-title-result = Result
tui-confirm-stop = Stop sing-box?
tui-confirm-restart = Restart sing-box?
tui-confirm-close-all = { $count ->
    [one] Close the only connection?
   *[other] Close all { $count } connections?
    }
tui-confirm-close-filtered = { $count ->
    [one] Close the connection that matches "{ $filter }"?
   *[other] Close the { $count } connections that match "{ $filter }"?
    }
tui-close-connections-note = Applications reconnect on their own when they need to.
tui-confirm-update =
    Install sing-box { $version }?
    Installed version: { $current }
tui-confirm-switch = Switch to sing-box { $version }?
tui-confirm-download = Download sing-box { $version }?
tui-confirm-delete = Delete the stored core sing-box { $version }?
tui-confirm-remove-source = Remove the core source { $source }?
tui-detail-build = { $source }, { $variant } build
tui-detail-download = Download size: { $size }; verification: { $checksum }
tui-detail-stored = Already in the version store; verification: { $checksum }
tui-detail-frees = This frees { $size }.
tui-switch-note = The configuration is checked first; sing-box then restarts on the new core.
tui-remove-source-note = Cores already stored from this source are kept.
tui-no-build-for = { $version } has no build for { $platform }
tui-already-active = { $version } is already the active core
tui-active-not-deletable = The active core cannot be deleted; switch to another core first
tui-builtin-source = { $name } is a built-in source and cannot be removed
tui-add-source-title = Add a core source
tui-add-source-hint = A GitHub repository that publishes sing-box-<version>-linux-<arch>.tar.gz (root only)
tui-import-title = Import a custom core
tui-import-hint = Absolute path or http(s) URL of a binary, .tar.gz, .zip or .gz file, optionally followed by its SHA-256 (root only)
tui-copied-snippet = Copied the provider snippet
tui-copied-subscription-url = Copied the sing-box subscription URL
tui-copied-url = Copied the address
tui-copied-web-ui = Copied the Sub-Store web UI address
tui-no-url = No address is available yet; enable the component first
tui-sub-store-not-set-up = Sub-Store is not set up
tui-status-unavailable = The daemon status is not available
tui-no-subscription-selected = No subscription is selected
tui-snippet-title = sing-box provider for { $name }
tui-connections-closed = All connections have been closed
tui-connection-closed = Closed the connection to { $target }
tui-connections-closed-count = { $count ->
    [one] Closed 1 connection
   *[other] Closed { $count } connections
    }
tui-copied-target = Copied { $target }
tui-clash-api-missing = The Clash API is not configured (experimental.clash_api)
tui-mode-list-unavailable = The mode list is not available yet
tui-single-mode = Only one Clash mode is configured
tui-mode-set = Switched to the { $mode } mode
tui-mouse-on = Mouse on: click, double click, right click, wheel
tui-mouse-off = Mouse off: the terminal selects text; M turns it back on
tui-not-selectable = The group { $group } does not accept manual selection
tui-node-selected = { $group } now uses { $node }
busy-starting = Starting sing-box
busy-stopping = Stopping sing-box
busy-restarting = Restarting sing-box
busy-reloading = Reloading the configuration
busy-checking = Checking the configuration
busy-downloading = Downloading sing-box
busy-checking-updates = Checking for updates
busy-setup = Setting up the components
busy-component = { $action ->
    [start] Starting { $component }
    [stop] Stopping { $component }
    [restart] Restarting { $component }
    [enable] Installing { $component }
    [disable] Disabling { $component }
   *[update] Updating { $component }
    }
busy-closing-connections = Closing the connections
busy-closing-connection = Closing the connection
busy-switching-mode = Switching the mode
busy-selecting-node = Selecting the node
busy-adding-source = Adding the core source
busy-importing-core = Importing the core
busy-switching-core = Switching the core
busy-downloading-core = Downloading the core
busy-deleting-core = Deleting the core
busy-removing-source = Removing the source

## Terminal dashboard: popups

help-title = Keyboard shortcuts
help-close-hint = Press any key or click to close
help-global = Global
help-switch-tab = Switch tab
help-start-stop-restart = Start, stop or restart
help-reload = Check and reload the config
help-check = Run sing-box check
help-update = Update the active core
help-mode = Cycle the Clash mode
help-quit = Quit
help-mouse = Mouse
help-mouse-click-key = Click
help-mouse-click = Tabs, hints, buttons; selects rows
help-mouse-double-key = Double/right
help-mouse-double = Opens the row (Enter) or its menu
help-mouse-wheel-key = Wheel/drag
help-mouse-wheel = Scrolls; drag a scrollbar to jump
help-mouse-toggle = Mouse on/off; Shift+drag to select
help-proxies = Proxies
help-groups-nodes = Groups and nodes
help-select-node = Select the node
help-delay-test = Test the group or the node
help-connections = Connections
help-conn-details = Show or hide the details
help-conn-filter = Filter as you type; Esc clears it
help-conn-sort = Next sort order; reverse it
help-conn-pause = Pause or resume updates
help-conn-copy = Copy the target
help-close-connections = Close one, or all shown
help-logs = Logs
help-scroll = Scroll; End follows
help-store-panes = Components or subscriptions
help-store-enter = Actions or provider snippet
help-store-copy = Copy URL, web UI or snippet
help-core = Core
help-core-panes = Sources, releases, installed
help-core-switch = Switch to the selection
help-core-variant = Next or previous variant
help-core-download = Download without switching
help-core-delete = Delete a core or a source
help-core-list = Stable only, more, refresh
help-core-add-source = Add a GitHub source (root)
help-core-import = Import a binary or URL (root)
setup-title = First run: optional components
setup-intro = singbox-board can install and supervise the following optional components.
setup-sub-store-about =
    Subscription manager with a web interface (sub-store-org/Sub-Store).
    It converts and merges subscriptions and serves them in the sing-box
    format, so that sing-box providers can import the nodes.
setup-http-meta-about =
    Starts mihomo on demand so that Sub-Store scripts can test whether nodes
    are reachable (xream/http-meta with MetaCubeX/mihomo). It is only needed
    together with Sub-Store node check scripts.
setup-sub-store-yes = Sub-Store will be enabled.
setup-sub-store-no = Sub-Store will not be enabled.
menu-start = Start
menu-stop = Stop
menu-restart = Restart
menu-enable = Enable (download, install and start)
menu-disable = Disable (stop and keep stopped)
menu-update = Update to the latest release

## Daemon: replies and log

daemon-needs-root = the daemon must run as root, because TUN, auto_route, tproxy and eBPF require it; pass --allow-non-root for development
daemon-non-root = Running without root privileges; TUN and routing features will fail
daemon-language-invalid = Unsupported language "{ $value }" in daemon.toml; the locale is used instead
daemon-config-loaded = Loaded { $path }
daemon-config-missing = { $path } was not found; using the default configuration
daemon-listening = singbox-board daemon { $version } (PID { $pid }) is listening on { $socket }
daemon-accept-failed = Failed to accept a connection: { $error }
daemon-shutdown-signal = Received { $signal }; shutting down
daemon-reload-failed = Reload failed: { $error }
daemon-already-running = another daemon is already listening on { $socket }
daemon-stale-socket = failed to remove the stale socket { $socket }
daemon-bind-failed = failed to listen on { $socket }
daemon-socket-chown-failed = Failed to assign { $socket } to group ID { $gid }: { $error }
daemon-client-rejected = Rejected a client (UID { $uid }, GID { $gid }, PID { $pid })
daemon-permission-denied = permission denied; run as root or join the socket group of the daemon
daemon-bad-request = invalid request: { $error }
daemon-request = UID { $uid } requested { $request }
daemon-root-only = { $action ->
    [add-source] adding a core source
    [remove-source] removing a core source
   *[import] importing a custom core
    } requires root privileges, because it decides which binary the daemon runs as root; use sudo
daemon-core-stored = Stored sing-box { $version } as { $id } ({ $checksum })
daemon-core-deleted = Deleted { $id }
daemon-source-added = Added the core source { $id } ({ $name })
daemon-source-removed = Removed the core source { $id }
daemon-unexpected-request = unexpected request { $name }
daemon-core-already-active = sing-box { $version } ({ $id }) is already the active core
daemon-adopt-failed = failed to keep the current core before switching: { $error }
daemon-logs-skipped = { $count ->
    [one] 1 log line was skipped because the client could not keep up
   *[other] { $count } log lines were skipped because the client could not keep up
    }
daemon-shutting-down = the daemon is shutting down
auth-group-missing = The socket group "{ $group }" does not exist; only root can connect
auth-group-lookup-failed = Failed to look up the socket group "{ $group }": { $error }

## Daemon: sing-box supervision

supervisor-binary-missing = { $path } is not installed; run `singbox-board update` to install it
supervisor-config-missing = the configuration file { $path } does not exist
supervisor-not-starting = sing-box is not started: { $reason }
supervisor-waiting-components = Waiting for the components before starting sing-box
supervisor-auto-start-failed = Automatic start failed: { $error }
supervisor-config-valid = The configuration is valid
supervisor-already-running = sing-box is already running (PID { $pid })
supervisor-started = sing-box started (PID { $pid })
supervisor-stopped = sing-box stopped ({ $exit })
supervisor-restart-cancelled = The automatic restart was cancelled
supervisor-not-running = sing-box is not running
supervisor-check-failed-restart = the configuration check failed; sing-box was not restarted:
supervisor-check-failed-reload = the configuration check failed; sing-box was not reloaded:
supervisor-check-failed-start = the configuration check failed:
supervisor-sighup-failed = failed to send SIGHUP to PID { $pid }: { $error }
supervisor-reloaded = The configuration has been reloaded
supervisor-reloaded-log = The configuration has been reloaded (SIGHUP)
supervisor-binary-not-found = { $path } was not found; install it with `singbox-board update`
supervisor-check-timeout = sing-box check timed out
supervisor-check-run-failed = failed to run { $path } check
supervisor-check-failed = sing-box check failed ({ $exit })
supervisor-exited-startup = sing-box exited during startup ({ $exit })
supervisor-kill = sing-box did not exit within { $seconds } seconds; sending SIGKILL
supervisor-exited = sing-box exited unexpectedly ({ $exit })
supervisor-restarting-in = Restarting sing-box in { $seconds } seconds
supervisor-restart-failed = Restart failed: { $error }
supervisor-rejects-config = { $label } rejects the current configuration, so the core was not switched; use --force to switch anyway:
supervisor-switch-failed = failed to switch { $path }: { $error }
supervisor-switched-log = Switched the core to { $label } (previously { $previous })
supervisor-switched = Switched to { $label }
supervisor-switched-restarted = Switched to { $label } and restarted sing-box (PID { $pid })
supervisor-rolled-back-log = { $label } failed to start; rolled back to { $previous }
supervisor-previous-running = { $label } is running again (PID { $pid })
supervisor-previous-failed = { $label } did not start either
supervisor-rolled-back = { $label } failed to start, and the previous core was restored: { $restored }
supervisor-switched-start-failed = switched to { $label }, but sing-box failed to start: { $error }
supervisor-switch-back-hint = Switch back with `singbox-board core use <id>`.

## Daemon: optional components

service-restart-failed = Restarting { $name } failed: { $error }
service-no-spec = { $name } has no launch specification
service-started = { $name } started (PID { $pid })
service-exited-startup = { $name } exited during startup ({ $exit })
service-kill = { $name } did not exit within { $seconds } seconds; sending SIGKILL
service-stopped = { $name } stopped ({ $exit })
service-exited = { $name } exited unexpectedly ({ $exit })
service-restarting-in = Restarting { $name } in { $seconds } seconds
service-reap = Terminating the leftover process { $path } (PID { $pid })
components-state-reset = { $error }; starting with an empty component state
components-setup-pending = The optional components are not configured yet; answer the first-run question in the terminal dashboard or run `singbox-board setup`
components-start-failed = { $name } failed to start: { $error }
components-setup-log = First-run setup: Sub-Store { $sub_store ->
    [yes] enabled
   *[no] disabled
    }, http-meta { $http_meta ->
    [yes] enabled
   *[no] disabled
    }
components-line-error = { $name }: { $error }
components-line-disabled = { $name }: not enabled
components-disabled = { $name } is disabled; enable it with `singbox-board component { $id } enable`
components-stopped = { $name } stopped ({ $exit })
components-not-running = { $name } is not running
components-disabled-done = { $name } has been disabled
components-updated = { $name } has been updated
components-install-failed = failed to install { $name }
components-not-ready = { $name } is running, but { $address } did not accept connections within { $seconds } seconds
components-user-missing = the user "{ $user }" does not exist; set components.run_as
components-running = { $name } is running (PID { $pid }) at { $url }
components-state = { $state ->
    [starting] { $name } is starting
    [stopping] { $name } is stopping
    [backoff] { $name } is waiting to restart
    [failed] { $name } has failed
    [running] { $name } is running
   *[stopped] { $name } is stopped
    }
install-node-configured = the Node.js set in components.node ({ $path }) cannot be used
install-node-old = { $path } is Node.js { $version }; Sub-Store is tested with v24
install-node-index = failed to parse the Node.js release index
install-node-no-lts = no Node.js LTS build is available for { $platform }; install Node.js and set components.node
install-node-downloading = Downloading Node.js { $version } ({ $platform })
install-no-checksum-entry = SHASUMS256.txt has no entry for { $name }
install-node-not-runnable = the downloaded Node.js does not run on this system
install-node-installed = Installed Node.js { $version }
install-downloading = Downloading { $part ->
    [backend] the Sub-Store backend
    [frontend] the Sub-Store web UI
    [http-meta] http-meta
   *[mihomo] mihomo
    } { $version }
install-mihomo-no-build = mihomo { $version } has no build for { $arch }; set http_meta.mihomo_arch
install-mihomo-decompress = failed to decompress mihomo
install-mihomo-not-runnable = the downloaded mihomo does not run on this system; set http_meta.mihomo_arch
install-node-version-timeout = { $path } --version timed out
install-node-version-failed = { $path } --version failed
install-node-version-unexpected = unexpected Node.js version "{ $version }"
install-node-no-official = there is no official Node.js build for { $arch }; install Node.js and set components.node
install-webui-no-index = the web UI archive has no index.html

## Daemon: core version store

cores-sources-ignored = Ignoring { $path }: { $error }
cores-unknown-source = unknown core source { $id }; root can add it with `singbox-board core source add { $id }`
cores-invalid-repo = expected a GitHub repository in the form owner/name, received "{ $repo }"
cores-source-exists = { $repo } is already a core source
cores-lookup-failed = failed to look up { $repo }
cores-source-added-log = Added the core source { $repo }
cores-not-custom = { $id } is not a custom source; built-in sources cannot be removed
cores-source-removed-log = Removed the core source { $id }
cores-no-release = { $source } has no release { $tag }; choose another source with --source owner/repo
cores-invalid-id = invalid core ID "{ $id }"
cores-no-variant = { $source } { $tag } has no { $platform } build of the { $variant } variant; available variants: { $available }
cores-downloading = Downloading { $asset } ({ $size }) from { $source }
cores-no-checksum = { $source } publishes no checksum for { $asset }; relying on TLS only
cores-import-downloading = Downloading the custom core { $location }
cores-import-location = specify an absolute path or an http(s) URL
cores-adopted = Kept the previous { $path } ({ $version }) as the core { $id }
cores-not-runnable = { $name } does not run on this system; check the architecture and the build variant
cores-move-failed = failed to move the core into { $path }
cores-stored-log = Stored sing-box { $version } as { $id } ({ $size }, { $checksum })
cores-not-stored = no stored core { $id }
cores-remove-active = { $id } is the active core; switch to another core first
cores-deleted-log = Deleted the core { $id }
cores-sums-not-text = { $name } is not a text file
cores-version-timeout = `sing-box version` timed out
cores-version-failed = `sing-box version` failed: { $error }
archive-too-large = { $name } unpacks to more than { $limit }
archive-read-failed = failed to read the archive
archive-no-binary = { $name } does not contain a sing-box executable

## Daemon: GitHub

github-no-asset = the release { $tag } has no file named { $name }
github-invalid-proxy = invalid update.proxy
github-http-error = the request to { $url } failed with HTTP { $status }: { $body }
github-no-releases = { $repo } has no releases
github-no-digest = GitHub published no digest for { $name }

## Configuration documents

profile-empty = the configuration is empty
profile-invalid-json = not valid JSON: { $error }
profile-not-object = the configuration must be a JSON object
profile-node-list = this is a node list, not a sing-box configuration; ask the provider for the sing-box format or convert it with Sub-Store

## Daemon: configuration profiles

profiles-index-ignored = Ignoring the unreadable profile index { $path }: { $error }
profiles-not-found = no profile matches "{ $query }"
profiles-ambiguous = "{ $query }" matches several profiles:
profiles-content-and-url = give either content or a URL, not both
profiles-default-name = Profile { $number }
profiles-adopted-name = Original configuration
profiles-added = Added profile "{ $name }" ({ $id })
profiles-added-log = Added profile "{ $name }" ({ $id })
profiles-saved = Saved "{ $name }"
profiles-unchanged = "{ $name }" is unchanged
profiles-rejected = sing-box rejects the new configuration, so the active profile was not changed; use --force to save anyway
profiles-check-ok = sing-box check passed.
profiles-check-warning = sing-box check reports problems; fix them before switching to this profile: { $error }
profiles-check-skipped = sing-box is not installed, so the configuration was not checked.
profiles-check-passed = "{ $name }" passes sing-box check
profiles-check-failed = "{ $name }" fails sing-box check
profiles-not-running = sing-box is not running; the change takes effect when it starts.
profiles-reloaded = sing-box has been reloaded.
profiles-reload-failed = Reloading sing-box failed: { $error }
profiles-name-empty = the name must not be empty
profiles-name-taken = another profile is already named "{ $name }"
profiles-not-remote = "{ $name }" is not a remote profile
profiles-settings-saved = Saved the settings of "{ $name }"
profiles-remove-active = "{ $name }" is in use; switch to another profile before deleting it
profiles-removed = Deleted "{ $name }"
profiles-removed-log = Deleted profile "{ $name }" ({ $id })
profiles-updated = Updated "{ $name }" ({ $size })
profiles-update-unchanged = "{ $name }" is up to date
profiles-updated-log = Downloaded profile "{ $name }" ({ $size })
profiles-update-failed = "{ $name }": { $error }
profiles-no-remote = there are no remote profiles
profiles-auto-update-failed = Automatic update of "{ $name }" failed: { $error }
profiles-not-text = { $path } is not a text file
profiles-too-large = the configuration exceeds the size limit of { $limit }
profiles-link-failed = failed to link { $path } to the profile store
profiles-adopted = Moved the current configuration into the store as "{ $name }" ({ $id })
profiles-adopted-log = Moved { $path } into the profile store as "{ $name }" ({ $id })
profiles-nothing-to-adopt = The configuration file is already managed, or there is none
profiles-already-active = "{ $name }" is already in use
profiles-invalid-url = { $url } is not an http(s) URL
profiles-invalid-proxy = invalid profiles.proxy
profiles-http-error = { $url } answered { $status }

## Daemon: switching profiles

supervisor-no-config-slot = core.config in daemon.toml lists no configuration file to switch
supervisor-rejects-profile = sing-box rejects "{ $label }", so the profile was not switched; use --force to switch anyway:
supervisor-profile-switched-log = Switched the configuration to "{ $label }"
supervisor-profile-switched = Switched to "{ $label }"
supervisor-profile-restarted = Switched to "{ $label }" and restarted sing-box (PID { $pid })
supervisor-profile-started = Switched to "{ $label }" and started sing-box (PID { $pid })
supervisor-profile-rolled-back-log = "{ $label }" failed to start; switched back to "{ $previous }"
supervisor-previous-profile-running = "{ $label }" is running again (PID { $pid })
supervisor-previous-profile-failed = "{ $label }" did not start either
supervisor-profile-rolled-back = "{ $label }" failed to start, and the previous profile was restored: { $restored }
supervisor-profile-start-failed = switched to "{ $label }", but sing-box failed to start: { $error }

## Terminal dashboard: profiles

tab-profiles = Profiles
field-profile = Profile
tui-profile-unmanaged = not managed
tui-panel-profiles = Profiles ({ $count })
tui-panel-summary = Overview
tui-panel-profile-details = Details
tui-panel-node = Selected item
col-updated = Updated
col-subscription = Subscription
key-new = New
key-edit = Edit
key-update-profile = Update
key-navigate = Navigate
key-reorder = Reorder
key-undo = Undo
key-search = Search
key-save = Save
key-close-editor = Close
busy-saving = Saving
busy-saving-reloading = Saving and reloading sing-box
busy-switching-profile = Switching the profile
busy-updating-profile = Updating the profile
busy-updating-profiles = Updating the remote profiles
busy-importing-profile = Importing the profile
busy-duplicating = Duplicating the profile
busy-deleting = Deleting the profile
tui-profiles-empty = No profiles yet. Press n to create one from the template, or i to import a file or a subscription URL.
tui-profiles-hint = Select a profile to see what it contains.
tui-profiles-unmanaged = sing-box uses { $path }, which is not in the profile store yet. Press A to move it in; switching profiles does this as well.
tui-profile-in-use = in use
tui-profile-not-in-use = not in use
tui-ago-now = just now
tui-ago-minutes = { $minutes } min ago
tui-ago-hours = { $hours } h ago
detail-state = State
detail-created = Created
detail-updated = Changed
detail-url = URL
detail-interval = Interval
detail-fetched = Downloaded
detail-usage = Traffic
detail-last-error = Error
summary-inbounds = Inbounds
summary-outbounds = Outbounds
summary-outbound-count = { $count } ({ $types })
summary-groups = Groups
summary-endpoints = Endpoints
summary-providers = Providers
summary-dns = DNS
summary-final = final { $target }
summary-route = Route
summary-route-detail = { $rules } rules, { $sets } rule sets, final { $target }
summary-log = Log
summary-no-clash-api = not configured; the dashboard needs it for proxies and connections
summary-has-comments = The file has comments. The editor keeps them; changes made in the tree view or by formatting remove them.
menu-profile-use = Use this profile
menu-profile-use-detail = check, switch, restart sing-box
menu-profile-edit = Edit
menu-profile-edit-external = Edit in $EDITOR
menu-profile-update = Update now
menu-profile-rename = Rename
menu-profile-url = Subscription URL
menu-profile-interval = Update interval
menu-profile-make-local = Stop updating
menu-profile-make-local-detail = keep as a local profile
menu-profile-copy-url = Copy the subscription URL
menu-profile-duplicate = Duplicate
menu-profile-check = Check with sing-box
menu-profile-export = Export to a file
menu-profile-delete = Delete
tui-confirm-use-profile = Switch sing-box to "{ $name }"?
tui-use-adopts-note = The current configuration file is moved into the store first.
tui-use-profile-note = sing-box checks it first; if it fails to start, the previous profile is restored.
tui-confirm-delete-profile = Delete the profile "{ $name }"?
tui-confirm-adopt = Move the current configuration file into the profile store? sing-box keeps running unchanged.
tui-new-profile-title = New profile
tui-new-profile-hint = The template has a local proxy port, a selector for your nodes, DNS and the Clash API. It opens in the editor.
tui-new-profile-placeholder = Name (optional)
tui-import-profile-title = Import a profile
tui-import-profile-hint = A sing-box configuration file, or an http(s) subscription URL that serves one. Remote profiles are updated automatically.
tui-import-empty = enter a path or a URL
tui-rename-profile-title = Rename the profile
tui-profile-url-title = Subscription URL
tui-profile-url-hint = An http(s) URL that serves a sing-box configuration. Leave it empty to keep the profile local.
tui-profile-interval-title = Update interval
tui-profile-interval-hint = Minutes between automatic downloads; 0 downloads only on request.
tui-interval-invalid = enter whole minutes, 0 for manual updates
tui-export-title = Export the profile
tui-export-hint = The file is written with your own permissions.
tui-exported = Exported to { $path }
tui-profile-copy-name = { $name } copy
tui-copied-profile-url = Copied the subscription URL
tui-save-failed-title = Not saved
tui-save-anyway = Save anyway
tui-save-anyway-detail = sing-box may fail to start with it
tui-keep-editing = Keep editing
tui-edit-again = Edit again
tui-discard-changes = Discard the changes
tui-invalid-edit-title = The edit is not valid
tui-editor-title = Editing { $name }
tui-editor-modified = (modified)
tui-editor-empty = The document is empty; press a to add a section.
tui-reference-hint = ⏎ picks one of the existing tags
tui-edit-value-title = Edit the value
tui-rename-key-title = Rename the key
tui-new-key-title = New key
tui-new-key-hint = Name of the new member
tui-custom-key = Other key…
tui-type-value = Type a value…
tui-value-type-title = Value of { $key }
tui-add-title = Add to { $path }
tui-add-title-root = Add a section
tui-node-deleted = Deleted { $path }; press u to undo
tui-nothing-to-undo = Nothing to undo
tui-nothing-to-redo = Nothing to redo
tui-copied-node = Copied as JSON
tui-search-title = Search
tui-search-hint = Matches keys and values
tui-search-none = nothing matches
node-object = Object with { $count } { $count ->
        [one] member
       *[other] members
    }
node-array = List with { $count } { $count ->
        [one] item
       *[other] items
    }
node-string = Text
node-number = Number
node-bool = Switch (true or false)
editor-key-empty = the key must not be empty
editor-key-taken = "{ $key }" already exists here
editor-node-gone = the item no longer exists
editor-not-a-bool = enter true or false
editor-not-a-member = only object members have a name
editor-not-a-number = enter a number
editor-root-locked = the document itself cannot be moved or deleted
help-profiles = Profiles
help-profiles-menu = All actions, including switching
help-profiles-edit = Edit in the built-in editor (E: $EDITOR)
help-profiles-new = New from the template, import a file or URL
help-profiles-update = Update one or all remote profiles
help-profiles-delete = Delete, adopt the current file
help-editor-title = Tree view
help-editor-move = Navigate
help-editor-cursor = Move
help-editor-fold = Collapse or expand
help-editor-toggle = Toggle, expand all, collapse all
help-editor-search = Search, next, previous
help-editor-file = Document
help-editor-save = Check and save
help-editor-undo = Undo or redo
help-editor-close = Back to the text editor
help-editor-change = Change
help-editor-edit = Edit the value, or pick a tag
help-editor-json = Edit the item as text
help-editor-add = Add after, or inside
help-editor-rename = Rename the key
help-editor-delete = Delete (u undoes)
help-editor-duplicate = Duplicate
help-editor-reorder = Move up or down
help-editor-copy = Copy as JSON
tui-tree-view = tree view
tui-tree-comments = Changes here rewrite the text without comments; Ctrl+Z in the text undoes that
tui-no-external-editor = Set $VISUAL or $EDITOR to use an external editor; press e for the built-in one.
key-back-to-text = Back to text

## Terminal dashboard: text editor

tui-code-modified = modified
tui-code-invalid = invalid JSON
tui-code-position = Ln { $line }, Col { $col }
tui-code-selected = { $count } selected
tui-code-status-ok = Valid JSON. Ctrl+S saves after sing-box check; F10 lists every command.
tui-code-no-problems = No problems found
tui-code-copied = Copied { $lines ->
        [one] one line
       *[other] { $lines } lines
    }
tui-code-clipboard-empty = Nothing copied yet; the terminal's own paste (often Ctrl+Shift+V) works too.
tui-code-fix-first = Line { $line }: { $error }
tui-code-tree-invalid = The tree view needs valid JSON. Line { $line }: { $error }
tui-code-formatted = Formatted; Ctrl+Z undoes it
tui-code-formatted-comments = Formatted, which removed the comments; Ctrl+Z undoes it
tui-code-formatted-already = Already formatted
tui-code-unsaved-title = "{ $name }" has unsaved changes
tui-code-save-close = Save and close
tui-code-keep-editing-at-error = Keep editing at the reported place
tui-code-save-in-progress = Still saving; wait for it to finish
tui-find-label = Find
tui-replace-label = Replace
tui-goto-label = Go to
tui-goto-placeholder = line[:column] or a path like outbounds[0].server
tui-goto-empty = enter a line number or a path
tui-goto-not-found = nothing at { $target }
tui-find-count = { $count ->
        [one] one match
       *[other] { $count } matches
    }
tui-replaced = Replaced { $count ->
        [one] one match
       *[other] { $count } matches
    }
key-goto = Go to
key-replace = Replace
key-replace-all = Replace all
key-switch-field = Switch field
key-previous-next = Previous/next
key-undo-redo = Undo/redo
key-tree = Tree
key-commands = Commands
code-commands-title = Editor commands
code-cmd-save = Save (sing-box check)
code-cmd-format = Format the document
code-cmd-find = Find
code-cmd-replace = Replace
code-cmd-goto = Go to a line or path
code-cmd-problem = Go to the problem
code-cmd-tree = Tree view
code-cmd-comment = Comment or uncomment lines
code-cmd-duplicate = Duplicate lines
code-cmd-delete-lines = Delete lines
code-cmd-undo = Undo
code-cmd-redo = Redo
code-cmd-select-all = Select all
code-cmd-help = Keys
code-cmd-close = Close the editor
help-code-title = Text editor
help-code-file = Document
help-code-save = Save; sing-box checks it first
help-code-close = Close, asks about unsaved changes
help-code-tree = Tree view with sing-box templates
help-code-commands = All commands
help-code-format = Format the document
help-code-find = Find
help-code-search = Find (Alt+C: match case)
help-code-replace = Replace (Alt+A: all)
help-code-next = Next or previous match
help-code-goto = Go to a line or a path
help-code-problem = Go to the error
help-code-edit = Edit
help-code-undo = Undo, redo
help-code-clipboard = Copy, cut, paste; no selection: the line
help-code-select-all = Select all
help-code-lines = Duplicate or delete lines
help-code-move-lines = Move lines
help-code-indent = Indent, outdent
help-code-comment = Comment lines with //
help-code-cursor = Cursor
help-code-select = Select
help-code-words = Move by word
help-code-ends = Start or end of the document
help-code-mouse = Click, drag, wheel; double click: word
help-code-note = The terminal's own selection still works with Shift held while dragging. Brackets and quotes close themselves and Enter keeps the indentation.

## JSON problems (profiles and the text editor)

json-at = line { $line }, column { $column }: { $message }
json-expected-colon = a colon is missing after the key
json-expected-value = a value is missing here
json-missing-comma = a comma is missing between two values
json-expected-key = keys must be in double quotes
json-bad-number = { $number } is not a valid number
json-single-quotes = strings need double quotes
json-trailing = unexpected text after the end of the document
json-unmatched = “{ $token }” has no matching opening bracket
json-unexpected = “{ $token }” does not belong here
json-bad-literal = { $word } is not a value; text needs double quotes
json-unclosed = “{ $bracket }” is not closed
json-unclosed-string = the string is not closed on this line
json-unclosed-comment = the comment is not closed
json-bad-escape = invalid escape sequence in a string; write \\ for a backslash
json-too-deep = nested too deeply
json-duplicate-key = "{ $key }" appears more than once here; sing-box uses the last one
json-odd-space = the invisible character { $name } here is not a space sing-box accepts; replace it with a normal space

## Terminal dashboard: editor templates

tpl-mixed = HTTP and SOCKS proxy port
tpl-tun = virtual interface for all traffic
tpl-socks-in = SOCKS proxy port
tpl-http-in = HTTP proxy port
tpl-tproxy = transparent proxy (TPROXY)
tpl-redirect = transparent proxy (redirect)
tpl-selector = group you pick a node in
tpl-urltest = group that picks the fastest node
tpl-direct = direct connection
tpl-node = proxy node
tpl-vless = VLESS with REALITY
tpl-upstream = upstream proxy
tpl-wireguard = WireGuard endpoint
tpl-sniff = detect the protocol
tpl-hijack-dns = answer DNS queries
tpl-private-direct = private addresses go direct
tpl-domain-rule = domain suffixes to an outbound
tpl-rule-set-rule = rule set to an outbound
tpl-process-rule = processes to an outbound
tpl-reject = block
tpl-clash-mode = by Clash mode
tpl-remote-rule-set = downloaded rule set
tpl-local-rule-set = rule set from a local file
tpl-inline-rule-set = rules written inline
tpl-dns-udp = plain DNS
tpl-dns-https = DNS over HTTPS
tpl-dns-tls = DNS over TLS
tpl-dns-quic = DNS over QUIC
tpl-dns-local = system resolver
tpl-dns-fakeip = fake IP addresses
tpl-dns-dhcp = from DHCP
tpl-dns-rule-set = rule set to a server
tpl-dns-fakeip-rule = A and AAAA queries to FakeIP
tpl-provider = remote node provider
tpl-sub-store-provider = Sub-Store { $kind }
tpl-text = text
tpl-number = number
tpl-switch = switch
tpl-object = object
tpl-list = list
tpl-null = empty

## System tray

tray-no-session-bus = cannot connect to the D-Bus session bus ({ $error }); the tray needs a graphical desktop session
tray-already-running = the tray is already running in this desktop session
tray-no-executable = cannot find the path of the singbox-board executable
tray-waiting-host = No system tray is available yet ({ $reason }). The icon appears as soon as one starts; on GNOME, enable the AppIndicator extension.
tray-start-failed = cannot show the tray icon: { $error }
tray-notify-failed = cannot show a notification: { $error }
tray-daemon-down = Cannot reach the daemon
tray-core-state = { $state ->
    [running] sing-box is running
    [starting] sing-box is starting
    [stopping] sing-box is stopping
    [backoff] sing-box will restart shortly
    [failed] sing-box has failed
   *[stopped] sing-box is stopped
    }
tray-core-exited = sing-box exited unexpectedly
tray-detail = { $label }: { $value }
tray-label-core = Core
tray-label-mode = Mode
tray-start = Start sing-box
tray-stop = Stop sing-box
tray-restart = Restart sing-box
tray-start-service = Start the singbox-board service
tray-no-profiles = No profiles yet
tray-update-profiles = Update the remote profiles
tray-open-dashboard = Open the dashboard
tray-open-sub-store = Open Sub-Store
tray-autostart = Start on login
tray-quit = Quit
tray-failed = { $op ->
    [start] Could not start sing-box
    [stop] Could not stop sing-box
    [restart] Could not restart sing-box
    [profile] Could not switch the profile
    [update] Could not update the remote profiles
    [mode] Could not switch the mode
    [dashboard] Could not open the dashboard
    [browser] Could not open the web page
    [autostart] Could not change the login item
    [container] Could not start or stop the container
    [service] Could not start the service
   *[other] The action failed
    }
tray-no-terminal = no terminal emulator found; install one such as Konsole or GNOME Console, or set the TERMINAL environment variable
tray-open-failed = xdg-open could not open { $url } ({ $status })
tray-no-home = neither XDG_CONFIG_HOME nor HOME is set
tray-desktop-comment = sing-box status and controls in the system tray

## Containers (kurumi-containerd): states and runtime

runtime-origin-release = downloaded release
runtime-origin-imported = imported build
runtime-origin-configured = daemon.toml
runtime-origin-system = system (PATH)
busy-container-starting = starting
busy-container-stopping = stopping
busy-container-restarting = restarting
busy-container-installing = installing
busy-container-removing = removing
busy-container-downloading = downloading { $percent }%
busy-container-downloading-unknown = downloading

## Containers: configurations

containers-config-too-large = the configuration exceeds { $limit }
containers-config-invalid-toml = invalid TOML at line { $line }, column { $column }: { $error }
containers-config-invalid-toml-plain = invalid TOML: { $error }
containers-config-not-table = { $key } must be a table
containers-config-missing = { $key } is missing
containers-config-bad-name = container.name "{ $name }" may only contain ASCII letters, digits, ".", "_" and "-"
containers-config-rootfs = set exactly one of container.rootfs and container.rootfs_image
containers-config-init = container.init must be an absolute path inside the container, not "{ $init }"
containers-config-network = unknown network mode "{ $network }" (valid: { $valid })
containers-config-broken = the configuration of { $name } cannot be used: { $error }
containers-config-rejected = kurumi-containerd rejects the configuration: { $error }
containers-check-passed = kurumi-containerd accepts the configuration.
containers-check-later = kurumi-containerd checks the configuration once the root filesystem is installed.
containers-check-warning = Warning: { $error }
containers-default-name = container
containers-template-network = unknown network mode "{ $network }" for a new container (host, nat or none)
containers-created-hint = Next, install a root filesystem into { $rootfs }, for example with `singbox-board container install <name> debian/trixie`.
containers-name-taken = another registered container is already named "{ $name }" (container.name)
containers-display-name-taken = another container is already called "{ $name }"
containers-empty-name = the name must not be empty
containers-rename-running = stop { $name } before changing its container.name
containers-save-rejected = the configuration was not saved (--force saves it anyway)
containers-add-file-and-content = give either a file to register or content to store, not both
containers-add-relative = the configuration path must be absolute
containers-already-registered = { $path } is already registered

## Containers: daemon replies and log

containers-registry-ignored = Ignoring the unreadable container registry { $path }: { $error }
containers-not-found = no container matches "{ $query }"
containers-none = no containers are registered
containers-need-root-daemon = containers need the daemon to run as root
containers-root-only = only root may do this ({ $action }), because it decides what runs as root in a container; use sudo
containers-busy = container { $name } is busy ({ $action })
containers-runtime-missing = kurumi-containerd is not installed; install it with `singbox-board container runtime update`
containers-runtime-auto-install = kurumi-containerd is not installed yet; downloading the newest release
containers-runtime-configured = containers.runtime in daemon.toml selects { $path }; downloaded releases are not used while it is set
containers-runtime-lookup-failed = could not look up the kurumi-containerd releases of { $repo }
containers-runtime-downloading = Downloading kurumi-containerd { $tag } from { $repo }
containers-runtime-installed-log = Installed kurumi-containerd { $version } ({ $checksum })
containers-runtime-installed = Installed kurumi-containerd { $version } ({ $checksum })
containers-runtime-unsupported = kurumi-containerd publishes no Linux build for this architecture ({ $arch }); import one with `singbox-board container runtime import`
containers-runtime-no-asset = release { $tag } has no build for { $target }
containers-runtime-import-location = give an absolute path or an http(s) URL
containers-runtime-not-runnable = this kurumi-containerd build does not run on this system
containers-runtime-version-timeout = kurumi-containerd --version did not answer in time
containers-runtime-version-failed = kurumi-containerd --version failed: { $error }
containers-runtime-not-binary = { $name } is neither a kurumi-containerd binary nor an archive containing one
containers-command-timeout = kurumi-containerd did not finish within { $seconds } seconds
containers-already-running = Container { $name } is already running (PID { $pid })
containers-foreground = { $name } is configured with container.foreground = true; the daemon runs containers in the background only, so set it to false
containers-not-installed = { $name } has no root filesystem at { $rootfs } yet; install one first (`singbox-board container install`)
containers-starting-log = Starting container { $name }
containers-start-failed-log = Container { $name } failed to start: { $error }
containers-start-failed = container { $name } failed to start: { $error }
containers-started = Container { $name } started (PID { $pid })
containers-not-running = Container { $name } is not running
containers-stopping-log = Stopping container { $name }
containers-stop-failed = container { $name } could not be stopped: { $error }
containers-stopped = Container { $name } stopped
containers-exec-empty = no command given
containers-exec-log = Running in container { $name }: { $command }
containers-install-size = { $name } uses an ext4 image (container.rootfs_image); give its size, for example 8G
containers-install-size-directory = a size only applies to ext4 images (container.rootfs_image)
containers-install-exists = { $name } already has a root filesystem at { $rootfs }; replace it with --force
containers-install-running = stop { $name } before installing a root filesystem
containers-install-source = "{ $source }" is neither an absolute path, an http(s) URL nor an image such as debian/trixie
containers-install-failed = installing the root filesystem of { $name } failed: { $error }
containers-installing-log = Installing the root filesystem of { $name } from { $source }, { $size }
containers-installed = Installed the root filesystem of { $name } from { $source } into { $rootfs }
containers-downloading-log = Downloading the root filesystem of { $name } from { $source }
containers-download-progress = { $name }: downloaded { $percent }% ({ $received } of { $total })
containers-download-unverified = The root filesystem of { $name } from { $source } was not verified, because no SHA-256 was given
containers-image-unknown = the image server has no image { $image } for { $arch }; list the images with `singbox-board container images`
containers-image-unknown-release = the image server has no { $image }; available: { $releases }
containers-image-no-checksum = { $url } lists no checksum for rootfs.tar.xz
containers-added-log = Registered container { $name } ({ $id }): { $file }
containers-added = Registered container { $name } ({ $id }, container.name { $container })
containers-saved-log = Saved the configuration of container { $name }: { $file }
containers-saved = Saved the configuration of { $name }
containers-restart-to-apply = The container is running; restart it to apply the change.
containers-autostart-on = { $name } starts with the daemon
containers-autostart-off = { $name } no longer starts with the daemon
containers-renamed = Renamed to { $name }
containers-remove-running = stop { $name } before removing it
containers-remove-mounted = { $path } is still mounted; unmount it before deleting the container's files
containers-removed-purged = Removed container { $name } and deleted its files
containers-removed-kept = Removed container { $name }; its files are kept in { $path }
containers-removed-linked = Removed container { $name }; its configuration { $path } is left where it is
containers-removed-log = Removed container { $name } ({ $id })
containers-adopted = { $count ->
    [one] Registered 1 container
   *[other] Registered { $count } containers
    }
containers-adopted-line = { $name }: { $file }
containers-adopt-skipped = { $name } skipped: { $error }
containers-autostart-failed = Could not start container { $name } at boot: { $error }
containers-left-running = { $count ->
    [one] 1 container keeps running; it is not stopped with the daemon
   *[other] { $count } containers keep running; they are not stopped with the daemon
    }

## Containers: command line

cli-container = Manage containers run by kurumi-containerd
cli-container-list = List the containers (default)
cli-container-show = Show a container's details
cli-container-show-config = Print its TOML configuration instead
cli-container-new = Create a container from the template
cli-container-name = Name shown for the container
cli-container-id = Container name or id
cli-container-network = Network of the new container: host (default; shares sing-box's TUN), nat or none
cli-container-new-image = Install this image as the root filesystem, e.g. debian/trixie (see `container images`)
cli-container-new-edit = Open the configuration in the editor before installing
cli-container-new-start = Start the container once it is ready
cli-container-add = Register a kurumi-containerd TOML configuration (a copy, or the file in place with --link)
cli-container-add-file = TOML file, or - for standard input
cli-container-add-link = Keep using the file where it is instead of storing a copy
cli-container-adopt = Register the containers of a kurumi-containerd registry (root's ~/.kurumi-containerd/config.json by default)
cli-container-adopt-registry = The registry file to read
cli-container-edit = Edit a container's configuration in the built-in editor
cli-container-force-save = Save even if kurumi-containerd rejects the configuration
cli-container-start = Start a container
cli-container-stop = Stop a container
cli-container-restart = Restart a container
cli-container-install = Install a root filesystem into a container
cli-container-install-source = A local tar or ZIP archive, an http(s) URL or an image such as debian/trixie
cli-container-install-size = Size of a new ext4 image (rootfs_image), e.g. 8G
cli-container-install-sha256 = Expected SHA-256 checksum of the archive
cli-container-install-force = Replace an existing root filesystem
cli-container-exec = Run a command in a running container and print its output
cli-container-exec-timeout = Seconds to wait for the command (default 60)
cli-container-exec-command = The command and its arguments; no shell is involved (use sh -c for pipes)
cli-container-enter = Open an interactive shell in a running container (needs root)
cli-container-enter-user = User to log in as (default root)
cli-container-autostart = Choose whether a container starts with the daemon
cli-container-autostart-value = on or off
cli-container-rename = Rename a container
cli-container-remove = Unregister a stopped container
cli-container-remove-purge = Also delete its files in the store, root filesystem included
cli-container-images = List the root filesystem images available for this machine
cli-container-images-filter = Show only distributions whose name contains this
cli-container-check = Check that this host can run containers
cli-container-scan = Recover the state of containers that are still running
cli-container-runtime = Show the kurumi-containerd runtime
cli-container-runtime-update = Install kurumi-containerd or update it to the newest release
cli-container-runtime-tag = Install this release instead of the newest, e.g. v0.2.3
cli-container-runtime-import = Use a kurumi-containerd build from a file or URL (root only)
cli-container-runtime-location = Absolute path or http(s) URL of a binary or release archive
ctl-label-containers = Containers
ctl-containers-summary = { $running } of { $total } running
ctl-label-name = Name
ctl-label-state = State
ctl-label-process = Process
ctl-label-usage = Usage
ctl-label-reboots = Reboots
ctl-label-config-error = Configuration
ctl-label-autostart = Autostart
ctl-label-config = Configuration
ctl-label-last-error = Last error
ctl-label-hostname = Hostname
ctl-label-rootfs = Root filesystem
ctl-label-init = Init
ctl-label-network = Network
ctl-label-ports = Ports
ctl-label-mount = Mount
ctl-label-limits = Limits
ctl-label-options = Options
ctl-label-release = Release
ctl-label-release-build = Release build
ctl-label-registry = Registry
ctl-container-via = via { $bridge }
ctl-container-live = PID { $pid }, up { $uptime }, { $processes } processes, { $memory }
ctl-container-process = init PID { $pid } ({ $init }), monitor PID { $monitor }, up { $uptime }
ctl-container-usage = { $processes } processes, { $memory } resident, { $cpu } s CPU
ctl-container-linked = { $file } (registered in place)
ctl-container-image-file = ext4 image
ctl-container-no-rootfs = no root filesystem yet
ctl-container-autostart = starts with the daemon
ctl-container-limit-memory = memory { $memory }
ctl-container-limit-cpu = { $cpus } CPUs
ctl-container-limit-pids = { $pids } processes
ctl-container-flag-volatile = volatile (changes are discarded)
ctl-container-flag-userns = nested user namespaces allowed
ctl-container-flag-foreground = foreground (not supported by the daemon)
ctl-container-runtime-version = { $version } ({ $origin })
ctl-container-runtime-unknown-version = installed ({ $origin }), version unknown
ctl-container-runtime-missing = not installed; it is downloaded when the first container starts
ctl-container-runtime-unsupported = not installed, and no release build fits this machine
ctl-container-runtime-no-build = none for this machine
ctl-container-runtime-downloading = kurumi-containerd is downloaded first; this may take a moment.
ctl-container-runtime-unsafe = refusing to run { $path } as root: it is not owned by root or is writable by others
ctl-containers-empty = No containers yet.
ctl-container-hint-new = New: singbox-board container new <name> --image debian/trixie --start
ctl-container-hint-use = Use: singbox-board container start|stop|enter|exec <name>; details: singbox-board container show <name>
ctl-container-installing = Installing the root filesystem; downloading and unpacking may take a few minutes.
ctl-container-output-truncated = (output truncated)
ctl-container-enter-needs-root = opening a shell in a container needs root; run: { $command }
ctl-container-enter-needs-terminal = an interactive shell needs a terminal; use `singbox-board container exec` for commands
ctl-container-edit-needs-terminal = the built-in editor needs a terminal; use --external with $VISUAL or $EDITOR
ctl-container-images-title = Images on { $server } for { $arch }
ctl-container-images-empty = No images match.
ctl-container-images-hint = Install one: singbox-board container new <name> --image <distro/release>, or container install <name> <distro/release>

## Containers: terminal dashboard

tab-containers = Containers
col-uptime = Uptime
ctl-label-processes = Processes
tui-panel-containers = Containers ({ $count })
tui-panel-container-details = Container
tui-header-containers = containers { $running }/{ $total }
tui-runtime-ready = READY
tui-runtime-missing = NOT INSTALLED
tui-runtime-details = { $binary }, registry in { $home }
tui-runtime-download-note = kurumi-containerd is downloaded and verified when the first container starts, or now with U.
tui-containers-not-root = Running without root: containers can be started and stopped; creating, editing and installing need sudo singbox-board.
tui-containers-empty = No containers yet.
tui-containers-empty-hint = Press n to create one from a distribution image (Debian, Ubuntu, Alpine, Arch, ...), or Enter for more, such as registering existing kurumi-containerd configurations.
tui-containers-loading-images = Loading the image list
tui-containers-pick-image = Root filesystem for { $name }
tui-containers-pick-image-body = Images for { $arch } from { $server }, downloaded and verified with SHA256SUMS.
tui-container-new-title = New container
tui-container-new-hint = A name for the container. Its configuration is created from the template; the root filesystem is chosen next.
tui-container-new-placeholder = e.g. Debian dev
tui-container-network-title = Network of { $name }
tui-container-register-title = Register a configuration
tui-container-register-hint = Absolute path of a kurumi-containerd TOML file. It stays where it is.
tui-container-install-title = Install into { $name }
tui-container-install-hint = A local tar or ZIP archive (path on this machine) or an http(s) URL. Image targets (rootfs_image) also need a size: path 8G.
tui-container-install-size = this container uses an ext4 image; add its size after the source, e.g. 8G
tui-container-install-hint-short = No root filesystem yet; press i to install one.
tui-container-exec-title = Run in { $name }
tui-container-exec-hint = The command runs as root without a shell; quote words with spaces, or use sh -c '...' for pipes.
tui-container-exec-quotes = a quote is not closed
tui-container-exec-result = { $command } in { $name } (exit status { $code })
tui-container-exec-no-output = The command printed nothing.
tui-container-remove-title = Remove { $name }?
tui-container-shell-entering = Opening a shell in { $name }; exit it to return to the dashboard.
tui-container-shell-closed = Closed the shell of { $name }
tui-container-shell-failed = The shell of { $name } ended with { $status }
tui-container-reboots = { $count ->
    [one] rebooted once
   *[other] rebooted { $count } times
    }
tui-container-load-graph = CPU load
tui-confirm-container-stop = Stop container { $name }?
tui-confirm-container-restart = Restart container { $name }?
tui-confirm-runtime-install = Download and install kurumi-containerd now?
tui-confirm-runtime-update = Update kurumi-containerd { $version } to the newest release?
tui-copied-path = Copied { $path }
tui-toml-title = Configuration of { $name }
tui-toml-running = running, restart to apply
tui-toml-status-ok = Valid configuration. Ctrl+S saves after kurumi-containerd checks it; F10 lists every command.
tui-toml-fix-first = Fix this first: { $error }
tui-toml-save-anyway-detail = the container may fail to start with it
busy-container-action = { $action ->
    [start] Starting { $name }
    [stop] Stopping { $name }
    [restart] Restarting { $name }
   *[remove] Removing { $name }
    }
busy-creating-container = Creating the container
busy-installing-rootfs = Installing the root filesystem of { $name }
busy-running-command = Running a command in { $name }
busy-registering = Registering
busy-checking-host = Checking the host
busy-downloading-runtime = Downloading kurumi-containerd
menu-container-shell = Open a shell
menu-container-exec = Run a command
menu-container-edit = Edit the configuration
menu-container-install = Install a root filesystem
menu-container-reinstall = Replace the root filesystem
menu-container-install-file = From a file or URL
menu-container-install-file-detail = tar, tar.gz, tar.xz, tar.zst, ZIP
menu-container-install-later = Later
menu-container-autostart-on = Start with the daemon
menu-container-autostart-off = Do not start with the daemon
menu-container-copy-path = Copy the root filesystem path
menu-container-remove = Remove
menu-container-remove-purge = Remove and delete its files
menu-container-remove-purge-detail = configuration and root filesystem
menu-container-remove-keep = Remove, keep its files
menu-container-remove-keep-detail = they stay in the store directory
menu-container-remove-linked = Remove the registration
menu-container-new = New container
menu-container-register = Register a TOML configuration
menu-container-adopt = Register root's kurumi-containerd containers
menu-container-check = Check the host
menu-container-runtime = Install or update kurumi-containerd
menu-network-host = Host network
menu-network-host-detail = shares the host's network and sing-box's TUN
menu-network-nat = NAT
menu-network-nat-detail = own address behind kurumi-br0
menu-network-none = None
menu-network-none-detail = loopback only
key-start-stop = Start/stop
key-shell = Shell
key-run = Run
key-install = Install
key-comment = Comment
key-problem = Problem
help-containers = Containers
help-containers-menu = All actions; new container
help-containers-run = Start or stop, shell, run a command
help-containers-edit = Edit (built-in, $EDITOR), install a root filesystem
help-containers-delete = Autostart, remove
help-containers-host = Check the host, update the runtime, register root's containers
tui-container-state-detail = PID { $pid }, up { $uptime }
tui-container-hostname = hostname { $hostname }
ctl-label-identity = Identity
tray-busy-container = Starting or stopping a container
containers-install-untrusted-parent = kurumi-containerd only installs a root filesystem into a directory that root owns and only root can write to; { $path } is not (fix it with chown root:root and chmod go-w)
containers-install-untrusted-ancestor = kurumi-containerd refuses to install below { $path }, because other users can write to it; make it writable by root only (chmod go-w)
ctl-container-runtime-checking = Looking for the newest kurumi-containerd release.
containers-linked-writable = Warning: users other than root can change { $path }; whoever can edit this configuration decides what runs as root in the container. Keep it writable by root only.

## Windows

win-cli-tray = Show sing-box in the Windows notification area
win-client-daemon-not-running = the daemon is not running (no pipe at { $socket }); start the service with `Start-Service singbox-board` (or `sc start singbox-board`) in a terminal opened as administrator
win-client-permission-denied = access to { $socket } was denied; run as administrator, or join the `singbox-board` group (as administrator: `net localgroup singbox-board %USERNAME% /add`) and sign out and back in
win-containers-unsupported = containers need Linux: kurumi-containerd runs Linux system containers and is not available on Windows
win-auth-uids-ignored = allowed_uids has no effect on Windows; add users to the socket group instead
win-auth-group-missing = The socket group "{ $group }" cannot be found ({ $error }); only administrators can connect
win-service-kill = { $name } did not exit within { $seconds } seconds; terminating it
win-supervisor-kill = sing-box did not exit within { $seconds } seconds; terminating it
win-supervisor-reload-restarts = sing-box cannot reload its configuration in place on Windows; restarting it
win-daemon-needs-admin = the daemon must run as administrator (normally as the singbox-board service), because TUN and routing require it; pass --allow-non-root for development
win-daemon-non-admin = Running without administrator rights; TUN and routing features will fail
win-daemon-client-rejected = Rejected a client ({ $user }, PID { $pid })
win-daemon-permission-denied = access denied; run as administrator or join the socket group of the daemon
win-daemon-request = { $user } requested { $request }
win-daemon-admin-only = { $action ->
    [add-source] adding a core source
    [remove-source] removing a core source
   *[import] importing a custom core
    } requires administrator rights, because it decides which program the daemon runs as SYSTEM; use a terminal opened as administrator
win-acl-failed = failed to restrict access to { $path }
win-symlink-privilege = cannot create the symbolic link { $path }: Windows allows that to administrators only, unless Developer Mode is turned on
win-service-dispatch = cannot run as a service ({ $error }); `daemon --service` is meant for the service control manager, run `singbox-board daemon` in a terminal instead
win-tray-already-running = the tray is already running in this session
win-service-start-failed = cannot start the service { $name }: { $error }
win-service-elevation-declined = starting the service { $name } needs administrator rights
