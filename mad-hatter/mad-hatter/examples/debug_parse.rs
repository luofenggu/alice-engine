use mad_hatter::{FromMarkdown, ToMarkdown};
use mad_hatter::llm::FromMarkdown as _;

#[derive(ToMarkdown, FromMarkdown, PartialEq, Debug, Clone)]
struct ReplaceBlock {
    search: String,
    replace: String,
}

#[derive(FromMarkdown, PartialEq, Debug)]
enum Action {
    Idle,
    IdleWithParam { seconds: Option<u16> },
    Thinking { content: String },
    Script { content: String },
    Summary { content: String },
    SendMsg { recipient: String, content: String },
    WriteFile { file_path: String, content: String },
    ReplaceInFile { file_path: String, blocks: Vec<ReplaceBlock> },
    Distill { action_id: String, summary: String },
    SetProfile { settings: String },
    CreateInstance { name: String, knowledge: String },
}

fn main() {
    let token = "abc123";
    
    // Test send_msg
    let input = format!(
        "Action-{token}\nsend_msg\nrecipient-{token}\nuser\ncontent-{token}\nhello world\nAction-end-{token}",
        token = token
    );
    println!("=== send_msg ===");
    println!("Input: {:?}", input);
    match Action::from_markdown(&input, token) {
        Ok(r) => println!("OK: {:?}", r),
        Err(e) => println!("ERR: {}", e),
    }
    
    // Test idle_with_param
    let input2 = format!(
        "Action-{token}\nidle_with_param\n120\nAction-end-{token}",
        token = token
    );
    println!("\n=== idle_with_param ===");
    match Action::from_markdown(&input2, token) {
        Ok(r) => println!("OK: {:?}", r),
        Err(e) => println!("ERR: {}", e),
    }
    
    // Test replace_in_file
    let input3 = format!(
        "Action-{token}\nreplace_in_file\nfile_path-{token}\nsrc/lib.rs\nblocks-{token}\nReplaceBlock-{token}\nsearch-{token}\nold code\nreplace-{token}\nnew code\nReplaceBlock-end-{token}\nAction-end-{token}",
        token = token
    );
    println!("\n=== replace_in_file ===");
    println!("Input: {:?}", input3);
    match Action::from_markdown(&input3, token) {
        Ok(r) => println!("OK: {:?}", r),
        Err(e) => println!("ERR: {}", e),
    }

    // Print schema
    println!("\n=== SCHEMA ===");
    println!("{}", Action::schema_markdown(token));
}
