# 表驱动重构方案

## 为什么要重构

当前「一个 Cmd 组合该输出什么」这个问题，由**六套并行机制**共同回答：

|机制|位置|回答什么|
|---|---|---|
|`cmd_map`|`config.rs`|全局显式规则|
|`app_cmd_map`|`config.rs`|按应用覆盖|
|`terminal_shift_keys` + `ctrl_busy_apps`|`config.rs`|终端内给部分键加 Shift|
|默认语义|`mapper.rs` 分支|`Cmd+X → Ctrl+X`|
|DE 特性规则|`mapper.rs` + `accel.rs`|读 DE 现有绑定后发出|
|运行时 GSettings|`desktop.rs` / `accel.rs`|终端判定与快捷键发现|

它们互相咬合，优先级靠 `mapper.rs:465-487` 的 if-else 链维持：

```
app_cmd_map  >  cmd_map  >  终端特化  >  默认语义
```

问题不在于行数多，而在于**「做了什么」无法一眼看出**。这直接导致了
`scripts/audit.py` 首轮查出 11 条文档声称需实现而代码没有、2 条分类错误。

## 目标结构

一张表回答所有问题：

|应用|按键组合|动作|
|---|---|---|
|`*`|`Cmd+X`|`emit` `Ctrl+X`|
|`*`|`Cmd+Q`|`emit` `Alt+F4`|
|`*`|`Cmd+[`|`emit` `Alt+Left`|
|`*`|`Cmd+空格`|`de_accel` `overview`|
|`TerminalEmulator`|`Cmd+C`|`term_accel` `copy`|
|`TerminalEmulator`|`Cmd+W`|`emit` `Ctrl+Shift+W`|
|`org.gnome.Ptyxis`|`Cmd+C`|`emit` `Ctrl+Insert`|
|`gvim`|`Cmd+V`|`emit` `Ctrl+Shift+V`|

三套配置项（`cmd_map` / `app_cmd_map` / `terminal_shift_keys` + `ctrl_busy_apps`）
合并为一张。`app_cmd_map` 的存在理由是「行为差异」，而差异已被 `应用` 列表达——
`Ctrl+Insert` 那条从特例降为普通一行。

## 动作即函数

关键点：**动作不是枚举的变体，而是函数**。因此表不限于静态映射。

|动作|签名|职责|
|---|---|---|
|`emit`|`(combo) -> events`|直接产出按键事件|
|`de_accel`|`(feature) -> events`|查 DE 现有绑定，发出同样的键|
|`term_accel`|`(action) -> events`|查终端 GSettings schema|
|`resolve_fn`|`(key, ctx) -> key`|F1–F12 在当前 Fn 状态下到底是什么键|

后三者内部做运行时查找。这不是妥协，是正确的分层：**表负责命名，
函数负责求值**。硬塞静态快照才会过期——那正是当前文档漂移的病因。

### `resolve_fn` 为什么必要

macOS 上 `Cmd+Shift+F1` 需按 `Cmd+Shift+Fn+F1` 才能拿到功能键。这意味着
F 键的含义取决于**完整修饰键上下文**，不能只看 F1 本身。

当前实现（`mapper.rs:427`）在 Cmd 处理**之前**插入 Fn 分支并 `return`，
绕过了整个查表流程。实测：

```
Fn + Meta + Shift + F1  →  Meta + 媒体键
```

`Meta`（42）被原样发出去了——因为 Meta 此前已被抑制（不输出），这里却
泄漏成一个孤立的按下事件；同时 `Ctrl`/`Shift` 丢失。**这是当前代码里的
真实 bug**，不是设计洁癖问题。

正确形态是预处理只做**替换**、不短路：

```rust
let code = self.resolve_fn(code);   // F1 → 媒体键，或反之
// 之后统一走 Cmd 查表 / 默认透传
```

`fn_mode` 因此不是一行规则，而是传入 `resolve_fn` 的参数：

|输入|`fkeysfirst`|`fkeyslast`|
|---|---|---|
|F1 单独|F1|媒体键|
|Fn+F1|媒体键|F1|
|Fn+Meta+Shift+F1|Meta+Shift+媒体键|Meta+Shift+F1|

第三行当前是坏的。

## 表外的部分（必须显式声明）

不是所有行为都是行。以下两者留在表外，但**必须在表附近标注其存在**，
否则读者会以为表是全集：

1. **修饰键重映射** —— CapsLock ↔ LeftCtrl 成对互换。发生在组合判定**之前**，
   作用于按键本身而非组合。表中 `按键组合` 一列无法表达「一个物理键被换成另一个」。
2. **按键释放对称性** —— 释放事件须与按下时决策一致（否则输出侧残留按下态）。
   这是状态机职责，不该表达成行。

## 匹配的优先级

从 if-else 链改为显式规则，行为保持不变：

```
1. 修饰键重映射        （按键级，最先）
2. Fn 键预处理          （按键级，替换后继续）
3. 应用专用规则         （应用列 = 当前 wm_class）
4. 全局显式规则         （应用列 = *）
5. 默认语义 Cmd+X→Ctrl+X
6. 透传
```

第 3 与第 4 步之间靠「应用列精确匹配优先于 `*`」实现，无需 if 链。

## 应用列的取值

|取值|含义|
|---|---|
|`*`|所有应用|
|`TerminalEmulator`|按 `.desktop` 的 `Categories` 判定（freedesktop 规范）|
|`gvim` 等 wm_class|精确匹配窗口类|

`TerminalEmulator` 这个取值是关键——装新终端自动生效，不需要维护名单。
这条能力保留自 `238c703`，不要退回硬编码列表（曾因写成小写 `ptyxis`
而漏判实际的 `org.gnome.Ptyxis`）。

## 迁移步骤

分步进行，每步保持可运行、可测试：

1. **修 Fn bug**：抽出 `resolve_fn`，去掉 `return` 短路，加 3 个测试
   （F1 单独 / Fn+F1 / Fn+Meta+Shift+F1）。独立于重构。
2. **建表**：把现有三套规则的默认值搬进 `Table`，实现按 `应用` 列匹配。
   行为不变，`mapper.rs` 决策链换成查表。
3. **合并动作**：`de_accel` / `term_accel` 收进同一接口。
4. **删除旧配置项**：`app_cmd_map` / `terminal_shift_keys` / `ctrl_busy_apps`
   标记 deprecated，一个版本后移除。

## 前提：每行都要有实测依据

表格只有在「每行都知道为什么存在」时才有价值。当前文档 11 条缺失里，
多条经查证根本无法按文档所述实现：

|文档条目|实际|
|---|---|
|`Ctrl+Cmd+Q` 锁屏 → `Super+l`|GNOME 51 **无 `lock-screen` 绑定键**，锁屏不在 wm.keybindings schema 里。应标 **F**|
|`Cmd+Shift+3/4/5` 截图 → `Print`|GNOME 现有绑定为 `Print`（截图UI）/ `Alt+Print`（窗口）/ `Shift+Print`（区域），与文档写法不符|
|`Ctrl+Cmd+F` 全屏 → `F11`|可实现。`toggle-fullscreen` 存在但未绑定（`@as []`），发 `F11` 即可|

所以：**先实机验证建立 ground truth，再据结果决定表里每一行**。
在未验证的结论上重构，只会把猜测固化成代码。