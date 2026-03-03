use std::collections::HashMap;

/// 骨架提取配置 — 极简程序分析
/// 从声明式定义文件(skeleton.toml)加载各语言的骨架提取规则
pub struct SkeletonConfig {
    /// 扩展名 → 行前缀标记列表
    markers: HashMap<String, Vec<String>>,
    /// 全文显示的扩展名集合
    full_extensions: Vec<String>,
}

/// 骨架提取结果
pub enum ExtractionResult {
    /// 全文显示（md等配置的扩展名）
    Full,
    /// 骨架行
    Skeleton(Vec<String>),
    /// 无规则，调用方自行决定fallback
    NoRule,
}

#[derive(serde::Deserialize)]
struct TomlRoot {
    languages: HashMap<String, LanguageDef>,
    #[serde(default)]
    full_extensions: Vec<String>,
}

#[derive(serde::Deserialize)]
struct LanguageDef {
    extensions: Vec<String>,
    markers: Vec<String>,
}

impl SkeletonConfig {
    /// 全局单例访问
    pub fn get() -> &'static Self {
        static INSTANCE: std::sync::OnceLock<SkeletonConfig> = std::sync::OnceLock::new();
        INSTANCE.get_or_init(Self::load)
    }

    fn load() -> Self {
        let toml_str = include_str!("skeleton.toml");
        let root: TomlRoot = toml::from_str(toml_str)
            .expect("Failed to parse skeleton.toml");

        let mut markers = HashMap::new();
        for lang in root.languages.values() {
            for ext in &lang.extensions {
                markers.insert(ext.clone(), lang.markers.clone());
            }
        }

        SkeletonConfig {
            markers,
            full_extensions: root.full_extensions,
        }
    }

    /// 从文件路径和内容提取骨架
    /// 内部处理扩展名提取、全文显示判断、骨架提取
    pub fn extract_from_path(&self, path: &str, content: &str) -> ExtractionResult {
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();

        // 全文显示的扩展名
        if self.full_extensions.iter().any(|e| e == &ext) {
            return ExtractionResult::Full;
        }

        // 尝试骨架提取
        if let Some(skeleton) = self.extract(&ext, content) {
            if !skeleton.is_empty() {
                return ExtractionResult::Skeleton(skeleton);
            }
        }

        ExtractionResult::NoRule
    }

    /// 根据文件扩展名提取骨架行
    /// 返回None表示该扩展名没有配置规则
    fn extract(&self, ext: &str, content: &str) -> Option<Vec<String>> {
        let markers = self.markers.get(ext)?;
        let skeleton: Vec<String> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| {
                let trimmed = line.trim_start();
                markers.iter().any(|m| trimmed.starts_with(m))
            })
            .map(|(i, line)| format!("{:>4}: {}", i + 1, line))
            .collect();
        Some(skeleton)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load() {
        let config = SkeletonConfig::load();
        assert!(config.markers.contains_key("rs"));
        assert!(config.markers.contains_key("py"));
        assert!(config.markers.contains_key("js"));
    }

    #[test]
    fn test_extract_rust() {
        let config = SkeletonConfig::load();
        let result = config.extract("rs", "use std::io;\n\npub fn hello() {\n    println!(\"hi\");\n}\n\nstruct Foo;").unwrap();
        assert!(result.iter().any(|l| l.contains("pub fn hello")));
        assert!(result.iter().any(|l| l.contains("struct Foo")));
    }

    #[test]
    fn test_unknown_ext() {
        let config = SkeletonConfig::load();
        assert!(config.extract("xyz", "anything").is_none());
    }

    #[test]
    fn test_extract_from_path_md() {
        let config = SkeletonConfig::load();
        assert!(matches!(config.extract_from_path("readme.md", "# Hello"), ExtractionResult::Full));
    }

    #[test]
    fn test_extract_from_path_rust() {
        let config = SkeletonConfig::load();
        let result = config.extract_from_path("main.rs", "pub fn main() {}");
        assert!(matches!(result, ExtractionResult::Skeleton(_)));
    }

    #[test]
    fn test_extract_from_path_unknown() {
        let config = SkeletonConfig::load();
        assert!(matches!(config.extract_from_path("data.xyz", "anything"), ExtractionResult::NoRule));
    }
}