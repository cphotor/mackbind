# mackbind — 设计方案

在 Linux 上模拟 macOS 键位习惯，同时完整保留 Linux 原生修饰键语义。

## 问题

macOS 与 Linux/Windows 的修饰键语义几乎相同，只有名字不同：

|物理位置|macOS|Linux 内核报告|X11 keysym|
|---|---|---|---|
|左起第 1|Ctrl|`KEY_LEFTCTRL`|Control_L|
|左起第 2|Option|`KEY_LEFTALT`|Alt_L|
|左起第 3|Command|`KEY_LEFTMETA`|Super_L / Meta_L|
|Tab 行最左|CapsLock|`KEY_CAPSLOCK`|Caps_Lock|

硬件层面**无需任何转换**。真正的问题出在软件层。

### Toshy 的做法及其副作用

Toshy（以及它继承的 Kinto）为了实现「`Cmd+X` → `Ctrl+X`」，采用了一条全局 modmap：

```
Key.LEFT_META → Key.RIGHT_CTRL     # Cmd 无条件变成 Ctrl
```

这条改写**无条件生效**，导致三个连锁后果：

1. **Cmd 失去 Super 语义。** macOS 上 Cmd 与 Ctrl 是并列的两个系统修饰键，`Ctrl+Cmd+Space`、`Ctrl+Cmd+Q`、`Ctrl+Cmd+F` 都是合法且有意义的组合。Cmd 被映射成 Ctrl 之后，这两个修饰键指向同一槽位，无法区分。

2. **LEFT_CTRL 槽位被腾空，只能安置 Super。** 因为 Cmd 占了 Ctrl 的语义，物理 Ctrl 键必须改道，于是：

   ```python
   modmap("Cond modmap - GUI - Mac kbd", {
       Key.LEFT_CTRL:  Key.LEFT_META,   # GUI 下 → Super
   })
   ```

   同一位置在终端下却是 `LEFT_CTRL`：

   ```python
   modmap("Caps mode - lctrl_role_swap - GUI",   {Key.CAPSLOCK: [Key.ESC, Key.LEFT_META]})
   modmap("Caps mode - lctrl_role_swap - Terms", {Key.CAPSLOCK: [Key.ESC, Key.LEFT_CTRL]})
   ```

   于是同一个物理键在 GUI 与终端下行为不同。

3. **GUI 下的 Ctrl 习惯被破坏。** 在 VSCode + vim 插件这类场景中，物理 Ctrl 发出的实际是 `Super+V`，vim 插件收不到 `Ctrl+V`，块选择失效。

此外 Toshy 的 7 个 `capslock_mode` 中**没有任何一个**能同时满足「Caps→真Ctrl」与「物理Ctrl→Caps」，因为其 `role_swap` 系列设计前提就是「左 Ctrl 的 GUI 角色是 Super」。

### 附带问题：内部实现泄漏

Toshy 把 Cmd 的内部槽位命名为 `RIGHT_CTRL`，且文档中 `Super`/`Cmd`/`Command`/`Meta` 是同义词。于是 `C("Super-e")` 看起来是正确写法，实际永远不工作——物理 `Cmd+E` 到达 keymap 层时已变成 `RIGHT_CTRL+E`。这类命名与实现的不一致会稳定地误导用户。

## 目标

1. **CapsLock ↔ 左 Ctrl 位置互换**，且两个键在所有环境下均发真 Ctrl，不产生 Super。
2. **Cmd 保持 `LEFT_META` 身份不变**，仅在命中映射表的组合上翻译为 Ctrl。
3. **Ctrl 恒等于 Ctrl**，GUI 与终端一致。终端内 `Ctrl+C` 中断进程、`Ctrl+L` 清屏、`Ctrl+A` 行首等原生行为完全不受影响。
4. **不修改 Cmd 的 Super 语义**：`Ctrl+Cmd+Space` 这类组合能正确表达为 `LEFT_CTRL + LEFT_META + Space`。
5. 表外的 Cmd 组合**原样透传**，交由桌面环境或应用自行解释。

## 核心模型

与 Toshy 的方向相反。

| | Toshy | mackbind |
|---|---|---|
|模型|默认改写 + 逐组合指定输出|默认透传 + 查表例外|
|Cmd 身份|`LEFT_META` → `RIGHT_CTRL`（全局）|始终 `LEFT_META`|
|修饰键槽位|五个键重排，互相挤占|不动|
|映射目标|每个组合单独指定|静态表，查表即用|
|超出表的部分|必须显式指定，漏了就没有|原样透传，天然正确|

mackbind 的逻辑可归结为三条规则：

```
收到 CAPSLOCK          → 发 LEFT_CTRL
收到 LEFT_CTRL         → 发 CAPSLOCK
收到 LEFT_META + 键    → 查表命中? 发 Ctrl+键 : 发 Super+键（透传）
收到其他               → 原样透传
```

规则之间无冲突，无先后依赖。这与 Toshy 需要处理「五个修饰键的完整重映射矩阵、6 种键盘类型、数千条组合规则、multipurpose 键时序判定」形成数量级上的差距。

## 修订：默认方向应当反转（待实现）

上文的「默认透传、查表例外」模型在实际使用中暴露了一个致命问题：**穷举不完**。

用户的原话：「cmd 组合键太多了，每个应用还不一样！不大可能穷举啊」。

当前实现是内置 23 条 `cmd_map` 白名单。这个数字本身就是设计失败的信号——真实的 Cmd 快捷键是长尾的，任何固定表都覆盖不全。未列入的组合（如 `Cmd+Shift+N`、`Cmd+Alt+P`）会原样透传为 `Super+…`，而在 Linux 上**没有任何应用识别 `Super+字母`**，结果是什么都不发生。

### 关键事实：Linux 桌面上 `Super` 只用于系统级操作

GNOME 当前 28 个 Super 绑定（`gsettings list-recursively org.gnome.desktop.wm.keybindings` 实测）：

```
窗口管理   Super+↑ ↓ ← →                最大化/最小化/平铺
           Super+Shift+方向             移到其他显示器
工作区     Super+Page_Up/Down         切换工作区
           Super+Shift+PgUp/Down      把窗口移到工作区
           Super+Home/End             跳到首/末工作区
应用切换   Super+Tab                   切换应用
输入法     Super+space                 切换输入法
其他       Super+h / Super+F1 / Super+↑ 等
```

**涉及字母键的仅 `<Super>h` 一项**（锁屏 `Super+l`、`Super+o` 为用户自设）。GNOME 刻意不用 `Super+字母`——字母键留给应用，Super 留给桌面环境管窗口。

这意味着 macOS 的 `Cmd+字母`（复制、保存、查找…）在 Linux 上**对应的从来不是 Super，而是 Ctrl**。「让 Cmd 保持 Super 本意」这个早期设计判断，对绝大多数组合而言只是让按键失效。

### 修正后的模型

```
默认       LEFT_META + 键  →  Ctrl + 键
例外 A     GNOME 已占用的 Super 组合 → 保持 Super   （从 gsettings 实时读取）
例外 B     终端窗口内        →  Ctrl+Shift + 键
```

例外清单很小且可自动推导，因此长尾组合（`Cmd+Shift+N` 等）**默认即正确**，无需枚举。

| | 当前实现 | 修订后 |
|---|---|---|
|默认行为|`Cmd+X` → 透传为 Super（无效）|**`Cmd+X` → `Ctrl+X`**|
|例外来源|穷举 23 条白名单|GNOME 绑定（自动）+ 终端（窗口类型）|
|长尾组合|失效|**默认生效**|
|显式覆盖|`cmd_map` 全部条目|`override`（仅非 Ctrl 目标，如 `Cmd+[` → `Alt+Left`）|

### 例外 B 的依据：终端本就能区分

一个曾被误判的问题：担心「`Cmd+C` 翻译为 `Ctrl+C` 会在终端里误触发 SIGINT」。

**这是错的。** macOS Terminal 里 `Cmd+C` 就是 `Ctrl+Shift+C`，Linux 终端同样支持 `Ctrl+Shift+C` 复制。两者一致，终端不需要任何特殊处理：

| |Linux 终端|macOS 终端|
|---|---|---|
|`Ctrl+C`|SIGINT 中断|SIGINT 中断|
|`Ctrl+Shift+C`|复制|复制|

因此 `Cmd+C` 在**终端内**的正确输出是 `Ctrl+Shift+C`，而非全局默认的 `Ctrl+C`。这是例外 B 的来源，也是当前实现缺失的一条规则（会导致默认反向后引入回归）。

### 关于「Cmd 与 Ctrl 无法区分」的代价

默认反向后，`Cmd+C` 与物理 `Ctrl+C` 产生**相同输出**（均为 `Ctrl+C`）。是否会误判？

**不会。** 解释权在应用而非键位：mackbind 只产出事件流，GTK 应用按自身菜单把 `Ctrl+C` 解释为复制，终端按 termios 的 `ISIG` 解释为 SIGINT。同一输出、不同解读，本就是 Linux 桌面的常态。

唯一受影响的场景是某个应用内要求 `Cmd+X` 与 `Ctrl+X` 触发不同动作（如 macOS 的 `Ctrl+Cmd+F` 全屏）。此类组合极少，且可通过 `override` 规则保留原样。

### 待确认

- 终端判定用 WM_CLASS 匹配，还是走 GNOME 扩展的 D-Bus 焦点信息
- 例外 A 中，GNOME 绑定变化时是否需要重启服务（`gsettings` 订阅 vs 启动时快照）

## 故障记录：输入法漏字母

### 现象

mackbind 运行时，输入法（fcitx5 + 小鹤双拼）打字漏字母，单个拼音音节无法合成，直接输出原始英文字母。表现为输入「发展」得到「fa vj」。

**与 mackbind 无关。** 根因是触控板 `tap-to-click`：

打字时手掌误触触控板被识别为鼠标点击，该位置的按键被吃掉。GNOME 虽有 `disable-while-typing`，但该功能依赖 GNOME 收到键盘事件；mackbind 抓走键盘后 GNOME 收不到，误判用户「未在打字」，因而不会自动禁用触控板。

解决：

```bash
gsettings set org.gnome.desktop.peripherals.touchpad tap-to-click false
gsettings set org.gnome.desktop.peripherals.touchpad tap-and-drag false
```

关闭后输入法恢复正常。

### 排查过程中走过的弯路

记录下来是因为这些方向**都看似合理但无效**，避免后来者重复：

|假设 | 实测结果 |
|---|---|
|批次切分导致输入法提前提交|无效。真实键盘同样每键一个 SYN，不是 mackbind 引入的|
|`InputEvent::new` 时间戳为 0 导致超时聚合失效|无效。evdev 文档明确：内核在读事件时会覆盖时间戳，注入的 0 不会到达输入法|
|参照 Toshy 加固定 9ms 按键间隔|**更严重**。Toshy 的 `throttle_delays` 是为解决按键粘滞，与输入法无关|
|按事件时间戳还原原始按键节奏|无效，且每个键都 sleep，程序变慢|
|转发 MISC 事件以保留触控板活动|**更严重**。MISC 下挂着 ABS 多点触控事件，一并送到未声明 `EV_ABS` 能力的虚拟设备，导致触控板与按键双双异常|

### 方法论教训

上述五次尝试的共同错误：**用「症状相似」推断「成因相同」**，把外部先例（Toshy 的实现、ptyxis 的协议栈）当作依据，而非从实际观测出发。

真正定位问题靠的是用户的一个观察——「打字时不小心碰到了触控板」——而非对 mackbind 日志的反复分析。mackbind 侧的事件流自始至终都是正确的。

后续排查此类问题时，应先确认「本程序输出的事件是否正确」，再怀疑下游；本例中日志显示事件完全正确，问题却依然存在，说明观察视角本身有盲区。

## 覆盖范围

基于 Macworld 完整快捷键表与 Apple 官方文档分类。

### L1 — 纯组合翻译（方案完整覆盖）

Cmd 作为修饰键，按下同键输出 Ctrl。应用自行解释，无需应用上下文。

```
Cmd+C / V / X / Z / A      →  Ctrl+C / V / X / Z / A     复制粘贴剪切撤销全选
Shift+Cmd+Z                →  Ctrl+Y                      重做
Cmd+F / G                  →  Ctrl+F / G                  查找 / 查找下一个
Shift+Cmd+G                →  Ctrl+G                      查找上一个
Cmd+S / O                  →  Ctrl+S / O                  保存 / 打开
Cmd+B / I / U / K          →  Ctrl+B / I / U / K          粗体 / 斜体 / 下划线 / 链接
Cmd+T                      →  Ctrl+T                      新标签
Cmd+W                      →  Ctrl+W                      关闭窗口
Cmd+P                      →  Ctrl+P                      打印
Cmd+R                      →  Ctrl+R                      刷新
Cmd+[ / ]                  →  Alt+Left / Alt+Right        浏览器后退 / 前进
Cmd+N                      →  Ctrl+N                      新建窗口
Shift+Cmd+P                →  Ctrl+Shift+P                打印相关
```

### L2 — 平台级功能（方案不处理，交由 DE）

这些是 macOS 特有功能，Linux 上要么有等价物，要么不存在。**不在映射表内，因此原样透传，不会与 GNOME 自身的绑定冲突。**

```
Shift+Cmd+3 / 4 / 5   截图            → GNOME 自带 PrtSc 体系
Cmd+Space             Spotlight        → GNOME 无等价物
Cmd+Del               移到废纸篓        → 依应用而定
Ctrl+Cmd+Q            锁屏             → Super+L
Fn 系列               显示台/Dictation → 无等价物
Ctrl+Up / Down        Mission Control  → GNOME Super+Up/Down
```

### L3 — Finder 专属（待实测，不在 v1）

Nautilus 绑定与 Finder 不同，需逐条核对。优先级低，因为有鼠标路径可达。

```
Cmd+D 复制 / Cmd+E 弹出 / Cmd+I 显示信息 / Cmd+1~4 视图切换
Shift+Cmd+N 新建文件夹 / Shift+Cmd+H 家目录 / Cmd+, 偏好设置
```

### Emacs 绑定（保留不动）

macOS 上 `Ctrl` 有独立的 Emacs 风格绑定。本方案不触碰 Ctrl，因此这些**天然可用**：

```
Ctrl+A 行首   Ctrl+E 行尾   Ctrl+F 前进   Ctrl+B 后退
Ctrl+P 上行   Ctrl+N 下行   Ctrl+L 清屏   Ctrl+D 删字符
Ctrl+K 删到行尾            Ctrl+W 删词
```

## 实现方案

### 为什么是 evdev + uinput

在 GNOME Wayland 下，XKB 层不可用：

|层级|机制|覆盖范围|应用区分|
|---|---|---|---|
|XKB（`setxkbmap`）|X 服务器内部 keymap|**仅 XWayland 应用**|不可|
|evdev grab + uinput|用户态进程截获事件|全局，含原生 Wayland|可（需焦点查询）|

mackbind 走第二条路，与 Toshy 相同的技术栈，但逻辑远为简单。

### 组件

```
1. 设备枚举与抓取
   - 枚举 /dev/input/event*，识别键盘
   - device.grab() 独占（需 input 组）

2. 修饰键状态机
   - 记录当前按下的修饰键集合
   - 处理左右侧区分（LEFT_CTRL / RIGHT_CTRL）

3. 映射决策
   - 单键规则：CapsLock ↔ LEFT_CTRL（静态）
   - 组合规则：LEFT_META + 键 → 查表（静态）
   - 表外透传

4. 虚拟设备注入
   - uinput 建虚拟键盘
   - 重放转换后的事件

5. 终端检测
   - 读 GNOME Shell 扩展的焦点窗口信息
   - 终端类窗口中禁用所有映射
```

### 关键技术点

**修饰键按下时不立即输出。** 收到修饰键先记入状态并抑制输出；收到非修饰键时，结合完整修饰键集合做决策，决定输出内容。

**释放事件需与按下时决策一致。** 转换后的修饰键，其释放事件必须按转换后的身份发出，否则会残留按下状态。

**终端排除只需窗口类型。** 这是唯一需要应用上下文的地方，且只需判断「是不是终端」，不需要「绑定了哪些键」。映射目标是静态表决定的，应用只需要收到正确的 `Ctrl+X`，会自行正确解释。

GNOME Wayland 下需 Shell 扩展提供焦点窗口信息。本机已安装 `focused-window-dbus@flexagoon.com`，但实测其 bus name 当前返回 `ServiceUnknown`，**需在实现阶段确认可用性**。若不可用，降级方案：读取 `GNOME_APPLICATION`/`XDG_ACTIVATION_TOKEN` 等替代来源，或接受无应用区分。

## 实现选型

| | Rust | Python |
|---|---|---|
|分发|单文件二进制，无运行时依赖|需解释器 + 依赖|
|evdev 绑定|`evdev` crate（需引入 libc 细节）|`python-evdev`（成熟，但本机仅 Toshy venv 内有）|
|开发速度|较慢|快|
|长期可维护|优|可|

倾向 **Rust**：无依赖分发对输入法级别的工具是实质优势，避免用户为了一百行映射逻辑去装 Python 生态。

依赖清单：`evdev`（设备访问与 uinput）、`serde` + `toml`（配置解析）。均为成熟 crate。

注：`evdev` crate **自带** `Device::grab()` / `ungrab()`，无需自行实现 `EVIOCGRAB` ioctl，`libc` 依赖可省去。

## 事件流实测结论

以下为在 MacBook Pro 2015（`/dev/input/event6`，`Apple Inc. Apple Internal Keyboard / Trackpad`）上的实测结果，是实现的事实基础。

### 修饰键键码

|物理键|键码|
|---|---|
|Cmd|`KEY_LEFTMETA` (125)|
|左 Ctrl|`KEY_LEFTCTRL` (29)|
|右 Ctrl|`KEY_RIGHTCTRL` (97)|
|Option|`KEY_LEFTALT` (56)|
|CapsLock|`KEY_CAPSLOCK` (58)|
|Fn|`KEY_FN` (464)|

**Cmd 就是 `KEY_LEFTMETA`**，与 PC 的 Win 键同码。这是整个设计的立足点：macOS 的 Cmd 与 Linux 的 Super 在硬件层无需转换，映射表可直接基于 `LEFT_META` 构建。

### Fn 键可见，可用于用户态优先级切换

`KEY_FN` (464) 在设备能力列表中声明，且作为普通 key event 到达：

```
单独 Fn       → 464 press(1), release(0)
按住 Fn       → 464 press(1), repeat(2) × N, release(0)
Fn + F1       → 464 press(1) 与 59 press(1) 独立到达
```

内核**不做合成**——`Fn+F1` 时 `KEY_FN` 与 `KEY_F1` 分别上报，而非直接给出 `KEY_BRIGHTNESSDOWN` (224)。

因此 **fn 按键优先级可在用户态实现**，两种模式（`fkeysfirst` / `fkeyslast`）都无需 root 或修改 `hid_apple` 内核参数：

```
Fn 未按住 + F1 →  发 KEY_F1
Fn 按住   + F1 →  发 KEY_BRIGHTNESSDOWN (224)
```

这推翻了「fnmode 只能通过 `/sys/module/hid_apple/parameters/fnmode` 切换、需root」的假设。

### 实现注意事项

**必须处理自动重复。** 修饰键按住会产生大量 `value=2` 的 repeat 事件（如上例中 `464 2` 连续多次）。这些不能重复触发映射，否则会产生成对的按下/释放。

**必须过滤 `MISC` 事件。** 实测每个按键前后都有 `MISC` 事件（MacBook 键盘背光/触摸板杂散事件），不属于按键语义，应透传或丢弃。

**释放事件须与按下时决策一致。** 转换后的修饰键，其 release 事件必须按转换后的键码发出，否则输出侧会残留按下状态。

## 阶段划分

### v1 — 最小可用

- CapsLock ↔ 左 Ctrl 互换，两个方向均发真 Ctrl
- L1 表中约 18 条组合映射
- 表外组合原样透传
- 过滤 `MISC` 事件与 `value=2` 自动重复事件
- 无应用区分（先验证 Ctrl 是否恢复正常）

**验证目标**：VSCode + vim 插件中 `Ctrl+V`、`Ctrl+A`、`Ctrl+W` 恢复正常；终端中 `Ctrl+C` 中断进程正常。

这一步独立于后续功能即可解决核心痛点。即使 v2 不做，项目已有实际价值。

### v1.5 — Fn 键优先级

- 基于 `KEY_FN` 按下状态，在用户态实现 F 键 / 媒体键切换
- 配置项选择 `fkeysfirst`（F 键优先，Fn 组合出媒体键）或 `fkeyslast`（媒体键优先，Fn 组合出 F 键）
- 需维护 F1–F12 与亮度/媒体键的对应表

该功能不依赖内核 `fnmode` 参数，纯用户态实现。

### v2 — 终端排除

- GNOME 焦点窗口查询
- 终端类窗口内禁用映射

**验证目标**：终端内 `Cmd` 组合不产生意外行为；`Ctrl` 系列完全正常。

### v3 — 完善

- L3 Finder 映射逐条核对
- 配置热重载
- 开机自启

## 风险

|风险|缓解|
|---|---|
|grab 失败导致键盘失灵|启动时验证抓取结果，失败则退出并报错，不静默挂起|
|修饰键状态泄漏|明确释放路径；退出时释放所有按下的键|
|与 Toshy 冲突|两者不能同时 grab 同一设备，需停用 Toshy 或显式检测|
|Wayland 焦点信息不可得|降级为无应用区分（v1 本就无此依赖）|

**与 Toshy 的关系**：两者不能共存于同一设备。mackbind 验证通过后停用 Toshy。若 mackbind 未达预期，Toshy 配置已备份在 `~/.config/toshy/toshy_user_preferences.sqlite.bak-20261009-174016`，可回滚。

## 待确认

- [ ] L1 表中 `Cmd+N` 在终端语义冲突（新标签 vs 新建进程），是否按应用区分
- [ ] `Cmd+P` 在 VSCode 中是「跳转文件」而非「打印」，是否按应用区分
- [ ] L3 Finder 映射是否纳入 v3 之后
- [ ] 项目名 `mackbind` 是否最终采用