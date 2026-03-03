/// Numeric counter — wraps an integer, hides literal 0/1 inside methods.
///
/// Usage: `let mut c = Counter::<u32>::new();`
/// then `c.increment()`, `c.reset()`, `c.value()` for comparisons.
pub struct Counter<T>(T);

impl Counter<u32> {
    pub fn new() -> Self { Self(0) }
    pub fn increment(&mut self) { self.0 += 1; }
    pub fn reset(&mut self) { self.0 = 0; }
    pub fn value(&self) -> u32 { self.0 }
}

impl Counter<u64> {
    pub fn new() -> Self { Self(0) }
    pub fn add(&mut self, n: u64) { self.0 += n; }
    pub fn reset(&mut self) { self.0 = 0; }
    pub fn value(&self) -> u64 { self.0 }
}

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

    #[test]
    fn test_counter_u32() {
        let mut c = Counter::<u32>::new();
        assert_eq!(c.value(), 0);
        c.increment();
        assert_eq!(c.value(), 1);
        c.increment();
        assert_eq!(c.value(), 2);
        c.reset();
        assert_eq!(c.value(), 0);
    }

    #[test]
    fn test_counter_u64() {
        let mut c = Counter::<u64>::new();
        assert_eq!(c.value(), 0);
        c.add(5);
        assert_eq!(c.value(), 5);
        c.add(3);
        assert_eq!(c.value(), 8);
        c.reset();
        assert_eq!(c.value(), 0);
    }
}
