# 表驱动重构方案

**状态：已完成（行为等价）。** 下文保留「为什么这么做」，
包括几处不那么显然的设计取舍。

## 为什么要重构

重构前，「一个 Cmd 组合该输出什么」由**六套并行机制**共同回答：

|机制|位置|回答什么|
|---|---|---|
|`cmd_map`|`config.rs`|全局显式规则|
|`app_cmd_map`|`config.rs`|按应用覆盖|
|`terminal_shift_keys` + `ctrl_busy_apps`|`config.rs`|终端内给部分键加 Shift|
|默认语义|`mapper.rs` 分支|`Cmd+X → Ctrl+X`|
|DE 特性规则|`mapper.rs` + `accel.rs`|读 DE 现有绑定后发出|
|运行时 GSettings|`desktop.rs` / `accel.rs`|终端判定与快捷键发现|

它们互相咬合，优先级靠 `mapper.rs` 里的 if-else 链维持：

```
app_cmd_map  >  cmd_map  >  终端特化  >  默认语义
```

问题不在于行数多，而在于**「做了什么」无法一眼看出**。这直接导致了
`scripts/audit.py` 首轮查出 11 条文档声称需实现而代码没有、2 条分类错误。

## 现在的结构

一张表回答所有问题，29 行：

|应用|按键组合|动作|
|---|---|---|
|`*`|`Cmd+Q`|`emit` `Alt+F4`|
|`*`|`Cmd+[`|`emit` `Alt+Left`|
|`*`|`Cmd+空格`|`de_accel` `overview`|
|`org.gnome.Ptyxis` 等|`Cmd+C`|`emit` `Ctrl+Insert`|
|`gvim`|`Cmd+任意键`|`terminal_ctrl_shift`|
|`TerminalEmulator`|`Cmd+任意键`|`terminal_ctrl_shift`|

**顺序即优先级**——原先散落在三处的优先级关系，现在只由规则在表中的
位置决定。查找是线性扫描取首个命中者；用户配置追加在末尾，故天然优先。

`mackbind --verbose` 启动时打印整张表。

## 动作即函数

关键点：**动作不是枚举的变体，而是函数**。因此表不限于静态映射。

|动作|职责|
|---|---|
|`emit`|直接产出按键事件|
|`de_accel`|查 DE 现有绑定，发出同样的键|
|`terminal_ctrl_shift`|查目标应用自己的绑定，决定是否加 Shift|

后两者内部做运行时查找。这不是妥协，是正确的分层：**表负责命名，
函数负责求值**。硬塞静态快照才会过期——那正是文档漂移的病因。

### `terminal_ctrl_shift` 与通配主键

这一条最能体现「静态表 + 运行时求值」的必要性。

终端的 `Ctrl+Shift` 绑定是**运行时**从它的 GSettings 查得的，各终端不同：
ptyxis 声明了 `new-window = Ctrl+Shift+N`，而 `N` 不在任何静态名单里。
若逐键列举规则表，运行时发现的键将无处安放——这正是 commit `18c03a2`
之前的缺陷（当时靠整体替换 `terminal_bindings` 解决，代价是该逻辑无法
表达成数据）。

故表中用 `keys = ["*"]` 通配主键，由动作在运行时判定具体哪些键生效。
兜底名单 `DEFAULT_TERMINAL_SHIFT_KEYS` 只在查不到 schema 时使用
（kitty、wezterm 等不走 GSettings 的终端）。

### `resolve_fn` 不在表里

F1–F12 的含义取决于 Fn 状态与**完整修饰键上下文**（macOS 上
`Cmd+Shift+F1` 需按 `Cmd+Shift+Fn+F1`），是**按键级**变换而非组合规则，
故留在表外——但同样已统一为一个函数 `resolve_fn`，且**只做替换不短路**。

它此前曾用 `return` 短路，绕过整个查表流程，实测
`Fn+Meta+Shift+F1` 输出 `Meta+媒体键`：Meta 泄漏（本应被抑制），
Ctrl/Shift 丢失。已修复并加 6 个测试。

## 表外的部分（必须显式声明）

不是所有行为都是行。以下两者留在表外：

1. **修饰键重映射** —— CapsLock ↔ LeftCtrl 成对互换。发生在组合判定**之前**，
   作用于按键本身而非组合。表中 `keys` 一列无法表达「一个物理键被换成另一个」。
2. **按键释放对称性** —— 释放事件须与按下时决策一致（否则输出侧残留按下态）。
   这是状态机职责，不该表达成行。

## 应用列的取值

|取值|含义|
|---|---|
|`*`|所有应用|
|`TerminalEmulator`|按 `.desktop` 的 `Categories` 判定（freedesktop 规范）|
|`gvim` 等 wm_class|按子串匹配窗口类|

`TerminalEmulator` 这个取值是关键——装新终端自动生效，不需要维护名单。
这条能力保留自 `238c703`，不要退回硬编码列表（曾因写成小写 `ptyxis`
而漏判实际的 `org.gnome.Ptyxis`）。

## 重构中踩到的两个坑

两者都因「顺序即优先级」这一新性质而暴露，值得记下：

1. **更具体的规则必须排在更通用的之前。** ptyxis 的 `Ctrl+Insert`
   原先放在通用终端规则之后，永远命中不了——因为查找取**首个**命中者。
2. **同一段规则被复制成两份。** 修复上述问题时新增了一个
   `TerminalEmulator` 循环，却忘了删原来的，导致通用规则仍排在前面。

教训：改动规则表后，用 `mackbind --verbose` 打印实际顺序核对，
不要凭记忆。

## 前提：每行都要有实测依据

表格只有在「每行都知道为什么存在」时才有价值。当前文档 11 条缺失里，
多条经查证根本无法按文档所述实现：

|文档条目|实际|
|---|---|
|`Ctrl+Cmd+Q` 锁屏 → `Super+l`|GNOME 51 **无 `lock-screen` 绑定键**，锁屏不在 wm.keybindings schema 里。应标 **F**|
|`Cmd+Shift+3/4/5` 截图 → `Print`|GNOME 现有绑定为 `Print`（截图UI）/ `Alt+Print`（窗口）/ `Shift+Print`（区域），与文档写法不符|
|`Ctrl+Cmd+F` 全屏 → `F11`|可实现。`toggle-fullscreen` 存在但未绑定（`@as []`），发 `F11` 即可|

这些尚未处理——**下一步就是逐条实机验证，据结果决定表里增删哪些行**。