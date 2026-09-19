//! 入库排序键与查询词共用同一规范化函数（02-doc §4.3）：
//! NFKC → 压缩连续空白 → 转小写。SQLite 侧比较再叠加 COLLATE NOCASE。

use unicode_normalization::UnicodeNormalization;

pub fn normalize(value: &str) -> String {
    let folded: String = value.nfkc().collect();
    folded
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn fullwidth_and_case_fold_to_ascii_lowercase() {
        assert_eq!(normalize("ＪＡＹ　Zhou"), "jay zhou");
        assert_eq!(normalize("ÉCLIPSE"), "éclipse");
    }

    #[test]
    fn whitespace_collapsed_and_trimmed() {
        assert_eq!(normalize("  夜曲\t\n  周杰伦 "), "夜曲 周杰伦");
    }

    #[test]
    fn chinese_untouched() {
        assert_eq!(normalize("周杰伦的床边故事"), "周杰伦的床边故事");
    }
}
