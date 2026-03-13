# Legacy 迁移方案

## 一、目标

将旧文件格式的记忆数据无缝迁移到数据库表，保证：
- 迁移代码独立不耦合主流程（`src/legacy/` 独立目录）
- 事务性：全部成功或全部回滚
- 幂等性：重复执行不会重复写入
- 数据完整性：迁移前后信息不丢失

## 二、目录结构

```
src/legacy/
├── mod.rs          — 模块入口，pub fn migrate_all()
└── migrate.rs      — 迁移逻辑实现
```

`legacy/` 目录将来和 `bindings/` 一起作为 Guardian 特殊目录（豁免扫描）。

## 三、迁移对象

| 旧文件 | 新表 | 解析策略 | 难度 |
|--------|------|----------|------|
| `memory/knowledge.md` | knowledge_store | 整文件→一条记录 | 低 |
| `memory/sessions/history.txt` | history_store | 整文件→一条记录 | 低 |
| `memory/sessions/*.jsonl` | session_blocks | 逐文件逐行JSON解析 | 中 |
| `memory/sessions/current.txt` | action_log | 整体作为LegacyNote插入 | 中 |
| `memory/knowledge/*.md` + `memory/keypoints.md` | knowledge_store | 合并→一条记录（已有逻辑） | 低 |

## 四、触发机制

### 触发点

`Instance::open()` 中，Memory::open() 之后调用：

```rust
legacy::migrate_all(&memory, &memory_dir)?;
```

### 判断是否需要迁移

每种数据独立判断，不用全局标记：

| 数据 | 跳过条件 | 文件路径 |
|------|----------|----------|
| knowledge | DB非空（read_knowledge非空） | `memory/knowledge.md` |
| history | DB非空（read_history非空） | `memory/sessions/history.txt` |
| sessions | DB非空（list_session_blocks_db非空） | `memory/sessions/*.jsonl` |
| current | 无action_log记录（cursor后无数据） | `memory/sessions/current.txt` |

**为什么不用全局标记文件？** 因为不同数据可能处于不同迁移状态（比如knowledge已迁移但sessions还没有）。独立判断更健壮。

## 五、解析策略

### 5.1 knowledge.md（低难度）

```
读取 memory/knowledge.md → trim → 非空则 write_knowledge()
```

**已有逻辑**：instance.rs 中的 knowledge 迁移代码搬到 legacy/ 中。同时处理更老的格式（keypoints.md + knowledge/*.md 合并）。

### 5.2 history.txt（低难度）

```
读取 memory/sessions/history.txt → trim → 非空则 write_history()
```

整文件作为一条记录写入 history_store。

### 5.3 sessions/*.jsonl（中难度）

**文件格式**：
- 文件名 = block_name（时间戳格式，如 `20260313143941.jsonl`）
- 每行一个JSON对象：`{"first_msg":"...","last_msg":"...","summary":"..."}`
- 一个文件可包含多行（多个session block entry）

**解析流程**：
```
遍历 memory/sessions/*.jsonl（按文件名排序）
  对每个文件：
    block_name = 文件名去掉.jsonl后缀
    逐行读取：
      解析JSON → SessionBlockEntry { first_msg, last_msg, summary }
      调用 insert_session_block_entry(block_name, entry)
```

**容错**：
- JSON解析失败的行：log warning，跳过该行，继续处理
- 空文件：跳过
- 文件读取失败：log warning，跳过该文件

### 5.4 current.txt（中难度）

**问题**：current.txt 是非结构化文本，包含行为编号标记、thinking内容、执行结果等混合格式。精确解析每个action block并还原为结构化的 Action + ActionOutput 不现实且不必要。

**策略**：整体作为一条 `insert_done_note` 记录插入 action_log。

```rust
let content = read_to_string("memory/sessions/current.txt")?;
if !content.trim().is_empty() {
    memory.insert_done_note("[Legacy] 以下是迁移前的会话记录：\n\n{content}");
}
```

**为什么不解析？**
1. current.txt 格式是为人类/LLM阅读设计的，不是结构化数据
2. 旧格式中的action block标记（`---------行为编号[xxx]开始---------`）是渲染格式，不是数据格式
3. 迁移后agent会在下一次summary时自然压缩这段legacy内容
4. 精确解析的ROI极低：需要处理各种边界情况（中断的block、嵌套格式等），而收益只是让DB中的action_type更精确

**效果**：agent醒来后看到一条包含旧session内容的note，可以正常summary/distill，记忆链不断裂。

## 六、事务性保证

### 迁移事务

每种数据的迁移在独立的SQLite事务中执行：

```rust
pub fn migrate_all(memory: &Memory, memory_dir: &Path) -> Result<()> {
    let sessions_dir = memory_dir.join("sessions");
    
    // 1. Knowledge（含旧格式合并）
    migrate_knowledge(memory, memory_dir)?;
    
    // 2. History
    migrate_history(memory, &sessions_dir)?;
    
    // 3. Sessions
    migrate_sessions(memory, &sessions_dir)?;
    
    // 4. Current
    migrate_current(memory, &sessions_dir)?;
    
    Ok(())
}
```

每个 `migrate_xxx` 函数内部：
1. 检查跳过条件（DB已有数据）→ 跳过
2. 检查文件是否存在 → 不存在则跳过
3. 读取文件 → 解析 → 写入DB
4. 成功后 rename 文件为 `xxx.migrated`（保留备份，不删除）

### 失败回滚

- 单个数据迁移失败：该数据回滚，其他数据不受影响
- DB写入在事务中，失败自动回滚
- 文件rename在DB写入成功后执行（先DB后文件）
- 如果DB成功但rename失败：下次启动时DB非空→跳过条件命中→不会重复迁移

### 幂等性

- DB非空检查确保不会重复写入
- `.migrated` 后缀确保不会重复读取
- 两层保护：即使 `.migrated` rename 失败，DB非空检查也能防止重复

## 七、数据完整性校验

迁移后的验证（log级别，不阻断启动）：

| 数据 | 校验 |
|------|------|
| knowledge | DB content长度 > 0 |
| history | DB content长度 > 0 |
| sessions | DB block数量 = jsonl文件数量 |
| current | action_log中有legacy note记录 |

校验失败只 log warning，不回滚。因为：
- 部分数据丢失（如空文件）不影响系统运行
- agent可以通过后续交互重建记忆

## 八、代码组织

### src/legacy/mod.rs

```rust
//! Legacy file-to-DB migration.
//!
//! This module handles one-time migration of memory files to database tables.
//! Guardian exemption: this directory contains legacy format parsing code
//! that will be removed once all instances have migrated.

mod migrate;

pub use migrate::migrate_all;
```

### src/legacy/migrate.rs

```rust
//! Migration implementations for each data type.

use crate::persist::memory::Memory;
use anyhow::Result;
use log::{info, warn};
use std::path::Path;

/// Run all legacy migrations. Each migration is independent and idempotent.
pub fn migrate_all(memory: &Memory, memory_dir: &Path) -> Result<()> {
    let sessions_dir = memory_dir.join("sessions");
    
    migrate_knowledge(memory, memory_dir)?;
    migrate_history(memory, &sessions_dir)?;
    migrate_sessions(memory, &sessions_dir)?;
    migrate_current(memory, &sessions_dir)?;
    
    Ok(())
}

fn migrate_knowledge(memory: &Memory, memory_dir: &Path) -> Result<()> {
    if !memory.read_knowledge().is_empty() {
        return Ok(()); // Already migrated
    }
    
    let knowledge_file = memory_dir.join("knowledge.md");
    if knowledge_file.exists() {
        let content = std::fs::read_to_string(&knowledge_file)?;
        if !content.trim().is_empty() {
            memory.write_knowledge(&content)?;
            rename_migrated(&knowledge_file);
            info!("[LEGACY] Migrated knowledge.md → DB ({} bytes)", content.len());
        }
        return Ok(());
    }
    
    // Older format: keypoints.md + knowledge/*.md
    let keypoints_path = memory_dir.join("keypoints.md");
    if keypoints_path.exists() {
        let merged = merge_old_knowledge(memory_dir)?;
        if !merged.is_empty() {
            memory.write_knowledge(&merged)?;
            rename_migrated(&keypoints_path);
            info!("[LEGACY] Migrated keypoints.md+knowledge/ → DB ({} bytes)", merged.len());
        }
    }
    
    Ok(())
}

fn migrate_history(memory: &Memory, sessions_dir: &Path) -> Result<()> {
    if !memory.read_history().is_empty() {
        return Ok(());
    }
    
    let history_file = sessions_dir.join("history.txt");
    if !history_file.exists() {
        return Ok(());
    }
    
    let content = std::fs::read_to_string(&history_file)?;
    if !content.trim().is_empty() {
        memory.write_history(&content)?;
        rename_migrated(&history_file);
        info!("[LEGACY] Migrated history.txt → DB ({} bytes)", content.len());
    }
    
    Ok(())
}

fn migrate_sessions(memory: &Memory, sessions_dir: &Path) -> Result<()> {
    if !memory.list_session_blocks_db()?.is_empty() {
        return Ok(());
    }
    
    let mut jsonl_files: Vec<_> = std::fs::read_dir(sessions_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
        .collect();
    
    if jsonl_files.is_empty() {
        return Ok(());
    }
    
    // Sort by filename (timestamp order)
    jsonl_files.sort_by_key(|e| e.file_name());
    
    let mut total_entries = 0;
    for entry in &jsonl_files {
        let path = entry.path();
        let block_name = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!("[LEGACY] Failed to read {}: {}", path.display(), e);
                continue;
            }
        };
        
        for (line_num, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            
            match serde_json::from_str::<SessionBlockEntry>(line) {
                Ok(entry) => {
                    memory.insert_session_block_entry(&block_name, &entry)?;
                    total_entries += 1;
                }
                Err(e) => {
                    warn!("[LEGACY] Failed to parse {}:{}: {}", 
                          path.display(), line_num + 1, e);
                }
            }
        }
        
        rename_migrated(&path);
    }
    
    info!("[LEGACY] Migrated {} session entries from {} files → DB", 
          total_entries, jsonl_files.len());
    
    Ok(())
}

fn migrate_current(memory: &Memory, sessions_dir: &Path) -> Result<()> {
    // Skip if action_log already has records
    if !memory.render_current_from_db().is_empty() {
        return Ok(());
    }
    
    let current_file = sessions_dir.join("current.txt");
    if !current_file.exists() {
        return Ok(());
    }
    
    let content = std::fs::read_to_string(&current_file)?;
    if !content.trim().is_empty() {
        let note = format!("[Legacy] 迁移前的会话记录：\n\n{}", content);
        memory.insert_done_note(&note)?;
        rename_migrated(&current_file);
        info!("[LEGACY] Migrated current.txt → DB ({} bytes)", content.len());
    }
    
    Ok(())
}

/// Rename file to .migrated (backup, not delete)
fn rename_migrated(path: &Path) {
    let migrated = path.with_extension(
        format!("{}.migrated", 
                path.extension().unwrap_or_default().to_str().unwrap_or(""))
    );
    if let Err(e) = std::fs::rename(path, &migrated) {
        warn!("[LEGACY] Failed to rename {} → {}: {}", 
              path.display(), migrated.display(), e);
    }
}

fn merge_old_knowledge(memory_dir: &Path) -> Result<String> {
    // ... (搬自instance.rs的migrate_knowledge逻辑)
}
```

## 九、Instance::open 改造

### 改造前（instance.rs）

knowledge迁移逻辑散布在 `open()` 和 `open_with_chat()` 中（代码重复）。

### 改造后

```rust
// Instance::open()
let memory = Memory::open(&memory_dir, &id).context("Failed to open memory")?;

// One-time legacy migration (all data types)
legacy::migrate_all(&memory, &memory_dir)?;
```

**变化**：
1. 删除 instance.rs 中的 knowledge 迁移代码（两处重复）
2. 删除 `migrate_knowledge()` 私有方法
3. 统一调用 `legacy::migrate_all()`
4. open 和 open_with_chat 都只需一行调用

## 十、Guardian 豁免策略

`src/legacy/` 目录的豁免理由：

1. **临时性**：迁移代码在所有实例完成迁移后将被删除
2. **隔离性**：不影响主流程，只在启动时触发一次
3. **旧格式解析**：必然包含对旧文件格式的字符串操作，这些不是"隐式契约"而是"显式的格式转换"

豁免范围：
- `src/legacy/*.rs` — 所有文件豁免 Guardian 扫描
- 与 `src/bindings/` 同等地位

## 十一、.last_rolled 处理

`.last_rolled` 是 history rolling 的幂等标记文件，目前仍在使用（memory.rs 中的 get/set/clear_last_rolled）。

**不迁移**：`.last_rolled` 是运行时幂等标记，不是持久化数据。保留文件形式。

但如果将来要彻底消灭文件依赖，可以加一个 `rolling_state` 表。这不在本次迁移范围内。

## 十二、迁移后清理（后续任务）

迁移代码上线并确认所有实例完成迁移后：

1. 删除 `src/legacy/` 目录
2. 删除 memory.rs 中 sessions_dir 相关代码（如果 .last_rolled 也迁移到DB的话）
3. 更新 core/mod.rs、engine/mod.rs 中的旧格式注释
4. 删除 instance.rs 中的 `KNOWLEDGE_FILE` 常量和 knowledge_dir 创建逻辑

