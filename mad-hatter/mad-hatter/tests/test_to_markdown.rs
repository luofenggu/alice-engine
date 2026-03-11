use mad_hatter::ToMarkdown;
use mad_hatter::llm::ToMarkdown as _;

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
