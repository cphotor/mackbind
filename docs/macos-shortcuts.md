# macOS 快捷键映射清单

依据：[Apple 官方 Mac 键盘快捷键](https://support.apple.com/zh-cn/102650)

**核心原则：以 macOS 语义为准。** 目标是让操作行为与 macOS 一致，而非让按键名称一致。若目标功能已被桌面环境占用，修改桌面环境的原有绑定以让位。

---

## 判定分类

| 类 | 含义 | 实现方式 |
|---|---|---|
| **A** | 应用内通用操作 | 默认 `Cmd+X` → `Ctrl+X` |
| **C** | 终端/编辑器特化 | `Cmd+X` → `Ctrl+Shift+X`（**逐键**，非整体） |
| **D** | macOS 特有键位或功能 | 显式规则表 |
| **E** | 有等价功能但桌面环境未绑 | mackbind 主动实现（按 DE 适配） |
| **F** | 概念不存在于目标平台 | 放弃 |
| **B** | 平台 Super 绑定 | **暂缓**，见「暂缓事项」 |

---

## 核心设计原则

### 一、以 macOS 语义为准

目标是让操作**行为**与 macOS 一致，而非按键名称一致。若目标功能已被桌面环境占用，按「macOS 优先」处理冲突。

### 二、桌面环境是可配置项，不可硬编码

Linux 上有 GNOME、KDE Plasma、Sway、Hyprland 等多种桌面环境，同一 macOS 功能在不同 DE 下的实现方式、可用性、默认快捷键都不同。

因此 DE 相关行为必须做成配置项：

```toml
[desktop]
type = "gnome"      # gnome | kde | none | 自定义
```

| 目标 | GNOME | KDE Plasma | Sway / i3 |
|---|---|---|---|
|切换应用|`Super+Tab`|`Meta+Tab`|需自行绑定|
|显示桌面|`show-desktop`（默认未绑）|Plasma 概览|需自行绑定|
|截图|未绑定，需注册|Spectacle 自带|需自行绑定|

`type = "none"` 表示不查任何 DE 绑定，相关规则跳过，其余规则照常生效。

**不可查的功能不可当作不存在**——应记录在文档中，待支持对应 DE 时启用。

### 三、特化类应用

此类应用内 `Ctrl` 已被占用，`Cmd` 在其中映射为 `Ctrl+Shift`。**但这是逐键的，不是整体的**——只对应用自身已用 `Ctrl+Shift` 绑定的键加 Shift，其余键仍走默认的 `Ctrl`。理由见「实现状态 → 终端特化的粒度」。

判定依据分两层：

1. `.desktop` 条目的 `Categories` 含 `TerminalEmulator` —— 按 freedesktop 规范判定，自动覆盖新装的终端。硬编码 WM_CLASS 名单曾因写成小写 `ptyxis` 而实际值是 `org.gnome.Ptyxis` 漏判。
2. `ctrl_busy_apps` 配置项 —— 用于终端之外、Ctrl 有非标准含义的应用（如独立窗口的 gvim）。

焦点窗口信息经 GNOME Shell 扩展 `focused-window-dbus` 获取，采用懒查询（收到按键才查，`wm_class` 未变则跳过解析）。

---

## 暂缓事项

### Super 键相关规则（原分类 B）

macOS 的 `Cmd+Tab`（切换应用）、`Cmd+H`（隐藏）、`Cmd+↑/↓`（窗口管理）在 Linux 上对应 `Super+X` 组合。

**这类规则暂缓实现。** 理由：

1. 目标是模拟 macOS，Super 在 Linux 上是「Windows 键」语义，与 macOS 的 Cmd 并不等价
2. 各 DE 对 Super 的占用情况差异大（GNOME 用它管窗口，Sway 另有一套）
3. 先把不依赖 DE 的规则（A/C/D/E）做扎实，Super 相关留待后续

后续实现时按 §二 的方式做成 DE 配置项，不写死任何 DE 的绑定表。

---

## 一、常用操作

|macOS|Linux 目标|类|
|---|---|---|
|`Cmd+X` 剪切|`Ctrl+X`|A|
|`Cmd+C` 复制|`Ctrl+C`|A|
|`Cmd+V` 粘贴|`Ctrl+V`|A|
|`Cmd+Z` 撤销|`Ctrl+Z`|A|
|`Shift+Cmd+Z` 重做|`Ctrl+Shift+Z`|A|
|`Cmd+A` 全选|`Ctrl+A`|A|
|`Cmd+F` 查找|`Ctrl+F`|A|
|`Cmd+G` 查找下一个|`Ctrl+G`|A|
|`Shift+Cmd+G` 查找上一个|`Ctrl+G`|A|
|`Cmd+O` 打开|`Ctrl+O`|A|
|`Cmd+P` 打印|`Ctrl+P`|A|
|`Cmd+Q` 退出应用|D-Bus 请求焦点应用退出|E|
|`Cmd+S` 存储|`Ctrl+S`|A|
|`Cmd+T` 新标签页|`Ctrl+T`|A|
|`Cmd+W` 关闭窗口|`Ctrl+W`|A|
|`Option+Cmd+W` 关闭 App 全部窗口|`Ctrl+Alt+W`|应用处理|
|`Cmd+H` 隐藏窗口|`Super+h`|B（暂缓）|
|`Option+Cmd+H` 隐藏其他 App|无|F|
|`Cmd+M` 最小化|`Super+Down`|B（暂缓）|
|`Option+Cmd+M` 最小化全部窗口|无|F|
|`Option+Cmd+Esc` 强制退出|`Ctrl+Alt+Esc`|A|
|`Cmd+空格` 聚焦/Spotlight|活动概览；**GNOME 原 `Super+space` 需让位**|E|
|`Option+Cmd+空格` 访达内聚焦|无全局对应|F|
|`Ctrl+Cmd+空格` 字符检视器|无|F|
|`Ctrl+Cmd+F` 全屏|`F11`|A|
|`Cmd+,` 偏好设置|无全局对应|F|
|`Cmd+Tab` 切换应用|`Super+Tab`|B（暂缓）|
|`Shift+Cmd+Tab` 反向切换|`Shift+Super+Tab`|B（暂缓）|
|`Cmd+`` ` `` 切换窗口|无全局对应|F|
|`Cmd+Shift+3/4/5` 截图/录屏|`Print` / `Shift+Print` / `Print` 长按|D|
|`Cmd+Shift+N` 访达新建文件夹|`Ctrl+Shift+N`|A|
|`Ctrl+Cmd+N` 新建含所选项目的文件夹|无|F|
|`Shift+Cmd+6` Siri 询问（macOS 27+）|无|F|
|`Fn+Q` 快速备忘录|无|F|
|`Space` 快速查看|无全局对应|F|

## 二、睡眠、退出与关机

|macOS|Linux 目标|类|
|---|---|---|
|`Ctrl+Cmd+Q` 锁定屏幕|`Super+l`|D|
|`Shift+Cmd+Q` 退出登录|`Super+Shift+Q`|D|
|`Option+Shift+Cmd+Q` 直接退出登录|无|F|

> 电源按钮类快捷键无对应键盘按键，忽略。

## 三、访达与系统

访达即 macOS 的 Finder，对应 Linux 的 Nautilus。以下多为文件管理器内部操作，多数可由应用自身处理。

|macOS|Linux 目标|类|
|---|---|---|
|`Cmd+D` 复制文件|`Ctrl+D`|A|
|`Cmd+E` 推出磁盘|无|F|
|`Cmd+I` 显示简介|`Ctrl+I`|A|
|`Cmd+[` 上一个文件夹|`Alt+←`|D|
|`Cmd+]` 下一个文件夹|`Alt+→`|D|
|`Cmd+Delete` 移到废纸篓|`Ctrl+Delete`|D|
|`Shift+Cmd+Delete` 清倒废纸篓|`gio trash --empty --force`|E|
|`Option+Shift+Cmd+Delete` 不确认清倒废纸篓|`gio trash --empty --force`|E|
|`Cmd+1/2/3/4` 图标/列表/分栏/画廊视图|Nautilus 内部|应用处理|
|`Shift+Cmd+H` 个人文件夹|Nautilus 内部|应用处理|
|`Option+Cmd+L` 下载文件夹|Nautilus 内部|应用处理|
|`Shift+Cmd+O` 文稿文件夹|Nautilus 内部|应用处理|
|`Shift+Cmd+U` 实用工具文件夹|Nautilus 内部|应用处理|
|`Cmd+K` 连接服务器|Nautilus 内部|应用处理|
|`Cmd+J` 显示选项|Nautilus 内部|应用处理|
|`Cmd+N` 新访达窗口|`Ctrl+N`|A|
|`Cmd+↑/↓` 打开上级/所选|`Ctrl+↑/↓`|A|
|`Ctrl+Cmd+↑` 新窗口打开上级|无|F|
|`Shift+Cmd+D` 桌面文件夹|Nautilus 内部|应用处理|
|`Shift+Cmd+F` 最近使用|Nautilus 内部|应用处理|
|`Shift+Cmd+G` 前往文件夹|Nautilus 内部|应用处理|
|`Shift+Cmd+I` iCloud 云盘|无（无 iCloud）|F|
|`Shift+Cmd+K` 网络窗口|Nautilus 内部|应用处理|
|`Shift+Cmd+R` 隔空投送|无（无 Airdrop）|F|
|`Shift+Cmd+P` 预览面板|Nautilus 内部|应用处理|
|`Shift+Cmd+T` 标签页栏|Nautilus 内部|应用处理|
|`Option+Cmd+P/S/T/V` 路径栏/边栏/工具栏/移动|应用处理|应用处理|
|`Cmd+/` 状态栏|Nautilus 内部|应用处理|
|`Option+Cmd+N` 智能文件夹|无|F|
|`Option+Cmd+Y` 幻灯片放映快速查看|无全局对应|F|
|`Option+Cmd+D` 显示/隐藏程序坞|无（GNOME 无 Dock）|F|
|`Ctrl+Cmd+T` 添加到程序坞|无（GNOME 无 Dock）|F|
|`Ctrl+Cmd+A` 制作替身|无（无替身概念）|F|
|`Shift+Cmd+C` 电脑窗口|无|F|
|`Shift+Cmd+X` 添加到边栏|应用处理|应用处理|
|`Cmd+亮度键` 视频镜像|无|F|
|`Option+亮度键` 显示器设置|无全局对应|F|
|`Ctrl+亮度键` 外接显示器亮度|无（硬件相关）|F|
|`Option+调度中心键` 调度中心设置|无|F|
|`Cmd+调度中心` / `Fn+H` / `Fn+F11` 显示桌面|实现方式待定|E|
|`Ctrl+↑` 调度中心|活动概览|E|
|`Ctrl+↓` 前方 App 所有窗口|无|F|
|`Option+音量/键盘亮度键` 打开对应设置|无全局对应|F|

## 四、文本编辑

### 光标移动

|macOS|Linux 目标|类|
|---|---|---|
|`Cmd+←/→` 行首/行尾|`Ctrl+←/→`|A|
|`Cmd+↑/↓` 文稿开头/末尾|`Ctrl+↑/↓`|A|
|`Option+←/→` 上一/下一词首尾|`Alt+←/→`|D|
|`Shift+Cmd+←/→` 选中到行首/行尾|`Ctrl+Shift+←/→`|A|
|`Shift+Cmd+↑/↓` 选中到文稿首/末|`Ctrl+Shift+↑/↓`|A|
|`Shift+↑/↓` 扩展选择到上/下一行|`Ctrl+Shift+↑/↓`|D|
|`Shift+←/→` 扩展一个字符|`Ctrl+Shift+←/→`|D|
|`Option+Shift+↑/↓` 扩展到段首/段尾|`Alt+Shift+↑/↓`|A（默认语义已覆盖）|
|`Option+Shift+←/→` 扩展到词首/词尾|`Alt+Shift+←/→`|A（默认语义已覆盖）|
|`Fn+↑/↓/←/→` PageUp/PageDown/Home/End|`Ctrl+↑/↓`（X11 惯例）|D|

### 删除

|macOS|Linux 目标|类|
|---|---|---|
|`Option+Delete` 删除左边字词|`Ctrl+Delete`|D|
|`Ctrl+H` 删除左边字符|`Backspace`|A|
|`Ctrl+D` 删除右边字符|`Ctrl+Delete`|A|
|`Fn+Delete` 向前删除|`Ctrl+Delete`|D|

### Emacs 风格绑定（macOS 原生支持，Linux 已有同名键）

这些在 Linux 上本就可用，**不作任何映射**：

```
Ctrl+A 行首      Ctrl+E 行尾      Ctrl+F 前进      Ctrl+B 后退
Ctrl+P 上移      Ctrl+N 下移      Ctrl+O 插入新行  Ctrl+T 交换字符
Ctrl+L 居中      Ctrl+H 删左字符  Ctrl+D 删右字符
```

### 格式与编辑

|macOS|Linux 目标|类|
|---|---|---|
|`Cmd+B/I/U` 粗体/斜体/下划线|`Ctrl+B/I/U`|A|
|`Cmd+K` 插入网页链接|`Ctrl+K`|A|
|`Cmd+T` 显示/隐藏字体窗口|无（F）/ 已有（Linux）|F|
|`Cmd+;` 拼写检查|`Ctrl+;`|A|
|`Shift+Cmd+;` 拼写和语法窗口|无|F|
|`Ctrl+Cmd+D` 显示词定义|无|F|
|`Cmd+{` / `Cmd+}` 左/右对齐|无全局对应|F|
|`Shift+Cmd+\|` 居中对齐|无全局对应|F|
|`Option+Cmd+F` 前往搜索栏|`Alt+←` 或已有|F|
|`Option+Cmd+T` 显示/隐藏工具栏|应用处理|应用处理|
|`Option+Cmd+C` 拷贝样式|无|F|
|`Option+Cmd+V` 粘贴样式|无|F|
|`Option+Shift+Cmd+V` 粘贴并匹配样式|无|F|
|`Option+Cmd+I` 检查器窗口|无|F|
|`Shift+Cmd+P` 页面设置|无|F|
|`Shift+Cmd+-` / `Shift+Cmd+=` 缩小/放大|无全局对应|F|
|`Shift+Cmd+?` 帮助菜单|无|F|
|`Shift+Cmd+空格` Siri 询问（macOS 27+）|无|F|

## 五、辅助功能

默认关闭，暂不实现。

|macOS|说明|
|---|---|
|`Ctrl+Option+Cmd+8` 反转颜色|
|`Ctrl+Option+Cmd+,` / `.` 降低/增强对比度|
|`Ctrl+F2~F6` / `Fn+Ctrl+F2~F6` 焦点移动|
|`Ctrl+Shift+F6/F7/F8` 焦点相关|
|`Option+Cmd+F5` 辅助功能快捷键面板|
|`Ctrl+Tab` / `Ctrl+Shift+Tab` 焦点切换|

## 六、App 专用

Safari、日历、音乐、邮件等各有自己的快捷键，**不纳入全局映射**。多数已在 Linux 上保持一致（如 `Cmd+C` 复制）。

### 「应用处理」类的判定：`Option+Cmd+W`

Apple 官方文档确有此键——「关闭最前方的窗口。要关闭 App 的所有窗口，
请按下 Option-Command-W」。但它**由应用自行实现**，并非系统级：

|应用|`Option+Cmd+W`|
|---|---|
|访达|有（关闭该窗口全部）|
|Chrome|**无**（实测按了没反应）|

因此归类为「应用处理」，标 F 或标 E 都不准确：

- 标 **F**（概念不存在）→ 错，macOS 上确实存在
- 标 **E**（本工具实现）→ 错，GNOME 无对应动作，且不该由 mackbind 代劳

Linux 侧由默认语义给出 `Ctrl+Alt+W`，能否生效取决于应用。

> **教训**：「在某个应用里实测无反应」≠「该快捷键不存在」。
> 应用可能只是没实现它。换应用复测、或查官方文档后再下结论。
> 这与「找错地方导致误判不存在」是同一类错误。

---

## 应用分类清单

### 特化类（`Cmd` → `Ctrl+Shift`）

此类应用内 `Ctrl` 已被占用，映射为 `Ctrl+Shift` 以避免冲突。

**终端**（`Ctrl+C` 中断进程、`Ctrl+Z` 挂起）：

```
gnome-terminal-server  org.gnome.Console  kitty  Alacritty
foot  wezterm  org.kde.konsole  konsole  xfce4-terminal
io.elementary.terminal  Tilix  org.tigervault
```

**GVIM**（`Ctrl+V` 块选择、`Ctrl+C` 退出）：

```
nvim  vim
```

**VSCode**（`Ctrl+C` 复制行、`Ctrl+V` 粘贴）：

```
code  code-url-handler  codium
```

> VSCode 自身快捷键已与 macOS 高度一致，纳入特化类会与现有绑定冲突。
> 建议仅映射确实不同的组合，或整体排除。

---

## GNOME 需要调整的绑定

按「macOS 优先」原则，以下需让位或新增：

|macOS|目标|GNOME 现状|处理|
|---|---|---|---|
|`Cmd+空格` Spotlight|活动概览|`Super+space` = 切换输入法|让位|
|`Ctrl+Cmd+Q` 锁屏|`Super+l`|用户已自设|无冲突|
|`Cmd+Tab` 切换应用|`Super+Tab`|已绑定|无冲突|
|`Cmd+↑/↓`|`Super+↑/↓`|已绑定|无冲突|
|`Shift+Cmd+3/4/5` 截图|`Print` / `Shift+Print`|已绑定|无冲突|
|`Shift+Cmd+Q` 退出登录|`Super+Shift+Q`|需确认|可能需新增|

### GNOME 抢占 `Ctrl+Alt` 系列（重要）

默认语义把 `Cmd` 变 `Ctrl`，于是所有 `Option+Cmd+X` 都落到 `Ctrl+Alt+X`。
而 GNOME 把 `Ctrl+Alt` 系列绑给了**工作区切换**——这些在桌面环境层
就被拦截，**应用根本收不到**：

```
switch-to-workspace-up      ['<Control><Alt>Up']
switch-to-workspace-down    ['<Control><Alt>Down']
switch-to-workspace-left    ['<Control><Alt>Left']
switch-to-workspace-right   ['<Control><Alt>Right']
switch-panels               ['<Control><Alt>Tab']
cycle-panels                ['<Control><Alt>Escape']
```

影响：任何 `Option+Cmd+方向键/Tab/Esc` 组合都无法透传给应用。
这不是 mackbind 的缺陷，是目标平台的既有绑定。遇到这类组合时，
**必须查 DE 是否已占用**，不能只看应用认不认。

（`Ctrl+Alt+W` 不在此列——`W` 未被占用，能传给应用。但 Chrome 也不认它，
且 macOS 上该组合本就无行为，故标 F。）

---

## 实现状态

对照本文档逐条核对的结果。**未列出的组合即由默认语义自动覆盖**（`Cmd+X → Ctrl+X`），不需要单独实现。

### 已实现

|项|实现方式|
|---|---|
|A 应用内通用操作|默认语义，无需列举|
|C 终端/GVIM 特化|逐键粒度，见下|
|`Cmd+Q` → `Alt+F4`|全局显式规则（GNOME `wm.keybindings close`）|
|`Cmd+空格` → 概览|读取 GNOME 的 `toggle-overview` 现有绑定并发出同样的键|
|D 光标移动 / `Cmd+[` `Cmd+]`|`DEFAULT_CMD_MAP` 中 14 条|
|Delete → `Ctrl+Delete`|`DEFAULT_CMD_MAP`|
|`Super+l` 锁屏|用户已自设，mackbind 不介入|
|Super 相关（分类 B）|**暂缓**，见「暂缓事项」|

### 未实现

|项|阻塞原因|
|---|---|
|`Shift+Cmd+Delete` 清倒废纸篓|需执行 `gio trash --empty --force`；可做，未做|
|`Ctrl+↑` 调度中心|与概览同键，GNOME 未提供独立的「调度中心」动作|
|「显示桌面」|动作存在（`show-desktop`）但默认未绑定，需写入用户账户设置|
|Option 类组合最终清单|`A-KEY_LEFT` / `A-KEY_RIGHT` 已做，其余未逐条核对|
|访达 → Nautilus 内部操作|多数由应用自身处理，无需全局规则|

### 终端特化的粒度（重要）

**不是**「终端里把 `Cmd` 一律变成 `Ctrl+Shift`」。那样做会让 `Cmd+Q` 退化成 `Ctrl+Shift+Q`——一个谁也不认的组合。

实际做法是**逐键**：只对**终端自身已用 `Ctrl+Shift` 绑定**的键加 Shift。

更进一步：GNOME 系终端用 GSettings 声明全部快捷键，mackbind 直接读取其 schema：

```bash
gsettings list-recursively org.gnome.Ptyxis.Shortcuts
# new-window      '<ctrl><shift>n'
# close-window    '<ctrl><shift>q'
# copy-clipboard  '<ctrl><shift>c'
```

因此用户改过终端快捷键，mackbind 会自动跟上。手写白名单曾漏掉 `n`（新窗口），这是改用 schema 的直接原因。

不使用 GSettings 的终端（kitty、wezterm 等）查不到 schema，退回内置白名单。

> 注：ptyxis 的 `close-window` 实为 `Ctrl+Shift+Q`，但全局规则 `Cmd+Q → Alt+F4` 优先于终端特化，故实际发出的是 `Alt+F4`。这是有意的——`Alt+F4` 是 GNOME 的通用关闭绑定，跨应用一致。

### 终端的复制键：ptyxis / konsole 特例

这两个终端在**无选中文本**时会把 `Ctrl+Shift+C` 穿透给子进程当作 SIGINT，使「复制」杀掉正在运行的任务。wezterm、ghostty、gnome-terminal 无此问题（无条件吞掉该组合）。

故这两者改用 `Ctrl+Insert`（X11 传统复制键，终端均支持且不会被当作控制字符发送）。由 `app_cmd_map` 配置项承载，判定依据是**行为差异**而非应用名单。

---

## 附：概览（Spotlight）的实现

macOS 的 `Cmd+空格` 对应 GNOME 的活动概览。GNOME 为此提供了可绑定动作：

```bash
gsettings get org.gnome.shell.keybindings toggle-overview
# ['<Shift><Control>space']
```

mackbind 的做法是**读取这个现有绑定并发出同样的键**，而非改写绑定。因此：

- 不在用户配置（dconf）里留下任何记录
- 不影响用户自己对该键的设置
- 用户改了绑定，mackbind 自动跟上

该组合在 macOS 中是 `Cmd+Ctrl+空格`（切换输入法），语义亦相符。

注：`Super+space`（GNOME 的输入法切换）在 mackbind 模型下不会被占用——默认语义不产生 Super。二者不再冲突，无需「让位」。

### 一度误判为不可行

初查时只看了 `org.gnome.desktop.wm.keybindings`，其中确实没有概览动作，遂判定需「写入用户账户设置」才能实现，属侵入式改动而暂缓。

这个结论是错的——概览动作在**另一个 schema**（`org.gnome.shell.keybindings`）下。教训与此前「默认方向反转」同源：**在断言某能力不存在之前，应先确认是否找对了地方**。

真正不可用的是以下路径（实测记录，以免重复尝试）：

|方案|结果|
|---|---|
|`org.gnome.Shell.Eval`|返回 `(false, '')`，GNOME 41+ 限制|
|`org.gnome.Shell.WindowTracker`|对象路径 `/org/gnome/Shell/WindowTracker` 不存在|
|`xdotool` / `wmctrl`|Wayland 下不可用|

### 键盘类型差异

macOS 的 `Cmd+调度中心` 在 MacBook 上对应 `Fn+F11`，在带独立功能键的外接键盘上则无对应键。GNOME 侧同样无独立的「调度中心」动作（`Ctrl+↑` 语义上接近，但 GNOME 未提供该绑定）。

---

## 附：「显示桌面」的实现

GNOME 内置 `show-desktop` 动作（隐藏所有普通窗口），默认未绑定：

```bash
gsettings get org.gnome.desktop.wm.keybindings show-desktop   # @as []（未绑定）
```

对应 macOS 的 `Cmd+调度中心` / `Fn+H` / `Fn+F11`（显示桌面），以及 `Ctrl+↑`（调度中心）。

绑定该动作后可由 mackbind 发出对应键实现，但同样需要写入账户设置，故暂缓。

> 注：设置界面中的条目名因 GNOME 版本而异。GNOME 51（本机）中该动作的
> schema 键为 `show-desktop`；「隐藏所有普通窗口」可能是其他版本或
> 其他 schema 下的名称。设置界面的显示名与底层键名不一定一致。
