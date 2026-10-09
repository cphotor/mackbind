# macOS 快捷键映射清单

依据：[Apple 官方 Mac 键盘快捷键](https://support.apple.com/zh-cn/102650)

**核心原则：以 macOS 语义为准。** 目标是让操作行为与 macOS 一致，而非让按键名称一致。若目标功能已被桌面环境占用，修改桌面环境的原有绑定以让位。

---

## 判定分类

| 类 | 含义 | 实现方式 |
|---|---|---|
| **A** | 应用内通用操作 | 默认 `Cmd+X` → `Ctrl+X` |
| **C** | 终端/编辑器特化 | `Cmd+X` → `Ctrl+Shift+X` |
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

此类应用内 `Ctrl` 已被占用，`Cmd` 映射为 `Ctrl+Shift` 以避免冲突。

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
|`Shift+Cmd+Z` 重做|`Ctrl+Y`|A|
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
|`Option+Cmd+W` 关闭 App 全部窗口|待实现|E|
|`Cmd+H` 隐藏窗口|`Super+h`|B（暂缓）|
|`Option+Cmd+H` 隐藏其他 App|无|F|
|`Cmd+M` 最小化|`Super+Down`|B（暂缓）|
|`Option+Cmd+M` 最小化全部窗口|无|F|
|`Option+Cmd+Esc` 强制退出|D-Bus 近似|E|
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
|`Option+Shift+↑/↓` 扩展到段首/段尾|`Alt+Shift+↑/↓`|D|
|`Option+Shift+←/→` 扩展到词首/词尾|`Alt+Shift+←/→`|D|
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

---

## 待办

- [x] VSCode —— 不做特化处理，`Cmd` 映射为 `Ctrl` 即可
- [x] GVIM —— 视同终端，纳入特化类
- [ ] `Cmd+Q` 的 D-Bus 实现方案（需按 DE 适配，KDE 有不同机制）
- [x] 「显示桌面」的实现方式（见下）
- [ ] Option 类组合的最终清单
- [ ] 访达→Nautilus 内部操作的映射策略（应用内处理 vs 全局规则）

## 附：「显示桌面」的实现

GNOME 内置 `show-desktop` 动作（隐藏所有普通窗口），默认未绑定：

```bash
gsettings get org.gnome.desktop.wm.keybindings show-desktop   # @as []（未绑定）
```

对应 macOS 的 `Cmd+调度中心` / `Fn+H` / `Fn+F11`（显示桌面），以及 `Ctrl+↑`（调度中心）。

> 注：设置界面中的条目名因 GNOME 版本而异。GNOME 51（本机）中该动作的
> schema 键为 `show-desktop`；「隐藏所有普通窗口」可能是其他版本或
> 其他 schema 下的名称。设置界面的显示名与底层键名不一定一致。
