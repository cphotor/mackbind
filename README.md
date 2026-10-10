# mackbind

在 Linux 上复现 macOS 键位习惯，同时完整保留 Linux 原生修饰键语义。

## 为什么需要它

macOS 与 Linux 的修饰键在硬件层面**完全一致**，内核也如实上报：

|物理位置|macOS|Linux 键码|
|---|---|---|
|左起第 1|Ctrl|`KEY_LEFTCTRL`|
|左起第 2|Option|`KEY_LEFTALT`|
|左起第 3|Command|`KEY_LEFTMETA`|
|Tab 行最左|CapsLock|`KEY_CAPSLOCK`|

现有工具（如 Toshy、Kinto）为实现「`Cmd+C` → `Ctrl+C`」，采用了一条全局改写：把 `KEY_LEFTMETA` 映射为 `KEY_RIGHT_CTRL`，**无条件生效**。这带来三个连锁后果：

1. **Cmd 失去 Super 语义。** macOS 上 Cmd 与 Ctrl 是并列的两个系统修饰键，`Ctrl+Cmd+Space`、`Ctrl+Cmd+F` 等组合有意义；改写后两者指向同一槽位，组合语义全部丢失。
2. **`LEFT_CTRL` 槽位被腾空**去安置 Super，导致 GUI 与终端下行为分叉。
3. **GUI 下的 Ctrl 习惯被破坏。** 在 VSCode + vim 插件中，物理 Ctrl 实际发出 `Super+V`，vim 收不到 `Ctrl+V`。

mackbind 采用的模型：**`Cmd` 即 `Ctrl`，规则表只管例外**。默认把 `Cmd+X` 映射为 `Ctrl+X`（保住 Linux 原生的 Ctrl 行为），仅在目标键与主键不同时才查表——`Cmd+Q` → `Alt+F4`、`Cmd+[` → `Alt+←`。这同时保住了 Ctrl 的原生行为和 macOS 的操作语义。

## 功能

开箱即用，无需任何配置：

- **CapsLock ↔ 左 Ctrl 位置互换**，两个键在 GUI 与终端下均发真 Ctrl
- **`Cmd+X` 默认映射为 `Ctrl+X`** —— 不做穷举，长尾组合自动生效（`Cmd+E`、`Cmd+B` 等原表未收录的组合同样可用）
- **14 条显式规则** —— 目标键与主键不同者：`Cmd+Q` → `Alt+F4`、光标移动、`Cmd+[` `Cmd+]` → `Alt+←` `Alt+→` 等
- **`Cmd+空格` → 概览** —— 读取桌面环境现有的绑定并发出同样的键，不改写你的设置
- **终端逐键特化** —— 终端内 `Cmd+C` → `Ctrl+Shift+C` 复制，而 `Ctrl+C` 仍是中断。GNOME 系终端的快捷键直接读其 GSettings schema，改过绑定也能跟上
- **Fn 键优先级** —— 用户态实现 F 键与亮度/媒体键的切换，无需修改内核参数或 root 权限

## 已知问题

### 未实现的快捷键

|macOS|Linux|状态|
|---|---|---|
|`Cmd+空格` Spotlight|GNOME 概览|✅ 读取 GNOME 现有绑定后发出同样的键|
|`Cmd+Tab` / `Cmd+H`|Super 组合|**暂缓**。Super 在 Linux 上是 Windows 键语义，与 macOS Cmd 不等价|
|显示桌面|`show-desktop`|动作存在但默认未绑定，实现需写入用户账户设置|
|`Shift+Cmd+Delete` 清倒废纸篓|`gio trash`|未实现|

完整的逐条状态见 `docs/macos-shortcuts.md` 的「实现状态」。

### 依赖 GNOME Shell 扩展

「按应用区分」需要知道当前焦点窗口是谁。Wayland 下 GNOME 不把窗口信息交给客户端，本机实测：

|方案|结果|
|---|---|
|`org.gnome.Shell.Eval`|返回 `(false, '')`，GNOME 41+ 限制|
|`org.gnome.Shell.WindowTracker`|对象路径不存在|
|`xdotool` / `wmctrl`|Wayland 下不可用|

因此需要扩展 [`focused-window-dbus`](https://extensions.gnome.org/extension/focused-window-dbus/)：

```bash
gnome-extensions install focused-window-dbus@flexagoon.com
```

扩展缺失时相关功能退化为默认行为（不视为错误），其余映射照常工作。

验证扩展是否真的可用：

```bash
gdbus call --session --dest org.gnome.Shell \
  --object-path /org/gnome/shell/extensions/FocusedWindow \
  --method org.gnome.shell.extensions.FocusedWindow.Get
```

应返回含 `wm_class` 的 JSON。注意 `--dest` 必须是 `org.gnome.Shell`：该扩展由 Shell 进程直接导出对象，不占独立 bus name，写成 `org.gnome.Shell.Extensions.Dbus` 会得到 `ServiceUnknown`，那是地址写错、**不是**扩展没启用。

### 输入法漏字母（非本工具缺陷）

若使用触控板且开启了「轻触即点击」，打字时手掌误触会被识别为鼠标点击，导致该位置的按键丢失，输入法无法合成拼音音节（表现为只输出英文字母）。

GNOME 的「打字时禁用触控板」在本工具运行时**不会生效**——该功能依赖桌面环境收到键盘事件，而本工具已独占键盘设备。

解决：

```bash
gsettings set org.gnome.desktop.peripherals.touchpad tap-to-click false
gsettings set org.gnome.desktop.peripherals.touchpad tap-and-drag false
```

### 触控板在运行期间不可用

MacBook 的键盘与触控板是同一个 evdev 设备。本工具独占该设备后，触控板事件不再上报，其功能（点击、滚动、手势）在运行期间不可用。

这是 evdev 独占机制的固有限制。关闭「轻触即点击」可规避打字时的误触（见上）。

## 工作原理

```
真实键盘 → /dev/input/eventN → [evdev 抓取] → 映射逻辑 → [uinput 注入] → 虚拟键盘 → 系统
                                    ↑ 独占，中间截断                    ↓ 系统继续往下走
```

工作于 evdev 层而非 XKB 层，因此在 GNOME Wayland 下同样有效（XKB 只覆盖 XWayland 应用）。

## 环境要求

- Linux（evdev + uinput）
- Rust 1.64+（编译需要）
- 当前用户在 `input` 组

## 安装

```bash
# 1. 确认用户在 input 组（通常已默认加入）
sudo gpasswd -a $USER input

# 2. 编译并安装到 ~/.local/bin
git clone https://github.com/<you>/mackbind
cd mackbind
cargo build --release
install -Dm755 target/release/mackbind ~/.local/bin/mackbind
```

安装到 `~/.local/bin/` 而非 `cargo install` 默认的 `~/.cargo/bin/`，是因为 **systemd user service 的 PATH 不包含 `~/.cargo/bin`**，`~/.local/bin` 则是更标准的用户级路径。

若偏好 `cargo install`，用 `--root` 指定目标：

```bash
cargo install --path . --root ~/.local    # 装到 ~/.local/bin/mackbind
```

## 使用

### 手动运行

```bash
mackbind                    # 启动
mackbind -v                 # 打印每个按键的转换过程
mackbind --check            # 仅校验配置，不抓取键盘
mackbind --list-devices     # 列出检测到的键盘设备
```

手动运行时停止：`systemctl --user stop mackbind`（若已启用服务），或从另一终端 `kill <PID>`。

### 作为 systemd 服务（推荐）

创建 `~/.config/systemd/user/mackbind.service`：

```ini
[Unit]
Description=mackbind — macOS keybindings for Linux
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/mackbind
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

启用：

```bash
systemctl --user daemon-reload
systemctl --user enable --now mackbind
```

`Restart=on-failure` 表示崩溃时自动重启，但你主动 `stop` 不会触发重启。

**不需要 `linger`。** linger 用于让 user service 在注销后继续运行，但注销后 GNOME 已退出，uinput 注入的事件没有接收方，因此对 mackbind 没有实际收益。

常用命令：

```bash
systemctl --user status mackbind        # 查看状态
systemctl --user restart mackbind       # 重启（配置改动后）
systemctl --user stop mackbind          # 停止
journalctl --user -u mackbind -f        # 实时日志
journalctl --user -u mackbind --since today   # 今日日志
```

## 配置

可选。**不写任何配置即为完整默认功能。** 配置文件查找顺序（后者覆盖前者）：

```
/etc/mackbind/config.toml          系统级
~/.config/mackbind/config.toml      用户级
$MACKBIND_CONFIG                    环境变量指定（最高优先级）
```

配置示例见 [`config.example.toml`](config.example.toml)。主要选项：

|选项|默认|说明|
|---|---|---|
|`remap`|CapsLock ↔ 左 Ctrl|修饰键替换/互换|
|`rules`|29 行|**映射真源**。每行 = `(应用, 按键组合) → 动作`，顺序即优先级|
|`fn_mode`|`fkeysfirst`|Fn 优先级：`fkeysfirst` / `fkeyslast` / `disabled`|
|`desktop.type`|`gnome`|桌面环境：`gnome` / `kde` / `none`|
|`desktop.spotlight`|`true`|`Cmd+空格` 是否映射为概览（读取 DE 现有绑定，不改写）|
|`ctrl_busy_apps`|`["gvim"]`|终端之外、Ctrl 有非标准含义的应用。**终端无需列举**，按 `.desktop` 的 `Categories=TerminalEmulator` 判定|

配置中字段名写错会**直接报错退出**而非静默忽略。

**规则表就是全部映射**——没有散落在别处的隐藏行为。启动时
`mackbind --verbose` 会打印整张表，每行管什么一屏可见。

### 规则表

三列：`apps`（应用）· `keys`（用户按下的组合，不含 Cmd）· `action`（做什么）。

```toml
# 全局：Cmd+Q 退出应用
[[rules]]
keys   = ["KEY_Q"]
action = { emit = { mods = ["A"], key = "KEY_F4" } }

# 只在 VSCode 里改写 Cmd+B
[[rules]]
apps   = ["code"]
keys   = ["KEY_B"]
action = { emit = { mods = ["C"], key = "KEY_B" } }

# Cmd+空格 → 概览：读 DE 现有绑定后发出同样的键
[[rules]]
keys   = ["KEY_SPACE"]
action = { de_accel = "overview" }
```

三种动作：

|动作|含义|
|---|---|
|`emit`|发出固定组合|
|`de_accel`|读桌面环境现有的绑定，发出同样的键（不改写用户配置）|
|`terminal_ctrl_shift`|仅当应用**自己**用 `Ctrl+Shift` 绑了该键时才加 Shift|

**顺序即优先级**：自上而下首个命中者生效。你的条目追加在默认之后，
故天然覆盖默认。

### 修改示例

```toml
# 不做 CapsLock/Ctrl 互换
remap = []

# 只把 CapsLock 变成左 Ctrl，不动左 Ctrl 本身
remap = [{ from = "KEY_CAPSLOCK", to = "KEY_LEFTCTRL" }]

# 加一条自己的快捷键：Cmd+E → Ctrl+Shift+E
[[rules]]
keys   = ["KEY_E"]
action = { emit = { mods = ["C-S"], key = "KEY_E" } }
desc   = "我的快捷键"

# 关闭 Fn 优先级
fn_mode = "disabled"

# 声明桌面环境
[desktop]
type = "gnome"

# 把某个应用也纳入特化（终端已自动判定）
ctrl_busy_apps = ["gvim", "org.gnome.gedit"]
```

更完整的示例见仓库根目录的 `config.example.toml`。

修改后 `systemctl --user restart mackbind` 或重新手动启动生效。

## 停用

临时停用但保留配置：

```bash
systemctl --user stop mackbind
```

开机不再自启：

```bash
systemctl --user disable mackbind
```

停止后键盘立即恢复原生行为。

## 删除

```bash
# 1. 停用并移除服务
systemctl --user disable --now mackbind

# 2. 删除 unit 文件
rm ~/.config/systemd/user/mackbind.service
systemctl --user daemon-reload

# 3. 删除二进制
rm ~/.local/bin/mackbind

# 4. 删除配置（可选）
rm ~/.config/mackbind/config.toml /etc/mackbind/config.toml

# 5. 移除 udev 规则 / input 组（若曾添加）
#    一般不需要移除
```

确认彻底清理：

```bash
ls ~/.config/systemd/user/ | grep mackbind   # 应无输出
pgrep -a mackbind                            # 应无输出
```

## 故障排除

**提示 `Device or resource busy`**

其他程序正占用键盘（最常见是 Toshy）。先停止它：

```bash
toshy-services-stop
# 若已卸载 Toshy，确认没有残留
pgrep -af xwaykeyz
```

**键盘失灵**

程序异常退出时 grab 可能未释放。从另一终端执行：

```bash
systemctl --user restart mackbind    # 若已启用服务
# 或
pkill mackbind
```

内核会在文件描述符关闭时自动释放 grab，通常立即恢复。仍未恢复则重新插拔键盘或重启。

**配置写错导致无法启动**

用 `--check` 定位问题：

```bash
mackbind --check
```

**某组合键不生效**

用 `-v` 看实际转换过程：

```bash
mackbind -v
```

## 与 Toshy 的关系

两者**不能同时运行**——都会独占键盘设备，第二个会 grab 失败。

| | Toshy | mackbind |
|---|---|---|
|模型|默认改写 + 逐组合指定输出|`Cmd` 即 `Ctrl`，规则表只管例外|
|Cmd 身份|`LEFT_META` → `RIGHT_CTRL`（全局）|`Cmd` 转为 `Ctrl`，Super 释放|
|Ctrl 语义|GUI 下被改写|恒为 Ctrl|
|表外 Cmd 组合|需显式指定|自动映射为 `Ctrl+X`|
|终端快捷键来源|配置内逐终端枚举|读终端自身 GSettings schema|

## 开发

```bash
cargo build --release     # 构建
cargo test                # 运行单元测试
cargo clippy              # 静态检查
```

架构与设计决策详见 [`DESIGN.md`](DESIGN.md)。

## 许可证

待定