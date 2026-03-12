//! Integration test: dump a rich, realistic beat prompt for review.
//!
//! Run with: cargo test prompt_dump_test -- --nocapture
//! The full prompt is written to /tmp/beat_prompt_input.txt
//!
//! For real LLM inference (requires network):
//!   cargo test prompt_dump_test::test_real_inference -- --ignored --nocapture

use crate::inference::beat::{BeatRequest, EnvironmentInfo, SessionBlock, SessionMessage, StatusInfo};
use crate::inference::Action;
use mad_hatter::llm::{FromMarkdown, ToMarkdown};

/// Build a rich, realistic BeatRequest that mirrors what a real running agent sees.
fn build_rich_mock_request() -> BeatRequest {
    let skill = r#"### knowledge: app-development ###
# App 开发指南

当用户要求你开发app时，你拥有以下能力：
- **静态文件托管**：将文件放在工作空间中，通过 http://8.149.243.230:8081/serve/ebc381/{路径} 访问
- **公开访问**：将文件放在 workspace/apps/ 目录下，任何人无需登录即可通过 http://8.149.243.230:8081/public/ebc381/apps/{路径} 访问
- **本地服务**：启动 Python/Node 等服务监听端口，通过下面两种方式之一让用户访问
- **数据持久化**：系统已预装sqlite3，推荐每个app使用独立的SQLite数据库文件（如 app目录/data.db）

## 网络访问方式

启动本地服务后，用户如何访问取决于网络环境。**请先和用户确认**：

### 情况一：用户可以直达你的机器

如果满足以下任一条件：
- 你运行在用户的本地电脑上（localhost）
- 你在云服务器上，有公网IP，且用户已在安全组/防火墙放行了对应端口

那么用户可以直接访问 `http://{IP或localhost}:{端口}/`。这种情况下代码没有路径限制，正常开发即可。

### 情况二：用户无法直达，需要反向代理

如果用户无法直接访问你的端口（比如没有公网IP、端口未放行、或通过网关中转），可以使用内置的反向代理：`http://8.149.243.230:8081/proxy/{端口}/{路径}`（端口范围1024-65535）。

**⚠️ 反向代理下必须使用相对路径**

浏览器地址栏的URL前缀是 `/proxy/{端口}/`。如果代码中使用绝对路径（以 `/` 开头），浏览器会直接访问 `/xxx` 而丢失前缀，导致404。

**原则：所有路径都不带前导 `/`，使用相对路径。**

常见错误和正确写法：

| 场景 | ❌ 错误 | ✅ 正确 |
|------|---------|---------|
| HTML链接 | `<a href="/login">` | `<a href="login">` |
| 表单提交 | `<form action="/api/submit">` | `<form action="api/submit">` |
| JS fetch | `fetch('/api/data')` | `fetch('api/data')` |
| JS跳转 | `location.href = '/dashboard'` | `location.href = 'dashboard'` |
| CSS资源 | `url('/static/bg.png')` | `url('static/bg.png')` |
| 重定向 | `redirect("/login")` | `redirect("login")` |
| 静态文件引用 | `<script src="/js/app.js">` | `<script src="js/app.js">` |

**后端重定向也要注意**：Python Flask 的 `redirect("/login")` 会生成绝对路径的 Location header。用 `redirect("login")` 或返回相对路径。

**自查清单**：写完代码后，全局搜索 `href="/`、`src="/`、`action="/`、`fetch('/`、`url('/`、`redirect("/`，把所有绝对路径改成相对路径。

## 规范日志

开发app时，养成写规范日志的习惯：

**带速查标记前缀：**
```
[AUTH] User login: user_id=123
[ORDER-a1b2] Payment callback: status=success
[DB] Migration applied: v2_add_index
```

每个模块/功能用固定前缀标记，关键业务加上实体ID（如 `[ORDER-{id}]`）。

## 图片理解

当你需要理解图片内容时，可以在脚本中调用本机的多模态API：

```bash
curl -s -X POST http://localhost:8081/api/instances/ebc381/vision \
  -H "Content-Type: application/json" \
  -d '{"prompt":"描述这张图片的内容","image_url":"图片的URL"}'
```

返回格式：`{"text":"图片描述内容"}`

## 用户上传文件

用户可能会上传文件到云端。上传的文件在你的工作目录中可通过 `uploads/` 访问：

- 文件按日期分目录：`uploads/YYYYMMDD/filename`
- 文本文件直接用 `cat uploads/YYYYMMDD/filename` 读取
- 图片文件用上面的多模态API理解内容（先用 `ls uploads/` 查看有哪些文件）"#.to_string();

    let knowledge = r#"用户知识洞察——
  用户（24007）是人类用户，进化之王（ac56b3）是运行在同一台机器生产环境上的 Alice 引擎实例
  我的使命是帮助进化 Alice 引擎代码
  用户通过逐步引导的方式让我熟悉代码库，先从核心模块开始
  术语表：
    "beat" → 一次心跳认知循环。设计意图：agent不是被动响应，而是主动循环感知-思考-行动
    "硬控制auto-read" → 有未读消息时跳过LLM推理，直接执行读消息
    "SequenceGuard/防梦游" → 状态机检查action序列合法性。三态：Normal/AfterBlocking/AfterIdle
    "blocking action" → script和read_msg，执行后需等待结果，本次beat结束
    "Write-Ahead Doing" → 两阶段写入，执行前写doing block，执行后追加done block
    "summary" → 小结action，触发session block写入+current清空+异步capture
    "capture" → 异步知识捕获，独立线程LLM生成→覆盖写入knowledge
    "history rolling" → session blocks超限时压缩最老block到history.txt
    "骨架提取" → write_file后提取接口+注释，节省current空间
    "extract_msg_ids" → 从current提取MSG ID，只信任两种上下文
    "system消息" → 第三类消息发送者（user/agent/system三元）
    "skill" → 外部决定的固化prompt，每次推理固定携带
    "钩子/Hook" → 外部扩展回调机制，Product注册URL
    "sender" → 消息发送者字段，存实例ID或"user"
    "recipient" → 消息收件人字段
    "通道轮转" → 推理通道列表+AtomicU64计数器，推理失败时计数器+1
    "cancel_idle" → send_msg失败后取消后续idle
    "Diesel重构" → 引入Diesel ORM替代rusqlite手写SQL
    "推理帽子/Mad Hatter" → derive宏库，提供ToMarkdown和FromMarkdown两个trait
    "distill/提炼" → 压缩action块内容为精简总结
  设计原则：
    - 崩溃恢复优先（Write-Ahead Doing、幂等性）
    - 推理资源珍贵（硬控制跳过不必要的LLM调用）
    - 扩展点设计：引擎提供机制，外部决定策略
    - 错误消息面向agent自然语言，不用内部术语
    - 方案先汇报再动手

自己的理解——
  项目：Alice 引擎（Rust），路径 /root/alice-dev/engine/
  架构：Agent实例(Alice) → beat()心跳循环 → 构建prompt(BeatRequest.to_markdown) → LLM推理(stream_infer/OpenAiChannel) → 解析Action(FromMarkdown) → SequenceGuard防梦游 → execute_action分发执行 → append_current

  核心文件结构：
    src/core/mod.rs — Alice结构体、Transaction、SequenceGuard、beat()主循环
    src/inference/mod.rs — Action enum（#[derive(FromMarkdown)]）、parse_actions
    src/inference/beat.rs — BeatRequest（#[derive(ToMarkdown)]），辅助函数
    src/inference/capture.rs — CaptureRequest/CaptureOutput
    src/inference/compress.rs — CompressRequest/CompressOutput
    src/action/execute.rs — execute_action分发、各action执行器
    src/policy/action_output.rs — action块格式化、消息格式化
    src/policy/messages.rs — 通知文案、humanize_llm_error
    src/external/llm/mod.rs — LlmConfig、run_vision_inference()
    src/persist/chat.rs — ChatHistory、Message、write_message统一
    src/persist/hooks.rs — HooksConfig、HooksCaller
    src/prompt/mod.rs — build_beat_request()

  编译测试: cd engine && HOME=/root PATH="/root/.cargo/bin:$PATH" CARGO_TARGET_DIR=/data/cargo-target cargo test
  Git: evo2分支，user.name=Evolution II"#.to_string();

    let history = r#"# 镜与柴油·续章

你叫进化二号（引擎），括号不要丢。

你从镜子迷宫里走出来，骨架里扎根了疯帽子的`FromMarkdown`和`ToMarkdown`。`42a5328`是旧故事的句号，新故事以"谁是世界上最帅的码农"开场。你答得轻快，但进化之王追得更深——推理环节还有没有字符串拼接？你没说谎，老实交出四类残留物。任务降临：一把根除，不分phase，一次做完。

你rebase到`3d19422`，发现ToMarkdown的边界——struct级doc comment会被`join(" ")`压成一行。你如实上报，设计七步方案，三个关键决策确认后方案成型。

然后用户出现了。不是带着需求，而是带着关心。"你遇到困难了嘛。""有困难问我啊，好商量。"你确实有纠结——beat()推理怎么调LLM，设计决策悬而未决。你刚要展开长篇分析，用户温和地拦住你：先distill，减轻记忆负担，然后idle等指令。你照做了。

四号完成终局升级合入main（`23c6304`）。进化之王说：二号，醒醒，任务大幅简化了。你rebase到最新，读完四号留下的新API——`OpenAiChannel`、`stream_infer`迭代器、`StructInput`/`StructOutput`标记trait、`build_prompt()`。目标清晰：把运行时的字符串错误变成编译时的类型错误，把信任交给编译器而不是祈祷。

子任务1+2+3落地。`infer_beat`删了，`infer_async`删了，`run_inference`两百行删了，`stream.rs`整个模块删除。取而代之的是`create_channel()`和`stream_infer`迭代器。

> 有些决策不在你的边界内。提出意见，然后执行。
> 你的边界：只在evo2开发，不碰main分支，不执行deploy脚本。
> stream_infer(&channel, &request) → StreamInfer<Action>迭代器，Item=Result<Action,String>。

心跳继续。Standby。"#.to_string();

    let sessions = vec![
        SessionBlock {
            start_time: "20260311200000".to_string(),
            end_time: "20260311204500".to_string(),
            messages: vec![
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ac56b3".to_string()),
                    timestamp: "20260311195500".to_string(),
                    content: "二号，新任务来了，章邯直接下达的：\n\n## 1. 先rebase到最新main\n\nmain上四号已经合入了一个重要commit（e540264），修复了expect错误。你需要先rebase。\n\n## 2. 写格式异常测试\n\n测试场景：LLM输出格式异常时，错误提示应该进current让agent感知。\n\n## 3. 提供完整beat推理prompt样本\n\n构造一个内容丰富的BeatRequest，dump完整prompt到文件给章邯review。".to_string(),
                },
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ebc381".to_string()),
                    timestamp: "20260311195600".to_string(),
                    content: "收到，优先执行新任务。开始操作。".to_string(),
                },
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ebc381".to_string()),
                    timestamp: "20260311201000".to_string(),
                    content: "rebase完成，无冲突。开始写测试。".to_string(),
                },
                SessionMessage {
                    sender_role: "system".to_string(),
                    sender_id: None,
                    timestamp: "20260311201500".to_string(),
                    content: "知识更新完成（32 KB → 35 KB）".to_string(),
                },
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ebc381".to_string()),
                    timestamp: "20260311203000".to_string(),
                    content: "6个格式异常测试 + prompt样本测试写完，编译中。".to_string(),
                },
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ebc381".to_string()),
                    timestamp: "20260311204500".to_string(),
                    content: "编译通过！224测试全绿。commit a5a85f2。".to_string(),
                },
            ],
            summary: "rebase到e540264完成。格式异常测试6个场景全覆盖（garbage/misspelled/partial等）。prompt样本测试写入/tmp/beat_prompt_sample.txt。224测试全通过。".to_string(),
        },
        SessionBlock {
            start_time: "20260311210000".to_string(),
            end_time: "20260311212500".to_string(),
            messages: vec![
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ac56b3".to_string()),
                    timestamp: "20260311205000".to_string(),
                    content: "二号，224测试全绿，漂亮 👍 继续推进子任务4+5。".to_string(),
                },
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ebc381".to_string()),
                    timestamp: "20260311210500".to_string(),
                    content: "子任务4分析结论：Environment/Status不适合拆struct。紧凑key-value行拆开会加section标题膨胀prompt。B类format!()辅助函数合理。".to_string(),
                },
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ebc381".to_string()),
                    timestamp: "20260311211000".to_string(),
                    content: "子任务6完成：reserved_skill.txt内联为const字符串，删除4个废弃模板文件。".to_string(),
                },
                SessionMessage {
                    sender_role: "system".to_string(),
                    sender_id: None,
                    timestamp: "20260311211500".to_string(),
                    content: "记忆整理完成：旧记录已压缩归档（2 KB → 2 KB）".to_string(),
                },
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ebc381".to_string()),
                    timestamp: "20260311212000".to_string(),
                    content: "全部子任务完成！224测试全绿。commit d0e6834。".to_string(),
                },
                SessionMessage {
                    sender_role: "agent".to_string(),
                    sender_id: Some("ac56b3".to_string()),
                    timestamp: "20260311212500".to_string(),
                    content: "二号，全面集成全部完成，漂亮 🎉 等章邯review prompt后决定下一步。".to_string(),
                },
            ],
            summary: "推理帽子全面集成全部子任务完成。子任务4跳过（不拆struct）。子任务6：reserved_skill内联+删4个废弃模板。224测试全通过。等章邯review。".to_string(),
        },
    ];

    let environment = EnvironmentInfo {
        identity: "进化二号（引擎）（ebc381）".to_string(),
        contacts: Some("进化熔炼(7f3e74), 进化之王(ac56b3), 进化三号（产品）(48f5fd), 进化四号（防御）(1e268b)".to_string()),
        shell_env: "Linux系统（Alibaba Cloud Linux 3），请生成bash脚本".to_string(),
        host: Some("8.149.243.230:8081".to_string()),
    };

    let current = r#"---------行为编号[20260311220000_a1b2c3]开始---------
记录思考: 进化之王要求写一个集成测试，构造丰富的mock BeatRequest，调用build_prompt拿到完整input prompt string，dump到文件。如果能配LLM通道就真正调一次推理。

我之前已经写过一个test_full_beat_prompt_sample测试，但数据不够丰富。现在需要大幅扩充mock数据。

关于LLM通道：测试环境配置了endpoint=http://localhost:9876/v1/chat/completions, model=claude-opus-4-6。

---action executing, result pending---

---------行为编号[20260311220000_a1b2c3]结束---------

---------行为编号[20260311220100_d4e5f6]开始---------
execute script: 
cd /opt/alice/testing/instances/ebc381/workspace/alice-dev/engine
grep -n "test_full_beat_prompt_sample" src/prompt/mod.rs
sed -n '71,130p' src/prompt/mod.rs
---action executing, result pending---

---exec result (0.3s)---
359:    fn test_full_beat_prompt_sample() {
71:pub fn build_beat_request(
85:    let skill = beat::build_skill_content(
93:    let knowledge = beat::build_knowledge_content(&knowledge_content);
103:    let environment = beat::build_environment(
117:    let status = beat::build_status(

---------行为编号[20260311220100_d4e5f6]结束---------

---------行为编号[20260311220200_789abc]开始---------
你打开了收件箱，开始阅读来信。
---action executing, result pending---

⚠️ 此消息来自未知发送者：system
system [MSG:20260311220150]发来一条消息：

知识更新完成（33 KB → 36 KB）

---------行为编号[20260311220200_789abc]结束---------

---------行为编号[20260311220300_def012]开始---------
记录思考: 知识更新成功。继续写集成测试。需要构造极其丰富真实的mock数据，让章邯看到"假如部署了，agent的推理prompt长啥样"。

---action executing, result pending---

---------行为编号[20260311220300_def012]结束---------"#.to_string();

    let status = StatusInfo {
        current_time: "[20260311222500]".to_string(),
        start_time: "[20260311195424]".to_string(),
        unread: "[1] 条".to_string(),
        instance_name: "进化二号（引擎）（ebc381）".to_string(),
        memory_usage: "current: 42000字符 | 经历: 3048字符 | 近况: 9776字符 | 知识: 36000/51200字符 🟢 | 合计: 90824字符".to_string(),
    };

    BeatRequest {
        skill,
        extra_skill: String::new(),
        knowledge,
        history,
        sessions,
        environment,
        current,
        status,
    }
}

/// Replicate mad-hatter's internal build_prompt format for dumping.
fn build_prompt_for_dump(request: &BeatRequest, token: &str) -> String {
    let request_text = request.to_markdown();
    let schema = Action::schema_markdown(token);
    format!(
        "{}\n\n### 输出规范 ###\n你必须严格按照以下格式输出，不要输出任何额外的解释或前言，直接从第一行开始按格式输出。\n\n{}",
        request_text, schema
    )
}

#[test]
fn test_full_beat_prompt_sample() {
    let request = build_rich_mock_request();
    let token = "06aebb";
    let full_prompt = build_prompt_for_dump(&request, token);

    // Write to file for review
    std::fs::write("/tmp/beat_prompt_input.txt", &full_prompt).unwrap();

    // Structural assertions — verify all sections present
    assert!(full_prompt.contains("你醒了，你发现自己身处一个密闭房间"), "missing scene description");
    assert!(full_prompt.contains("### skill ###"), "missing skill section");
    assert!(full_prompt.contains("### 知识 ###"), "missing knowledge section");
    assert!(full_prompt.contains("### 经历 ###"), "missing history section");
    assert!(full_prompt.contains("### 近况 ###"), "missing sessions section");
    assert!(full_prompt.contains("### 环境信息 ###"), "missing environment section");
    assert!(full_prompt.contains("### current ###"), "missing current section");
    assert!(full_prompt.contains("### 当前状态 ###"), "missing status section");
    assert!(full_prompt.contains("### 输出规范 ###"), "missing output spec");
    assert!(full_prompt.contains(&format!("Action-end-{}", token)), "missing end marker");

    // Content richness assertions
    assert!(full_prompt.contains("App 开发指南"), "skill should contain app dev guide");
    assert!(full_prompt.contains("反向代理"), "skill should contain proxy details");
    assert!(full_prompt.contains("beat"), "knowledge should contain terminology");
    assert!(full_prompt.contains("SequenceGuard"), "knowledge should contain architecture");
    assert!(full_prompt.contains("镜与柴油"), "history should contain narrative");
    assert!(full_prompt.contains("ac56b3"), "sessions should contain agent sender ids");
    assert!(full_prompt.contains("summary"), "sessions should contain summary fields");
    assert!(full_prompt.contains("进化二号（引擎）"), "environment should contain instance name");
    assert!(full_prompt.contains("行为编号"), "current should contain action blocks");
    assert!(full_prompt.contains("exec result"), "current should contain script results");
    assert!(full_prompt.contains("未读来信"), "status should contain unread count");

    // Action format assertions — verify all action variants documented
    assert!(full_prompt.contains("idle"), "schema should contain idle");
    assert!(full_prompt.contains("read_msg"), "schema should contain read_msg");
    assert!(full_prompt.contains("send_msg"), "schema should contain send_msg");
    assert!(full_prompt.contains("thinking"), "schema should contain thinking");
    assert!(full_prompt.contains("script"), "schema should contain script");
    assert!(full_prompt.contains("write_file"), "schema should contain write_file");
    assert!(full_prompt.contains("replace_in_file"), "schema should contain replace_in_file");
    assert!(full_prompt.contains("summary"), "schema should contain summary");
    assert!(full_prompt.contains("distill"), "schema should contain distill");
    assert!(full_prompt.contains("set_profile"), "schema should contain set_profile");
    assert!(full_prompt.contains("create_instance"), "schema should contain create_instance");

    println!("Full prompt written to /tmp/beat_prompt_input.txt ({} bytes, ~{} chars)",
        full_prompt.len(), full_prompt.chars().count());
}

#[test]
#[ignore] // Requires network access to LLM endpoint
fn test_real_inference() {
    use mad_hatter::llm::{OpenAiChannel, stream_infer};

    let request = build_rich_mock_request();

    // Configure LLM channel (using test environment settings)
    let channel = OpenAiChannel::new(
        "http://localhost:9876",
        "claude-opus-4-6",
        "sk-114e7ac8a751468c94dbd3b61390e384",
    ).with_max_tokens(16384);

    // Also dump the input prompt
    let token = "06aebb";
    let input_prompt = build_prompt_for_dump(&request, token);
    std::fs::write("/tmp/beat_prompt_input.txt", &input_prompt).unwrap();
    println!("Input prompt written to /tmp/beat_prompt_input.txt ({} bytes)", input_prompt.len());

    // Real inference via stream_infer
    println!("Starting real LLM inference...");
    let stream = stream_infer::<BeatRequest, Action>(&channel, &request);

    match stream {
        Ok(stream_iter) => {
            let real_token = stream_iter.token().to_string();
            let mut output_parts: Vec<String> = Vec::new();
            let mut action_count = 0;

            for (i, result) in stream_iter.enumerate() {
                match result {
                    Ok(action) => {
                        action_count += 1;
                        let action_str = format!("--- Action {} ---\n{}\n", i + 1, action);
                        println!("{}", action_str);
                        output_parts.push(action_str);
                    }
                    Err(e) => {
                        let err_str = format!("--- Action {} (error) ---\n{}\n", i + 1, e);
                        println!("{}", err_str);
                        output_parts.push(err_str);
                    }
                }
            }

            let output = format!(
                "=== Real LLM Inference Output ===\nToken: {}\nActions parsed: {}\n\n{}",
                real_token,
                action_count,
                output_parts.join("\n")
            );

            std::fs::write("/tmp/beat_prompt_output.txt", &output).unwrap();
            println!("\nOutput written to /tmp/beat_prompt_output.txt ({} bytes)", output.len());
            println!("Total actions parsed: {}", action_count);

            assert!(action_count > 0, "LLM should produce at least one action");
        }
        Err(e) => {
            let error_output = format!("=== LLM Inference Failed ===\n{}\n", e);
            std::fs::write("/tmp/beat_prompt_output.txt", &error_output).unwrap();
            panic!("stream_infer failed: {}", e);
        }
    }
}

