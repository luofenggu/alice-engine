use mad_hatter::ToMarkdown;
use mad_hatter::llm::ToMarkdown as _;

// --- Helper types ---

#[derive(ToMarkdown)]
struct InnerData {
    title: String,
    count: u64,
}

// --- Struct flatten tests ---

#[derive(ToMarkdown)]
struct RecordWithFlatten {
    record_id: String,
    #[markdown(flatten)]
    data: InnerData,
}

#[test]
fn test_struct_flatten_depth() {
    let r = RecordWithFlatten {
        record_id: "REC001".to_string(),
        data: InnerData { title: "hello".to_string(), count: 42 },
    };
    let out = r.to_markdown_depth(0);
    // record_id should render normally
    assert!(out.contains("record_id: REC001"), "got: {}", out);
    // flatten: no section title for data, inner fields rendered at same depth
    assert!(!out.contains("### data ###"), "should not have data section title, got: {}", out);
    assert!(out.contains("title: hello"), "got: {}", out);
    assert!(out.contains("count"), "got: {}", out);
    assert!(out.contains("42"), "got: {}", out);
}

#[test]
fn test_struct_flatten_item() {
    let r = RecordWithFlatten {
        record_id: "REC001".to_string(),
        data: InnerData { title: "hello".to_string(), count: 42 },
    };
    let out = r.to_markdown_item();
    assert!(out.contains("record_id: REC001"), "got: {}", out);
    // flatten: no "data: " prefix
    assert!(!out.contains("data:"), "should not have 'data:' prefix, got: {}", out);
    assert!(out.contains("title: hello"), "got: {}", out);
    assert!(out.contains("count: 42"), "got: {}", out);
}

// --- Struct flatten with Option ---

#[derive(ToMarkdown)]
struct RecordWithOptionalFlatten {
    record_id: String,
    #[markdown(flatten)]
    data: Option<InnerData>,
}

#[test]
fn test_struct_flatten_option_some_depth() {
    let r = RecordWithOptionalFlatten {
        record_id: "REC002".to_string(),
        data: Some(InnerData { title: "world".to_string(), count: 7 }),
    };
    let out = r.to_markdown_depth(0);
    assert!(out.contains("record_id: REC002"), "got: {}", out);
    assert!(!out.contains("### data ###"), "got: {}", out);
    assert!(out.contains("title: world"), "got: {}", out);
    assert!(out.contains("7"), "got: {}", out);
}

#[test]
fn test_struct_flatten_option_none_depth() {
    let r = RecordWithOptionalFlatten {
        record_id: "REC003".to_string(),
        data: None,
    };
    let out = r.to_markdown_depth(0);
    assert!(out.contains("record_id: REC003"), "got: {}", out);
    assert!(!out.contains("title"), "got: {}", out);
    assert!(!out.contains("count"), "got: {}", out);
}

#[test]
fn test_struct_flatten_option_some_item() {
    let r = RecordWithOptionalFlatten {
        record_id: "REC002".to_string(),
        data: Some(InnerData { title: "world".to_string(), count: 7 }),
    };
    let out = r.to_markdown_item();
    assert!(!out.contains("data:"), "got: {}", out);
    assert!(out.contains("title: world"), "got: {}", out);
    assert!(out.contains("count: 7"), "got: {}", out);
}

#[test]
fn test_struct_flatten_option_none_item() {
    let r = RecordWithOptionalFlatten {
        record_id: "REC003".to_string(),
        data: None,
    };
    let out = r.to_markdown_item();
    assert!(!out.contains("data"), "got: {}", out);
}

// --- Enum flatten tests ---

#[derive(ToMarkdown)]
enum ActionWithFlatten {
    #[allow(dead_code)]
    Simple { message: String },
    WithNested {
        action_id: String,
        #[markdown(flatten)]
        detail: InnerData,
    },
    WithOptionalNested {
        action_id: String,
        #[markdown(flatten)]
        detail: Option<InnerData>,
    },
}

#[test]
fn test_enum_flatten_depth() {
    let a = ActionWithFlatten::WithNested {
        action_id: "ACT001".to_string(),
        detail: InnerData { title: "nested".to_string(), count: 99 },
    };
    let out = a.to_markdown_depth(0);
    assert!(out.contains("with_nested"), "got: {}", out);
    assert!(out.contains("action_id: ACT001"), "got: {}", out);
    assert!(!out.contains("### detail ###"), "got: {}", out);
    assert!(out.contains("title: nested"), "got: {}", out);
    assert!(out.contains("99"), "got: {}", out);
}

#[test]
fn test_enum_flatten_item() {
    let a = ActionWithFlatten::WithNested {
        action_id: "ACT001".to_string(),
        detail: InnerData { title: "nested".to_string(), count: 99 },
    };
    let out = a.to_markdown_item();
    assert!(out.contains("with_nested"), "got: {}", out);
    assert!(out.contains("action_id: ACT001"), "got: {}", out);
    assert!(!out.contains("detail:"), "got: {}", out);
    assert!(out.contains("title: nested"), "got: {}", out);
    assert!(out.contains("count: 99"), "got: {}", out);
}

#[test]
fn test_enum_flatten_option_some_depth() {
    let a = ActionWithFlatten::WithOptionalNested {
        action_id: "ACT002".to_string(),
        detail: Some(InnerData { title: "opt".to_string(), count: 5 }),
    };
    let out = a.to_markdown_depth(0);
    assert!(!out.contains("### detail ###"), "got: {}", out);
    assert!(out.contains("title: opt"), "got: {}", out);
}

#[test]
fn test_enum_flatten_option_none_depth() {
    let a = ActionWithFlatten::WithOptionalNested {
        action_id: "ACT003".to_string(),
        detail: None,
    };
    let out = a.to_markdown_depth(0);
    assert!(out.contains("action_id: ACT003"), "got: {}", out);
    assert!(!out.contains("title"), "got: {}", out);
}

#[test]
fn test_enum_flatten_option_some_item() {
    let a = ActionWithFlatten::WithOptionalNested {
        action_id: "ACT002".to_string(),
        detail: Some(InnerData { title: "opt".to_string(), count: 5 }),
    };
    let out = a.to_markdown_item();
    assert!(!out.contains("detail:"), "got: {}", out);
    assert!(out.contains("title: opt"), "got: {}", out);
}

#[test]
fn test_enum_flatten_option_none_item() {
    let a = ActionWithFlatten::WithOptionalNested {
        action_id: "ACT003".to_string(),
        detail: None,
    };
    let out = a.to_markdown_item();
    assert!(!out.contains("detail"), "got: {}", out);
}

// --- Non-flatten baseline: verify normal behavior still works ---

#[derive(ToMarkdown)]
struct RecordWithoutFlatten {
    record_id: String,
    data: InnerData,
}

#[test]
fn test_struct_no_flatten_has_section_title() {
    let r = RecordWithoutFlatten {
        record_id: "REC004".to_string(),
        data: InnerData { title: "normal".to_string(), count: 1 },
    };
    let out = r.to_markdown_depth(0);
    // Without flatten, should have section title
    assert!(out.contains("# data #"), "should have data section title, got: {}", out);
}