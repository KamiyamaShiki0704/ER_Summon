# Summon

中文 | [English](README.en.md)

用于《艾尔登法环》的独立召唤 DLL。通过施加给玩家的 SpEffect 触发 NPC 召唤，可配置模型、参数、动画、阵营、位置、存活时间和事件实体 ID。

## 安装

从 Releases 下载压缩包，将 `summon.dll` 和 `summon.toml` 放在同一个 DLL 加载目录中：

```text
Summon/
├── summon.dll
└── summon.toml
```

已有自定义配置时，先备份配置，再更新 DLL。配置在启动时读取，修改后需要重启游戏。

## 兼容性

当前实体注册与内存释放功能已在 Worldwide `2.7.1.0`（App Ver. 1.17.1）实测。
初始化版本检查也识别 Worldwide `2.7.0.0` 和 Japanese `2.7.0.1`，但本次新增的原生注册与释放路径未在这些版本上实测。原生函数校验不通过时会拒绝相关操作，不能据此保证旧版本具有相同功能。

## 配置

完整默认配置见 [summon.toml](summon.toml)。全局配置放在第一个 `[[summons]]` 之前。

```toml
fallback_lifetime_ms = -1
remove_mode = "destroy"
reuse_spawned_units = false
vanish_request_speffect = 14200129
log_enabled = true
```

- `fallback_lifetime_ms = -1`：无限存活。在有效地图中，检测到死亡或指定消失 SpEffect 后开始清理。
- 非负存活时间以毫秒计；`0` 按最小 1 毫秒处理。内置默认值为 10000 毫秒，随包配置使用 `-1`。
- `remove_mode = "destroy"`：注销角色任务、从角色集合脱离，等待引用检查通过后析构并释放内存。此模式不复用单位。
- `remove_mode = "hide"`：隐藏并保留角色；配合 `reuse_spawned_units = true` 可复用。
- `destroy_delay_ms`：开始帧末脱离前的最短等待时间；实际释放还需等待引用检查通过。
- `vanish_request_speffect`：施加给召唤单位自身的消失效果，不是施加给玩家的召唤触发效果。

无限存活不代表跨地图保存。切图、世界或玩家重置期间，尚未释放的脱离对象可能保留到进程退出。释放角色实例不保证卸载模型缓存或其产生的子弹。

### 召唤条目

每个 `[[summons]]` 定义一个召唤类型。主要字段：

| 字段 | 用途 |
| --- | --- |
| `trigger_speffect` | 施加给本地玩家的触发效果，检测后消费一次 |
| `chr_id` | 角色模型编号，例如 4205 对应 c4205 |
| `chara_init_param_id` | CharaInitParam 参数 |
| `npc_param_id` / `npc_think_param_id` | NPC 参数和 AI 参数 |
| `event_entity_id` | 事件脚本控制使用的实体 ID；0 不注册 |
| `animation_id` | 激活时播放的动画 |
| `talk_id` / `is_player` | 对话 ID 与创建类型 |
| `offset_forward` | 沿相机水平朝向偏移，正数向前 |
| `offset_right` | 沿相机水平方向偏移，正数向右 |
| `offset_up` | 世界竖直偏移，正数向上 |

条目可覆盖全局的 `marker_speffect`、`vanish_request_speffect`、`generated_team_type`、`fallback_lifetime_ms`、三个位置偏移和 `disable_lock_on`。省略时继承全局值。

单位激活时朝向玩家当前的水平方向。位置偏移使用相机方向，二者分别计算。
`disable_lock_on = true` 禁止锁定整个召唤角色。
兼容旧位置字段 `spawn_forward_distance`、`side_offset`、`spawn_height_offset`；同一配置范围内不要同时填写新旧名称。

### 事件实体 ID

为需要事件控制的条目设置一个未被占用的正整数 ID。例如：

```toml
# 在目标 [[summons]] 条目内设置
event_entity_id = 13000890
```

此数字仅为示例，使用前应确认自己的地图与事件没有占用它。注册包括角色字段、实体映射及原生分组注册。

EMEVD 控制示例：

```js
ForceAnimationPlayback(13000890, 20012, false, false, false);
```

同时存在且需要分别控制的单位应使用不同 ID。同一角色集合内 ID 已被另一单位占用时，新单位的注册会被拒绝。原单位完成清理后可再次使用该 ID。

### 其他全局设置

- `max_active_summons`：活动召唤上限。
- `start_delay_ms`：DLL 加载后开始初始化的延迟，默认 5000 毫秒。
- `unbound_timeout_ms`：创建请求等待绑定角色的超时。
- `same_trigger_cooldown_ms` / `any_trigger_cooldown_ms`：同条目与公共触发冷却。
- `marker_speffect`：召唤单位的标记效果。
- `generated_team_type`：生成单位的阵营。

### 日志

`log_enabled = true` 会在 DLL 目录写入 `summon.log`。

- `entity_id_outcome=conflict`：实体 ID 已被占用。
- `group_after=1`：当前 ID/单位的分组映射已建立。
- `local_set_id_resolves_to_this_unit=true`：本地角色集合映射能找到该单位，不代表所有事件指令都已验证。
- `outcome=destroyed retirement=frame-end-drained`：角色最终释放已返回。

## 构建

需要 Rust、`x86_64-pc-windows-msvc` 目标和 Visual Studio C++ 构建工具。

```powershell
cargo test --locked
cargo build --release --locked
```

输出位于 `target/x86_64-pc-windows-msvc/release/summon.dll`。
Release 静态链接 MSVC CRT，并保留调试信息。安装 Windows SDK 后可由 `build.rs` 嵌入版本资源；找不到 `rc.exe` 时会警告并跳过该资源。

依赖由 Cargo 获取，固定到以下版本；首次构建需要网络连接：

```text
https://github.com/KamiyamaShiki0704/fromsoftware-rs
02fa5681e27fd2ecd9da79d34aef9fae96805539
```

## 许可与致谢

使用 MIT 或 Apache-2.0 许可。运行时绑定基于 [fromsoftware-rs](https://github.com/vswarte/fromsoftware-rs)，依赖保留各自的许可条款。
