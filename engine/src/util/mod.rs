/// Replace `search` with `replace` in `content` exactly once.
/// Returns `Err(count)` if `search` matches 0 or more than 1 times.
pub fn replace_once(content: &str, search: &str, replace: &str) -> Result<String, usize> {
    let count = content.matches(search).count();
    if count != 1 {
        return Err(count);
    }
    Ok(content.replacen(search, replace, 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replace_once_success() {
        let result = replace_once("hello world", "world", "rust");
        assert_eq!(result.unwrap(), "hello rust");
    }

    #[test]
    fn test_replace_once_zero_matches() {
        let result = replace_once("hello world", "xyz", "rust");
        assert_eq!(result.unwrap_err(), 0);
    }

    #[test]
    fn test_replace_once_multiple_matches() {
        let result = replace_once("aaa", "a", "b");
        assert_eq!(result.unwrap_err(), 3);
    }
}
