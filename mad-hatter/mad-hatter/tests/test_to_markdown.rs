use mad_hatter::ToMarkdown;
use mad_hatter::llm::ToMarkdown as _;

// ============================================================
// P0 tests (preserved)
// ============================================================

#[allow(dead_code)]
#[derive(ToMarkdown)]
/// 你是一个知识提炼专家。请根据以下信息更新知识文档。
struct CaptureInput {
    #[markdown(skip)]
    end_marker: String,

    /// 当前知识内容
    knowledge: String,
    /// 近况
    recent: String,
    /// 当前增量
    current: String,
    /// 本次小结
    summary: String,
}

#[test]
fn test_capture_to_markdown() {
    let input = CaptureInput {
        end_marker: "END123".into(),
        knowledge: "已有知识...".into(),
        recent: "最近发生...".into(),
        current: "".into(),  // 空→应跳过
        summary: "本次总结...".into(),
    };
    let md = input.to_markdown();

    // struct级doc comment → 头部
    assert!(md.starts_with("你是一个知识提炼专家"), "should start with struct doc comment");

    // 字段级doc comment → section标题
    assert!(md.contains("### 当前知识内容 ###"), "should have knowledge section title");
    assert!(md.contains("已有知识..."), "should have knowledge content");

    assert!(md.contains("### 近况 ###"), "should have recent section title");
    assert!(md.contains("最近发生..."), "should have recent content");

    // 空字段跳过
    assert!(!md.contains("当前增量"), "empty field should be skipped entirely");

    // skip字段不渲染
    assert!(!md.contains("END123"), "skip field should not appear");
    assert!(!md.contains("end_marker"), "skip field name should not appear");

    assert!(md.contains("### 本次小结 ###"), "should have summary section title");
    assert!(md.contains("本次总结..."), "should have summary content");
}

#[test]
fn test_all_empty_fields() {
    let input = CaptureInput {
        end_marker: "TOKEN".into(),
        knowledge: "".into(),
        recent: "".into(),
        current: "".into(),
        summary: "".into(),
    };
    let md = input.to_markdown();
    // 只有头部，没有任何section
    assert!(md.contains("你是一个知识提炼专家"));
    assert!(!md.contains("###"));
}

#[derive(ToMarkdown)]
struct SimpleStruct {
    /// 名称
    name: String,
    /// 描述
    description: String,
}

#[test]
fn test_no_struct_doc_comment() {
    let s = SimpleStruct {
        name: "Alice".into(),
        description: "An AI engine".into(),
    };
    let md = s.to_markdown();
    // 无struct级doc → 没有头部文本，直接section
    assert!(md.contains("### 名称 ###"));
    assert!(md.contains("Alice"));
    assert!(md.contains("### 描述 ###"));
    assert!(md.contains("An AI engine"));
}

#[test]
fn test_field_without_doc_uses_field_name() {
    #[derive(ToMarkdown)]
    struct NoDocs {
        title: String,
    }
    let s = NoDocs { title: "Hello".into() };
    let md = s.to_markdown();
    // 无字段doc → 用字段名作标题
    assert!(md.contains("### title ###"));
    assert!(md.contains("Hello"));
}

// ============================================================
// P2 tests: nested struct
// ============================================================

#[derive(ToMarkdown)]
struct Inner {
    /// 发送者
    sender: String,
    /// 内容
    content: String,
}

#[derive(ToMarkdown)]
/// 顶层结构
struct Outer {
    /// 标题
    title: String,
    /// 详细信息
    detail: Inner,
}

#[test]
fn test_nested_struct() {
    let o = Outer {
        title: "一些标题文本".into(),
        detail: Inner {
            sender: "alice".into(),
            content: "你好".into(),
        },
    };
    let md = o.to_markdown();

    // 顶层header
    assert!(md.contains("顶层结构"), "should have struct doc header");

    // 顶层字段用 ###
    assert!(md.contains("### 标题 ###"), "title should use ### heading");
    assert!(md.contains("一些标题文本"), "title content");

    // 嵌套字段的section标题用 ###
    assert!(md.contains("### 详细信息 ###"), "nested field should have ### heading");

    // 嵌套struct的字段用 #### (depth+1)
    assert!(md.contains("#### 发送者 ####"), "nested struct fields should use #### heading");
    assert!(md.contains("alice"), "nested field content");
    assert!(md.contains("#### 内容 ####"), "nested struct fields should use #### heading");
    assert!(md.contains("你好"), "nested field content");
}

// ============================================================
// P2 tests: Vec field
// ============================================================

#[derive(ToMarkdown)]
struct Message {
    /// 发送者标识
    sender: String,
    /// 消息内容
    content: String,
}

#[derive(ToMarkdown)]
struct Chat {
    /// 最近的对话消息
    messages: Vec<Message>,
}

#[test]
fn test_vec_field() {
    let chat = Chat {
        messages: vec![
            Message { sender: "alice".into(), content: "你好".into() },
            Message { sender: "bob".into(), content: "世界".into() },
        ],
    };
    let md = chat.to_markdown();

    // Section标题
    assert!(md.contains("### 最近的对话消息 ###"), "should have vec section title");

    // 元素用compact格式 (field: value)
    assert!(md.contains("sender: alice"), "first element sender");
    assert!(md.contains("content: 你好"), "first element content");
    assert!(md.contains("sender: bob"), "second element sender");
    assert!(md.contains("content: 世界"), "second element content");

    // 注释只出现一次（section标题），不在每个元素重复
    let count = md.matches("最近的对话消息").count();
    assert_eq!(count, 1, "doc comment should appear only once");
}

#[test]
fn test_empty_vec_skipped() {
    let chat = Chat {
        messages: vec![],
    };
    let md = chat.to_markdown();

    // 空Vec → 整个字段不渲染
    assert!(!md.contains("最近的对话消息"), "empty vec should be skipped");
    assert!(!md.contains("###"), "no headings for empty struct");
}

// ============================================================
// P2 tests: basic types
// ============================================================

#[derive(ToMarkdown)]
struct Status {
    /// 未读消息数
    unread_count: usize,
    /// HTTP端口
    port: u16,
    /// 是否在线
    online: bool,
}

#[test]
fn test_basic_types() {
    let s = Status {
        unread_count: 5,
        port: 8080,
        online: true,
    };
    let md = s.to_markdown();

    assert!(md.contains("### 未读消息数 ###"), "usize field heading");
    assert!(md.contains("5"), "usize value");
    assert!(md.contains("### HTTP端口 ###"), "u16 field heading");
    assert!(md.contains("8080"), "u16 value");
    assert!(md.contains("### 是否在线 ###"), "bool field heading");
    assert!(md.contains("true"), "bool value");
}

// ============================================================
// P2 tests: Option<non-String>
// ============================================================

#[derive(ToMarkdown)]
struct Config {
    /// 超时时间
    timeout: Option<u64>,
    /// 名称
    name: Option<String>,
}

#[test]
fn test_option_non_string() {
    let c = Config {
        timeout: Some(30),
        name: Some("test".into()),
    };
    let md = c.to_markdown();

    assert!(md.contains("### 超时时间 ###"), "Option<u64> Some heading");
    assert!(md.contains("30"), "Option<u64> Some value");
    assert!(md.contains("### 名称 ###"), "Option<String> Some heading");
    assert!(md.contains("test"), "Option<String> Some value");
}

#[test]
fn test_option_none_skipped() {
    let c = Config {
        timeout: None,
        name: None,
    };
    let md = c.to_markdown();

    assert!(!md.contains("超时时间"), "Option<u64> None should be skipped");
    assert!(!md.contains("名称"), "Option<String> None should be skipped");
}

// ============================================================
// P2 tests: mixed nested (simplified BeatRequest)
// ============================================================

#[derive(ToMarkdown)]
struct PromptMsg {
    role: String,
    sender: String,
    content: String,
}

#[derive(ToMarkdown)]
struct SessionEntry {
    messages: Vec<PromptMsg>,
    /// 小结
    summary: String,
}

#[derive(ToMarkdown)]
struct SessionBlock {
    block_name: String,
    entries: Vec<SessionEntry>,
}

#[allow(dead_code)]
#[derive(ToMarkdown)]
/// 你醒了
struct BeatRequestMini {
    #[markdown(skip)]
    action_token: String,
    /// 你的知识
    knowledge: String,
    /// 近况
    session_blocks: Vec<SessionBlock>,
    /// 当前状态
    current: String,
}

#[test]
fn test_mixed_nested() {
    let req = BeatRequestMini {
        action_token: "TOKEN123".into(),
        knowledge: "一些知识".into(),
        session_blocks: vec![
            SessionBlock {
                block_name: "session1".into(),
                entries: vec![
                    SessionEntry {
                        messages: vec![
                            PromptMsg {
                                role: "user".into(),
                                sender: "alice".into(),
                                content: "hello".into(),
                            },
                        ],
                        summary: "对话总结".into(),
                    },
                ],
            },
        ],
        current: "当前状态内容".into(),
    };
    let md = req.to_markdown();

    // Header
    assert!(md.contains("你醒了"), "struct doc header");

    // Skip field
    assert!(!md.contains("TOKEN123"), "skip field should not appear");

    // String fields
    assert!(md.contains("### 你的知识 ###"), "knowledge heading");
    assert!(md.contains("一些知识"), "knowledge content");

    // Vec<SessionBlock> section
    assert!(md.contains("### 近况 ###"), "session_blocks heading");

    // Vec elements use compact format
    assert!(md.contains("block_name: session1"), "block_name in compact format");

    // Nested Vec<PromptMsg> elements
    assert!(md.contains("role: user"), "nested vec element");
    assert!(md.contains("sender: alice"), "nested vec element");
    assert!(md.contains("content: hello"), "nested vec element");

    // summary field
    assert!(md.contains("summary: 对话总结") || md.contains("小结"), "summary in compact format");

    // Current field
    assert!(md.contains("### 当前状态 ###"), "current heading");
    assert!(md.contains("当前状态内容"), "current content");
}