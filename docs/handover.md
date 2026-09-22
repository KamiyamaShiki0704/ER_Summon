# 改造交接文档 —— Summon DLL（2026-09-22）

**仓库**：`FSTool/_Mod/_Project/Summon`（fork 自 `KamiyamaShiki0704/ER_Summon`）
**本次范围**：三个提交 `500756e` / `2e7c242` / `b7bb147`，远端 `main` 已同步至 `b7bb147`
**上游基线**：`ffbdf98`（推送前的远端状态）
**产物**：`target/x86_64-pc-windows-msvc/release/summon.dll`，684,544 字节
SHA256 `2188a999d9f87d63c7436d8020b5971662e984c624fa148e4c277c78b5aa13bc`

---

## 0. 原始需求

本次要解决三件事：

1. 召唤单位要有**可用于事件检测控制的指定实体 ID** —— 事件脚本按 id 解析回该单位；
2. 单位消失时**从内存真实清除**，不残留在地图内存里；
3. （排查中追加）加载 DLL 有概率触发 iOA / 其他杀软误报，而同目录其他游戏 DLL 不会。

前两条已完整落地；第 3 条完成了仓库内能做的部分，实机确认待做（见 §7）。

## 1. 提交清单

| 提交 | 摘要 | 涉及文件 |
| --- | --- | --- |
| `500756e` | 实体 ID 注册 + 真实内存移除 | `src/chr_lifecycle.rs`（新增 442 行）、`src/lib.rs`、`summon.toml`、`README.md`、`docs/compatibility/engine-chr-lifecycle.md`、`research/*.py`（9 个逆向脚本） |
| `2e7c242` | `destroy_summon` 每次移除只解析一次 character set entry（清理重复查询） | `src/chr_lifecycle.rs` |
| `b7bb147` | 发布产物改为普通开发构建形态 + VERSIONINFO 资源（AV 误报治理） | `Cargo.toml`、`build.rs`（新增 162 行）、`README.md`、`docs/compatibility/av-false-positive.md` |

18 个文件，+2138 / −32 行。

## 2. 代码改动地图

### 2.1 `src/lib.rs`（接入层）

新增：

- `mod chr_lifecycle;` 与导入；
- 日志设施：`LOG_FILE_NAME = "summon.log"`、静态 `LOG`、`log_line()`（受 `log_enabled` 控制，写在 DLL 同目录）；
- `RemoveMode` 枚举（`Hide` / `Destroy`，默认 `Destroy`）；
- `AttackSummonSettings` 新字段：`remove_mode`、`destroy_delay_ms`、`register_event_entity_id`、`log_enabled`；
- `reuses_units()`：仅当 `reuse_spawned_units && remove_mode == Hide` 时为真；
- `AttackSummonInstance.destroy_at: Option<Instant>`。

改动：

- `update_existing`：Destroy 模式走两阶段（先惰化 + 释放 id，延迟到期才 free）；Hide 模式保留原 park / 复用路径；
- `release_all`：签名改为 `(&mut self, world_chr_man, settings)`，Destroy 模式下真正删除并记日志；
- `activate_or_spawn`：复用判定改用 `reuses_units()`；`destroy_at.is_some()` 的实例绝不复活；
- `bind_last_created`：绑定成功后注册实体 ID，输出 `summon bound` 日志（含 `entity_id_outcome`、`id_resolves_to_this_unit`）；
- `install_tasks_once`：启动摘要日志（生效的 remove_mode / 实际复用开关 / 条目数）。

### 2.2 `src/chr_lifecycle.rs`（新增模块，核心）

封装全部经过逆向验证的引擎级操作。公开接口：

| 接口 | 作用 |
| --- | --- |
| `register_event_entity_id()` | 写 `ChrIns::event_entity_id` + 插入 `ChrSet::entity_id_mapping`；返回 `EntityIdOutcome`（`NotConfigured` / `AlreadyRegisteredByEngine` / `RegisteredByDll` / `Conflict` / `Unavailable`） |
| `release_event_entity_id()` | 清扫式释放：删除所有指向该 entry 的 id 映射 |
| `entity_id_owner()` | 解析 id → 归属 handle，用于"能否解析回本单位"的断言 |
| `chr_set_usage()` | 读取 set 当前 id 数与容量（日志用） |
| `destroy_summon()` | 真实移除：释放 id → 清锁定 → 清 creator 指针 → 调引擎析构 → 清 entry；返回 `DestroyOutcome` |
| `release_hidden_summon()` | 惰化 + 释放 id（两阶段的第一阶段） |

**三条关键设计**（接手必须理解，否则容易改出 use-after-free）：

1. **entry 三重交叉验证**：handle 索引 → entry 是否真的拥有该 chr → chr 反指针。前两源必须一致，否则拒绝操作（`DestroyOutcome::EntryMismatch`），杜绝用陈旧 handle 释放错对象；
2. **清 entry 是最后一步**：引擎后续整集 teardown 会跳过空槽，不会二次 free；
3. **不硬编码 RVA**：析构地址取自对象自身 vtable（slot 1），随版本变化自动跟随。

### 2.3 `build.rs`（新增）

用 Windows SDK `rc.exe` 编译 VERSIONINFO 资源，作为 link 参数传给 cdylib。

- 扫描 `Windows Kits\10\bin` 定位 `rc.exe`（也接受 `RC` 环境变量）；
- 找不到时打警告跳过，**不破坏构建**，SDK 不是硬依赖；
- 无新增 crate 依赖（内网可用）。

### 2.4 配置与文档

- `summon.toml`：新增 `remove_mode`、`destroy_delay_ms`、`register_event_entity_id`、`log_enabled` 四个全局项（均带注释）；各 `[[summons]]` 保留 `event_entity_id`（当前全为 `0`，见 §7）；
- `README.md`：新增 Entity ids / Removal / Log 三节；Build 节补 `Artifact shape`；
- `docs/compatibility/engine-chr-lifecycle.md`（新增）：角色生命周期逆向证据；
- `docs/compatibility/av-false-positive.md`（新增）：AV 误报取证、已排除假设、处置与残余风险；
- `docs/compatibility/er-1171.md`：修正失效安装路径（`F:` 盘已不存在 → `G:`，目标哈希未变，结论仍成立）。

## 3. 三个机制的原理

### 3.1 实体 ID 如何生效

`ChrSet::entity_id_mapping`（`ChrSet + 0x28`）是 `事件ID → ChrSetEntry` 的有序映射，引擎解析 id 就查它。debug 创建路径**不保证**把配置的 id 同时写进 `ChrIns::event_entity_id` 与映射，因此模块三件事都做：写字段、插映射、**再解析一遍验证能回到本单位**（结果记进日志）。已被同集其他角色占用的 id **拒绝窃取**并报 `conflict`。

### 3.2 真实移除的顺序

引擎**没有单角色释放接口**（ChrSet 虚表只有 slot 8 `free_chr_list` 会释放，且是整集释放；slot 9 是一句 `ret 0`）。所以 DLL 走与引擎等价的路径：

```text
dtor(chr, 1)   ← 标量删除析构（ChrIns vtable slot 1，RVA 0x3E81F0）
                 0x1403E81FF: call 0x3E7970      ; 析构本体
                 0x1403E8209: mov  edx, 0x580   ; ChrIns 大小
                 0x1403E8211: call operator delete
```

与引擎 `free_chr_list` 内部的两步释放等价 —— 先 `dtor(chr, 0)`，再经归属分配器的虚表 slot `0x68` 释放内存 —— 释放的是**同一块堆内存**（exe 未导入任何 CRT DLL，CRT 静态链接，两条路径都落到 MSVC `_free_base` / `HeapFree`）。

两阶段执行：先 `hide + 释放 id`，下一 tick（或 `destroy_delay_ms` 之后）才 free —— 避免其它系统正在遍历它。

### 3.3 AV 误报的根因与处置

差分取证（与从不被报的兄弟 mod DLL 对比）后，**排除**了签名、VERSIONINFO、静态 CRT、调试目录、节熵、节数等 6 个假设。真正差异有两个：

1. **构建形态**：旧 profile 是 `strip=true + opt-level="z" + lto + codegen-units=1`（体积最小化 = 加壳载荷特征）；对照组是 `debug=2 + strip="none"`（开发构建形态）。**已改**；
2. **独有字符串**（Rust release 下 panic 消息与 `file!()` 路径保留在 `.rdata`，`strip` 删不掉）：`VMT address not in executable!`、`Call target for REGISTER_TASK was not in exe`、`chr_ins.rs` 等 —— 全是"对游戏进程做内存操作"的自我描述，正是通用检测规则的靶子。源码在依赖库的必需路径上（`Subclass` 的 `expect()`、`CSTaskImp` 注册），**代码层删不掉**。

处置：改 profile + 加 VERSIONINFO。应急手段（实测可把上述字符串全部清零）：nightly 的 `panic = "immediate-abort"` + `-Z build-std`，但引入 nightly 依赖且 panic 丢消息，仅在白名单路线走不通时用。详见 [av-false-positive.md](compatibility/av-false-positive.md)。

## 4. 逆向证据与可重跑脚本

**目标二进制**：WW 2.7.1.0，`G:\SteamLibrary\steamapps\common\ELDEN RING\Game\eldenring.exe`
SHA256 `1A3547101327F65D0C76DA2F9190AC0AA66871EA42BAE2AECC61E11A8B597891`

| 脚本 | 作用 |
| --- | --- |
| [re_chrset.py](../research/re_chrset.py) | ChrSet 虚表导出 |
| [re_chrins_vmt.py](../research/re_chrins_vmt.py) | ChrIns 虚表 / 析构反汇编 |
| [re_disasm.py](../research/re_disasm.py) | 任意 RVA 区间反汇编 |
| [re_allocator.py](../research/re_allocator.py) | `DLAllocator::deallocate` 追踪 |
| [re_func.py](../research/re_func.py) | `.pdata` 函数边界查询 |
| [re_rtti_classes.py](../research/re_rtti_classes.py) | RTTI 类名枚举 |
| [re_scan_*.py](../research/) | 早期扫描脚本（loadstatus / transparency / debugflags） |

运行需要带 `pefile` + `capstone` 的 Python 解释器，直接以 exe 路径为参数执行。

关键 RVA（详见 [engine-chr-lifecycle.md](compatibility/engine-chr-lifecycle.md)）：ChrSet vtable `0x2A41348`；`free_chr_list` `0x495890`；ChrIns vtable `0x2A310C8`；标量删除析构 `0x3E81F0`；allocator holder `0x4846DC0`。

**偏移断言已进测试**：`pinned_chr_lifecycle_offsets_match_disassembly` 把逆向得到的 9 个偏移与绑定字段布局交叉比对，两源独立吻合；改动布局相关代码后此测试必须保持通过。

## 5. 构建 / 测试 / 验证

### 常规构建（stable）

```powershell
cargo test --offline                 # 5 项测试，含布局断言
cargo build --release --locked       # 产物
```

测试清单：

- `pinned_chr_lifecycle_offsets_match_disassembly`
- `pinned_debug_creator_layout_matches_private_field_bridge`
- `supports_only_the_pinned_fsrs_game_versions`
- `accepts_legacy_position_field_names`
- `distributed_config_uses_valid_position_fields`

产物核对（每次构建后应验证，别拿"构建成功"当结论）：

```python
import pefile
pe = pefile.PE("target/x86_64-pc-windows-msvc/release/summon.dll", fast_load=False)
print("VERSIONINFO:", any(e.id == 16 for e in pe.DIRECTORY_ENTRY_RESOURCE.entries))  # True
print("sections:", [s.Name.decode().rstrip("\0") for s in pe.sections])              # 7 节，含 .rsrc
print("debug types:", [d.struct.Type for d in pe.DIRECTORY_ENTRY_DEBUG])             # [2, 12, 13]
```

### 运行时验证（看日志，不用调试器）

`summon.log`（DLL 同目录）每单位三行：

- `summon bound`：`entity_id_outcome`、`chr_event_entity_id`、`id_resolves_to_this_unit`、`set_event_ids`；
- `summon staged for removal`：本次释放的 id 清单；
- `summon removed`：`outcome` + `set_event_ids` / `set_capacity`（计数应回落）。

## 6. 快速上手

```powershell
cd D:\_UGit\_FTProject\FSTool\_Mod\_Project\Summon
cargo test --offline
cargo build --release --locked
# 产物拷到游戏 loader 目录（summon.dll + summon.toml 同目录放置）
```

## 7. 未完成项与待决策（交接重点）

| # | 事项 | 现状 | 谁来做 |
| --- | --- | --- | --- |
| 1 | `summon.toml` 六个 `event_entity_id` 仍为 `0` | 只走 handle，不注册 id | **需人工**在自己事件空间挑空闲 id 填入；被占会报 `conflict`，解析不回会报 `does not resolve back` |
| 2 | 实机验证未做 | 沙箱无法保活 GUI | **需人工**：加载 → 触发 → 确认单位消失后不崩溃、日志计数回落、id 可被事件脚本寻址 |
| 3 | AV 误报实机确认未做 | 仓库侧已改（profile + VERSIONINFO） | **需人工**：新产物实机加载观察；若仍报，走白名单（提交 SHA256，见 av-false-positive.md）或 nightly 方案 |
| 4 | FSTool 主仓 `_Mod/_Project/Summon` 是**半注册 gitlink**（索引里有 mode 160000 条目，但仓库无 `.gitmodules`） | 未动 | 待决策：**A** 补 `.gitmodules` 正式注册为子模块；**B** 从 FSTool 索引移出，当独立克隆 |
| 5 | 日本版 1.17.1 未验证 | README 已注明 | 后续 |

## 8. 已知边界（不要踩）

- `remove_mode = "destroy"`（默认）**强制关闭单位复用** —— 要复用改 `"hide"`；
- `destroy_summon` 只在通过三重验证时执行（entry 自洽）；`entry-mismatch` 是保护机制，不是 bug；
- `group_id_mapping`（`ChrSet + 0x40`）不写入：绑定不含对应结构，若引擎有 per-character 条目，它指向已清空 entry，读回是"无角色"而非悬空指针；
- 子弹及单位产生的其它 field instance 有自己的生命周期，不在角色分配内（本模块不处理）；
- `research/` 脚本是证据链的一部分：改动引擎相关结论前先重跑复核，别只改文档。
