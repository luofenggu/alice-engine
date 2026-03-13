# 记忆机制数据库化 — 表结构设计

## 一、DB位置决策

新表放在现有 per-instance chat.db 中（和 messages 表同库）。

理由：
- 减少连接管理复杂度（Memory struct 只需一个 db connection）
- action_log 需要关联 messages 表的 timestamp，同库更自然
- instance_id 列保留（查询方便 + 未来扩展），当前实际单值

## 二、表结构

### 1. action_log（替代 current.txt）

```sql
CREATE TABLE IF NOT EXISTS action_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id     TEXT    NOT NULL,
    action_id       TEXT    NOT NULL,   -- YYYYMMDDHHmmss_6hex
    action_type     TEXT    NOT NULL,   -- idle/read_msg/send_msg/thinking/script/write_file/replace_in_file/summary/set_profile/create_instance/distill
    action_data     TEXT    NOT NULL,   -- serde_json::to_string(&Action)
    result_text     TEXT,               -- 执行结果（stdout、骨架、消息内容等）
    status          TEXT    NOT NULL DEFAULT 'executing',  -- executing/done/distilled
    msg_id_first    TEXT,               -- 本action关联的最早消息timestamp
    msg_id_last     TEXT,               -- 本action关联的最晚消息timestamp
    created_at      TEXT    NOT NULL    -- ISO 8601
);

CREATE INDEX IF NOT EXISTS idx_action_log_instance ON action_log(instance_id);
CREATE INDEX IF NOT EXISTS idx_action_log_action_id ON action_log(action_id);
CREATE INDEX IF NOT EXISTS idx_action_log_type ON action_log(instance_id, action_type);
```

**字段说明：**

| 字段 | 类型 | 说明 |
|------|------|------|
| id | INTEGER PK | 自增主键，cursor 引用此值 |
| instance_id | TEXT | 实例ID（per-instance DB 下实际单值） |
| action_id | TEXT | `YYYYMMDDHHmmss_6hex`，distill 的 target 就是这个值 |
| action_type | TEXT | Action enum 变体名（snake_case），独立列支持 SQL 条件查询 |
| action_data | TEXT | Action enum 的 JSON 序列化（完整字段） |
| result_text | TEXT | 执行结果。distill 时直接 UPDATE 此列为提炼总结 |
| status | TEXT | executing → done（正常）/ distilled（被提炼） |
| msg_id_first | TEXT | read_msg: 本次读到的最早消息 timestamp；send_msg: 发送的消息 timestamp |
| msg_id_last | TEXT | read_msg: 本次读到的最晚消息 timestamp；send_msg: 同 msg_id_first |
| created_at | TEXT | ISO 8601 创建时间 |

**msg_id 类型选择 TEXT 而非 INTEGER：**
- 现有 SessionBlockEntry.first_msg/last_msg 是 String（timestamp 格式）
- send_msg 返回 timestamp 字符串，read_msg 的消息标识也是 timestamp
- session 块展示用 timestamp
- 存 messages.id(i64) 反而需要额外查询映射

**status 简化：** 设计文档提到 5 种状态（executing/done/interrupted/rejected/distilled），但 interrupted 和 rejected 的行不会有 done_text，只是 result_text 包含错误/中断信息。简化为 3 种：
- `executing` — INSERT 时的初始状态
- `done` — 执行完成（含正常、中断、拒绝等，区别在 result_text 内容）
- `distilled` — 被提炼过

### 2. memory_cursor（替代"清空 current"的概念）

```sql
CREATE TABLE IF NOT EXISTS memory_cursor (
    instance_id     TEXT PRIMARY KEY,
    current_cursor  INTEGER NOT NULL DEFAULT 0,  -- action_log.id，渲染起始位置
    updated_at      TEXT NOT NULL
);
```

**cursor 语义：** `SELECT * FROM action_log WHERE id >= current_cursor` = 当前 session 的 action 列表。summary 时更新 cursor 到 `MAX(id) + 1`，等效于"清空 current"。

### 3. knowledge_store（替代 knowledge.md TextFile）

```sql
CREATE TABLE IF NOT EXISTS knowledge_store (
    instance_id  TEXT PRIMARY KEY,
    content      TEXT NOT NULL DEFAULT '',
    updated_at   TEXT NOT NULL
);
```

### 4. history_store（替代 history.txt TextFile）

```sql
CREATE TABLE IF NOT EXISTS history_store (
    instance_id  TEXT PRIMARY KEY,
    content      TEXT NOT NULL DEFAULT '',
    updated_at   TEXT NOT NULL
);
```

### 5. session_blocks（替代 sessions/*.jsonl）

```sql
CREATE TABLE IF NOT EXISTS session_blocks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id  TEXT    NOT NULL,
    block_name   TEXT    NOT NULL,   -- 时间戳格式，排序用
    first_msg    TEXT    NOT NULL,   -- 最早消息 timestamp
    last_msg     TEXT    NOT NULL,   -- 最晚消息 timestamp
    summary      TEXT    NOT NULL,   -- 小结内容
    created_at   TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_session_blocks_instance ON session_blocks(instance_id);
```

## 三、Diesel Schema（table! 宏）

```rust
// bindings/db.rs 新增

diesel::table! {
    action_log (id) {
        id -> BigInt,
        instance_id -> Text,
        action_id -> Text,
        action_type -> Text,
        action_data -> Text,
        result_text -> Nullable<Text>,
        status -> Text,
        msg_id_first -> Nullable<Text>,
        msg_id_last -> Nullable<Text>,
        created_at -> Text,
    }
}

diesel::table! {
    memory_cursor (instance_id) {
        instance_id -> Text,
        current_cursor -> BigInt,
        updated_at -> Text,
    }
}

diesel::table! {
    knowledge_store (instance_id) {
        instance_id -> Text,
        content -> Text,
        updated_at -> Text,
    }
}

diesel::table! {
    history_store (instance_id) {
        instance_id -> Text,
        content -> Text,
        updated_at -> Text,
    }
}

diesel::table! {
    session_blocks (id) {
        id -> BigInt,
        instance_id -> Text,
        block_name -> Text,
        first_msg -> Text,
        last_msg -> Text,
        summary -> Text,
        created_at -> Text,
    }
}
```

## 四、Model Structs

```rust
// --- action_log ---

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = action_log)]
pub struct ActionLogRow {
    pub id: i64,
    pub instance_id: String,
    pub action_id: String,
    pub action_type: String,
    pub action_data: String,
    pub result_text: Option<String>,
    pub status: String,
    pub msg_id_first: Option<String>,
    pub msg_id_last: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = action_log)]
pub struct NewActionLog<'a> {
    pub instance_id: &'a str,
    pub action_id: &'a str,
    pub action_type: &'a str,
    pub action_data: &'a str,
    pub result_text: Option<&'a str>,
    pub status: &'a str,
    pub msg_id_first: Option<&'a str>,
    pub msg_id_last: Option<&'a str>,
    pub created_at: &'a str,
}

// --- memory_cursor ---

#[derive(Debug, Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = memory_cursor)]
pub struct MemoryCursorRow {
    pub instance_id: String,
    pub current_cursor: i64,
    pub updated_at: String,
}

// --- knowledge_store ---

#[derive(Debug, Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = knowledge_store)]
pub struct KnowledgeRow {
    pub instance_id: String,
    pub content: String,
    pub updated_at: String,
}

// --- history_store ---

#[derive(Debug, Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = history_store)]
pub struct HistoryRow {
    pub instance_id: String,
    pub content: String,
    pub updated_at: String,
}

// --- session_blocks ---

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = session_blocks)]
pub struct SessionBlockRow {
    pub id: i64,
    pub instance_id: String,
    pub block_name: String,
    pub first_msg: String,
    pub last_msg: String,
    pub summary: String,
    pub created_at: String,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = session_blocks)]
pub struct NewSessionBlock<'a> {
    pub instance_id: &'a str,
    pub block_name: &'a str,
    pub first_msg: &'a str,
    pub last_msg: &'a str,
    pub summary: &'a str,
    pub created_at: &'a str,
}
```

## 五、Action Enum 改造

```rust
// inference/mod.rs

#[derive(Debug, Clone, FromMarkdown, Serialize, Deserialize)]
pub enum Action {
    // ... 现有 11 个变体不变
}
```

新增 `Serialize, Deserialize` derive。`action_type` 列值通过独立函数提取：

```rust
impl Action {
    pub fn type_name(&self) -> &'static str {
        match self {
            Action::Idle { .. } => "idle",
            Action::ReadMsg => "read_msg",
            Action::SendMsg { .. } => "send_msg",
            Action::Thinking { .. } => "thinking",
            Action::Script { .. } => "script",
            Action::WriteFile { .. } => "write_file",
            Action::ReplaceInFile { .. } => "replace_in_file",
            Action::Summary { .. } => "summary",
            Action::SetProfile { .. } => "set_profile",
            Action::CreateInstance { .. } => "create_instance",
            Action::Distill { .. } => "distill",
        }
    }
}
```

## 六、关键流程变化

### A. Write-Ahead Doing → INSERT/UPDATE

```
现在：
  append_current(action_block_doing(id, doing_text))  -- 写文件
  执行 action
  append_current(action_block_done(id, done_text))     -- 追加文件

目标：
  INSERT action_log (..., status='executing')          -- 写DB
  执行 action
  UPDATE action_log SET result_text=..., status='done'  -- 更新DB
```

### B. current 渲染 → SELECT + 格式化

```
现在：
  current_content = memory.current.read()   // 读文件

目标：
  rows = SELECT * FROM action_log WHERE instance_id=? AND id >= cursor ORDER BY id
  current_content = rows.map(render_action_log_entry).join("\n")
```

**渲染函数设计（关键讨论点）：**

设计文档说"Action enum 的 ToMarkdown 渲染 current"，但需要区分两种 ToMarkdown：
- **schema ToMarkdown**（FromMarkdown derive 宏配套）：输出规范格式，给 LLM 解析用
- **current 显示格式**：`build_doing_description` 那种人类可读描述

**我的建议：** 不给 Action derive ToMarkdown（避免和 schema 格式冲突），而是保留 `build_doing_description` 的功能，重构为渲染函数：

```rust
fn render_action_log_entry(row: &ActionLogRow) -> String {
    let action: Action = serde_json::from_str(&row.action_data).unwrap();
    let mut result = String::new();
    
    // 行为编号标记
    result.push_str(&format!("---------行为编号[{}]开始---------\n", row.action_id));
    
    match row.status.as_str() {
        "distilled" => {
            // 提炼过的：只显示提炼总结
            result.push_str(&format!("[已提炼] {}\n", row.result_text.as_deref().unwrap_or("")));
        }
        _ => {
            // doing 部分：从 Action 生成描述
            result.push_str(&build_doing_description(&action));
            result.push('\n');
            
            // result 部分
            if row.status == "executing" {
                result.push_str("---action executing, result pending---\n");
            } else if let Some(ref text) = row.result_text {
                if !text.is_empty() {
                    result.push_str(text);
                    if !text.ends_with('\n') {
                        result.push('\n');
                    }
                }
            }
        }
    }
    
    result.push_str(&format!("---------行为编号[{}]结束---------\n", row.action_id));
    result
}
```

这样 **current 渲染格式和现在完全一致**，LLM 不会感知到底层从文件变成了 DB。

**但设计文档的意图可能是**：彻底改变 current 格式，不再用行为编号标记，而是用 Action 的 ToMarkdown 渲染成更结构化的格式。这点需要进化之王确认。

### C. summary → SQL 查询

```
现在：
  extract_msg_ids(current_text)  // 文本搜索 [MSG:xxx]

目标：
  SELECT MIN(msg_id_first), MAX(msg_id_last)
  FROM action_log
  WHERE instance_id = ? AND id >= cursor
    AND action_type IN ('read_msg', 'send_msg')
    AND msg_id_first IS NOT NULL
```

**extract_msg_ids 彻底消亡** ✅

### D. distill → UPDATE

```
现在：
  replace_action_block(action_id, summary)  // 文本搜索替换

目标：
  UPDATE action_log
  SET result_text = ?, status = 'distilled'
  WHERE action_id = ? AND instance_id = ?
```

### E. knowledge/history → 表读写

```
SELECT content FROM knowledge_store WHERE instance_id = ?
INSERT OR REPLACE INTO knowledge_store VALUES (?, ?, ?)
```

## 七、待确认问题

### Q1: current 渲染格式保持不变 or 改版？

方案 A（保守）：渲染格式和现在 current.txt 完全一致（行为编号标记、doing/done 分段）。LLM 无感知。
方案 B（激进）：利用 Action ToMarkdown 生成新格式，彻底去掉行为编号标记。需要同时改 Distill 的 target 引用方式。

我建议方案 A 先落地，格式改版作为后续任务。

### Q2: WriteFile 的 action_data 存完整 content 吗？

WriteFile 的 content 可能很大（几KB到几十KB）。
- 方案 A：action_data 存完整 Action（含大 content）。数据完整但 DB 膨胀。
- 方案 B：入库前裁剪 WriteFile.content 为空或摘要。省空间但丢数据。
- 方案 C：action_data 存完整，但渲染时只用 path 不用 content。现在 build_doing_description 对 WriteFile 就只用 path。

我建议方案 C：存完整但渲染不用。数据完整性优先，DB 膨胀可接受（SQLite 处理大 TEXT 没问题）。

### Q3: skill TextFile 是否一并迁移？

Instance.skill 是 TextFile，设计文档提到需要迁移。是否加一个 skill_store 表？还是留到后续任务？

### Q4: action_output.rs 实际消亡范围

真正消亡的是文件格式相关函数（~200 行）：
- `action_block_start/end/full/doing/done` — 行为编号标记格式
- `distilled_block` — 提炼格式
- `build_doing_text/build_done_text` — Write-Ahead 格式
- `inference_interrupted/hallucination_defense_interrupted` — 中断标记

保留的是结果生成函数（~250 行）：
- `build_doing_description` — action 描述（→ 入 current 渲染）
- `script_result` — 脚本结果格式化（→ 入 result_text）
- `write_success_*` — 写入结果（→ 入 result_text）
- `read_msg_entry` — 消息格式化（→ 入 result_text）
- `send_success/send_failed_*` — 发送结果（→ 入 result_text）
- `replace_success/replace_match_error` — 替换结果
- `truncate_result/format_preview` — 截断工具
- `generate_action_id` — ID 生成

所以 action_output.rs 不会完全消亡，而是从 ~450 行瘦身到 ~250 行。