//! 项目分组规则：账本只存事件发生时的原始工作目录（既成事实），目录怎么归并
//! 成"项目"是展示层配置，存在 `app_setting`，改规则立即生效，不触发重扫。
//!
//! 对每个原始 cwd 的解析顺序：
//! 1. 用户规则里最长的路径前缀（登记的项目根 → 归并；隐藏 → 不作为项目）。
//!    用户规则在内置隐藏之前，所以登记 `~/Downloads/foo` 能把它从默认隐藏里捞回来。
//! 2. 内置隐藏：家目录本身、家目录下的点目录（`~/.claude` 等）、`~/Downloads`、
//!    系统临时目录。这些目录出现在项目列表里对谁都没意义。
//! 3. 向上找 `.git`（目录或 worktree 的 `.git` 文件都算）合并到仓库根；
//!    目录已不存在时跳过。
//! 4. 都不中：按原样目录呈现。

use crate::domain::normalize_project_path;
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const RULES_SETTING_KEY: &str = "project_grouping_rules";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectRules {
    /// 手动登记的项目根：其下所有 cwd 归并到该目录。
    pub roots: Vec<String>,
    /// 手动隐藏的目录前缀：其下用量不作为项目展示。
    pub hidden: Vec<String>,
}

impl ProjectRules {
    /// 归一化并去重；空白路径丢弃。同一路径同时出现在 roots 与 hidden 时
    /// roots 胜出——登记为项目是更明确的意图。
    pub fn normalized(self) -> Self {
        let mut roots: Vec<String> = Vec::new();
        for path in self.roots {
            if let Some(normalized) = normalize_project_path(&path) {
                if !roots.contains(&normalized) {
                    roots.push(normalized);
                }
            }
        }
        let mut hidden: Vec<String> = Vec::new();
        for path in self.hidden {
            if let Some(normalized) = normalize_project_path(&path) {
                if !hidden.contains(&normalized) && !roots.contains(&normalized) {
                    hidden.push(normalized);
                }
            }
        }
        Self { roots, hidden }
    }
}

pub fn load_rules(connection: &Connection) -> Result<ProjectRules> {
    let Some(raw) = crate::storage::get_app_setting(connection, RULES_SETTING_KEY)? else {
        return Ok(ProjectRules::default());
    };
    // 损坏的配置按默认规则继续，不让整个用量页因此打不开。
    Ok(serde_json::from_str::<ProjectRules>(&raw)
        .map(ProjectRules::normalized)
        .unwrap_or_default())
}

pub fn save_rules(connection: &Connection, rules: ProjectRules) -> Result<ProjectRules> {
    let rules = rules.normalized();
    let raw = serde_json::to_string(&rules).context("failed to serialize project rules")?;
    crate::storage::set_app_setting(connection, RULES_SETTING_KEY, &raw)?;
    Ok(rules)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// 归属到一个项目；`pinned` 表示命中了手动登记的项目根。
    Project { path: String, pinned: bool },
    /// 命中隐藏规则（用户或内置），不作为项目展示。
    Hidden,
}

pub struct ProjectResolver {
    rules: ProjectRules,
    home: String,
    temp: String,
    /// 测试注入：替代真实文件系统的 `.git` 向上查找。
    git_lookup: fn(&str) -> Option<String>,
    cache: HashMap<String, Resolution>,
}

impl ProjectResolver {
    pub fn new(rules: ProjectRules) -> Self {
        let home = dirs::home_dir()
            .and_then(|dir| normalize_project_path(&dir.to_string_lossy()))
            .unwrap_or_default();
        let temp =
            normalize_project_path(&std::env::temp_dir().to_string_lossy()).unwrap_or_default();
        Self::with_environment(rules, home, temp, git_root)
    }

    fn with_environment(
        rules: ProjectRules,
        home: String,
        temp: String,
        git_lookup: fn(&str) -> Option<String>,
    ) -> Self {
        Self {
            rules,
            home,
            temp,
            git_lookup,
            cache: HashMap::new(),
        }
    }

    /// 解析一个归一化后的原始 cwd。结果按原样字符串缓存——同一次查询里
    /// 重复路径很多，而 `.git` 探测是文件系统操作。
    pub fn resolve(&mut self, raw: &str) -> Resolution {
        if let Some(cached) = self.cache.get(raw) {
            return cached.clone();
        }
        let resolution = self.resolve_uncached(raw);
        self.cache.insert(raw.to_owned(), resolution.clone());
        resolution
    }

    fn resolve_uncached(&self, raw: &str) -> Resolution {
        // 用户规则：roots 与 hidden 一起取最长前缀，最具体的规则胜出。
        // 这样"隐藏 ~/code、但登记 ~/code/foo"能让 foo 独活。
        let mut best: Option<(usize, bool)> = None; // (前缀长度, 是否项目根)
        for root in &self.rules.roots {
            if is_within(raw, root) && best.is_none_or(|(len, _)| root.len() > len) {
                best = Some((root.len(), true));
            }
        }
        for hidden in &self.rules.hidden {
            if is_within(raw, hidden) && best.is_none_or(|(len, _)| hidden.len() > len) {
                best = Some((hidden.len(), false));
            }
        }
        if let Some((length, is_root)) = best {
            if is_root {
                let path = self
                    .rules
                    .roots
                    .iter()
                    .find(|root| root.len() == length && is_within(raw, root))
                    .cloned()
                    .unwrap_or_else(|| raw.to_owned());
                return Resolution::Project { path, pinned: true };
            }
            return Resolution::Hidden;
        }

        if self.builtin_hidden(raw) {
            return Resolution::Hidden;
        }

        if let Some(repo_root) = (self.git_lookup)(raw) {
            return Resolution::Project {
                path: repo_root,
                pinned: false,
            };
        }

        Resolution::Project {
            path: raw.to_owned(),
            pinned: false,
        }
    }

    fn builtin_hidden(&self, path: &str) -> bool {
        if !self.home.is_empty() {
            if path_equals(path, &self.home) {
                return true;
            }
            // 家目录下的点目录（~/.claude、~/.codex 等）与下载目录。
            let dot_prefix = format!("{}/.", self.home);
            if starts_with_ci(path, &dot_prefix) {
                return true;
            }
            let downloads = format!("{}/Downloads", self.home);
            if is_within(path, &downloads) {
                return true;
            }
        }
        if !self.temp.is_empty() && is_within(path, &self.temp) {
            return true;
        }
        // Unix 系统的公共临时目录（std::env::temp_dir 在 macOS 上是 /var/folders/…）。
        is_within(path, "/tmp") || is_within(path, "/private/tmp")
    }
}

/// 路径前缀匹配，按路径分段对齐（"D:/work/usa" 不匹配 "D:/work/usage"）。
/// Windows 与 macOS 上大小写不敏感——NTFS 与默认 APFS 都不区分大小写，
/// 手动输入的规则路径与日志记录的大小写可能不一致；Linux 保持区分。
fn is_within(path: &str, root: &str) -> bool {
    if root.is_empty() {
        return false;
    }
    if path_equals(path, root) {
        return true;
    }
    path.len() > root.len() && starts_with_ci(path, root) && path.as_bytes()[root.len()] == b'/'
}

fn path_equals(left: &str, right: &str) -> bool {
    if path_compare_ignores_case() {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn path_compare_ignores_case() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

/// 按字节比较前缀：路径可能含多字节字符（中文目录名），在任意字节位置
/// 做 `&str` 切片会 panic；字节级 `eq_ignore_ascii_case` 对非 ASCII 字节
/// 按原样比较，对 ASCII 忽略大小写，两边都正确。
fn starts_with_ci(path: &str, prefix: &str) -> bool {
    path.len() >= prefix.len()
        && ascii_insensitive_eq(&path.as_bytes()[..prefix.len()], prefix.as_bytes())
}

fn ascii_insensitive_eq(left: &[u8], right: &[u8]) -> bool {
    if path_compare_ignores_case() {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

/// 从给定目录向上找最近的 `.git`（目录或 worktree 的 `.git` 文件）。
/// 不做 canonicalize：账本里的路径形态就是展示形态，规避 UNC 前缀与大小写改写。
fn git_root(raw: &str) -> Option<String> {
    let start = PathBuf::from(raw);
    if !start.is_dir() {
        return None;
    }
    let mut current: &Path = &start;
    loop {
        if current.join(".git").exists() {
            return normalize_project_path(&current.to_string_lossy());
        }
        current = current.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_git(_: &str) -> Option<String> {
        None
    }

    fn resolver(rules: ProjectRules) -> ProjectResolver {
        ProjectResolver::with_environment(
            rules,
            "C:/Users/tester".into(),
            "C:/Users/tester/AppData/Local/Temp".into(),
            no_git,
        )
    }

    #[test]
    fn manual_roots_absorb_subdirectories_and_mark_pinned() {
        let mut resolver = resolver(ProjectRules {
            roots: vec!["F:/OneDrive/budget-app".into()],
            hidden: vec![],
        });

        assert_eq!(
            resolver.resolve("F:/OneDrive/budget-app/data/raw"),
            Resolution::Project {
                path: "F:/OneDrive/budget-app".into(),
                pinned: true
            }
        );
        assert_eq!(
            resolver.resolve("F:/OneDrive/budget-app"),
            Resolution::Project {
                path: "F:/OneDrive/budget-app".into(),
                pinned: true
            }
        );
        // 相邻目录不受影响：前缀必须按路径分段对齐。
        assert_eq!(
            resolver.resolve("F:/OneDrive/budget-app-legacy"),
            Resolution::Project {
                path: "F:/OneDrive/budget-app-legacy".into(),
                pinned: false
            }
        );
    }

    #[test]
    fn the_most_specific_rule_wins_between_roots_and_hidden() {
        let mut resolver = resolver(ProjectRules {
            roots: vec!["D:/code/foo".into()],
            hidden: vec!["D:/code".into()],
        });

        assert_eq!(
            resolver.resolve("D:/code/foo/src"),
            Resolution::Project {
                path: "D:/code/foo".into(),
                pinned: true
            }
        );
        assert_eq!(resolver.resolve("D:/code/bar"), Resolution::Hidden);
    }

    #[test]
    fn builtin_hidden_covers_home_dotdirs_downloads_and_temp() {
        let mut resolver = resolver(ProjectRules::default());

        assert_eq!(resolver.resolve("C:/Users/tester"), Resolution::Hidden);
        assert_eq!(
            resolver.resolve("C:/Users/tester/.claude"),
            Resolution::Hidden
        );
        assert_eq!(
            resolver.resolve("C:/Users/tester/Downloads/unzipped"),
            Resolution::Hidden
        );
        assert_eq!(
            resolver.resolve("C:/Users/tester/AppData/Local/Temp/scratch"),
            Resolution::Hidden
        );
        // 家目录下的普通目录不受影响。
        assert_eq!(
            resolver.resolve("C:/Users/tester/code"),
            Resolution::Project {
                path: "C:/Users/tester/code".into(),
                pinned: false
            }
        );
    }

    /// 后台派发工具（aicross 等）把会话跑在系统临时目录的 scratchpad 里，用量
    /// 被内置规则整体隐藏，只剩"未计入项目"一个计数。设计的出口是登记项目根：
    /// 用户规则先于内置规则判定，登记 scratchpad 的上级目录即可让这部分会话
    /// 作为普通项目下钻。此测试钉住这条逃生门，防止内置隐藏反向吞掉用户根。
    #[test]
    fn a_registered_root_inside_system_temp_overrides_the_builtin_hiding() {
        let mut resolver = resolver(ProjectRules {
            roots: vec!["C:/Users/tester/AppData/Local/Temp/claude".into()],
            hidden: vec![],
        });

        assert_eq!(
            resolver.resolve(
                "C:/Users/tester/AppData/Local/Temp/claude/F--OneDrive--/scratchpad/kimi_review"
            ),
            Resolution::Project {
                path: "C:/Users/tester/AppData/Local/Temp/claude".into(),
                pinned: true
            }
        );
        // scratchpad 之外、未登记的临时目录照旧隐藏。
        assert_eq!(
            resolver.resolve("C:/Users/tester/AppData/Local/Temp/aicross-demo"),
            Resolution::Hidden
        );
    }

    /// NTFS 与默认 APFS 不区分大小写：手动输入的规则大小写和日志记录
    /// 不一致时也要命中。Linux 区分大小写，此测试不适用。
    #[test]
    #[cfg(any(windows, target_os = "macos"))]
    fn rule_matching_ignores_ascii_case_on_case_insensitive_platforms() {
        let mut resolver = resolver(ProjectRules {
            roots: vec!["/Users/tester/Work/Metrik".into()],
            hidden: vec![],
        });

        assert_eq!(
            resolver.resolve("/Users/tester/work/metrik/src"),
            Resolution::Project {
                path: "/Users/tester/Work/Metrik".into(),
                pinned: true
            }
        );
    }

    #[test]
    fn multibyte_paths_match_rules_without_panicking() {
        // 真机日志里的实际形态：中文目录名。前缀比较必须按字节进行，
        // 在字符中间切 &str 会 panic。
        let mut resolver = resolver(ProjectRules {
            roots: vec!["F:/OneDrive/08预算编制/budget-app".into()],
            hidden: vec!["F:/OneDrive/19-论文".into()],
        });

        assert_eq!(
            resolver.resolve("F:/OneDrive/08预算编制/budget-app/data/raw"),
            Resolution::Project {
                path: "F:/OneDrive/08预算编制/budget-app".into(),
                pinned: true
            }
        );
        assert_eq!(
            resolver.resolve("F:/OneDrive/19-论文/ch1"),
            Resolution::Hidden
        );
        // 比登记根短的中文路径走原样兜底，不 panic。
        assert_eq!(
            resolver.resolve("F:/OneDrive/08预算编制"),
            Resolution::Project {
                path: "F:/OneDrive/08预算编制".into(),
                pinned: false
            }
        );
    }

    #[test]
    fn user_rules_override_builtin_hidden() {
        let mut resolver = resolver(ProjectRules {
            roots: vec!["C:/Users/tester/Downloads/real-project".into()],
            hidden: vec![],
        });

        assert_eq!(
            resolver.resolve("C:/Users/tester/Downloads/real-project/src"),
            Resolution::Project {
                path: "C:/Users/tester/Downloads/real-project".into(),
                pinned: true
            }
        );
        assert_eq!(
            resolver.resolve("C:/Users/tester/Downloads/other"),
            Resolution::Hidden
        );
    }

    #[test]
    fn git_lookup_rolls_paths_up_to_the_repository_root() {
        fn fake_git(path: &str) -> Option<String> {
            path.starts_with("D:/work/repo")
                .then(|| "D:/work/repo".to_owned())
        }
        let mut resolver = ProjectResolver::with_environment(
            ProjectRules::default(),
            "C:/Users/tester".into(),
            "C:/Users/tester/AppData/Local/Temp".into(),
            fake_git,
        );

        assert_eq!(
            resolver.resolve("D:/work/repo/src/ui"),
            Resolution::Project {
                path: "D:/work/repo".into(),
                pinned: false
            }
        );
        // 找不到 .git 的按原样呈现。
        assert_eq!(
            resolver.resolve("E:/loose/dir"),
            Resolution::Project {
                path: "E:/loose/dir".into(),
                pinned: false
            }
        );
    }

    #[test]
    fn real_git_walkup_finds_a_marker_in_an_ancestor() {
        let base = std::env::temp_dir().join(format!(
            "metrik-projects-git-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let nested = base.join("repo").join("crates").join("core");
        std::fs::create_dir_all(&nested).unwrap();
        // worktree 场景下 .git 是文件不是目录，两者都要认。
        std::fs::write(base.join("repo").join(".git"), "gitdir: elsewhere").unwrap();

        let raw = normalize_project_path(&nested.to_string_lossy()).unwrap();
        let expected = normalize_project_path(&base.join("repo").to_string_lossy()).unwrap();
        assert_eq!(git_root(&raw), Some(expected));
        assert_eq!(git_root("Z:/definitely/not/a/dir"), None);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn rules_round_trip_through_storage_with_normalization() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();

        assert_eq!(load_rules(&connection).unwrap(), ProjectRules::default());

        let saved = save_rules(
            &connection,
            ProjectRules {
                roots: vec![
                    "d:\\Work\\usage\\".into(),
                    "D:/Work/usage".into(),
                    "  ".into(),
                ],
                hidden: vec!["D:/Work/usage".into(), "C:\\scrap".into()],
            },
        )
        .unwrap();

        // 反斜杠归一、盘符大写、去尾斜杠、去重；roots 与 hidden 冲突时 roots 胜出。
        assert_eq!(saved.roots, vec!["D:/Work/usage".to_owned()]);
        assert_eq!(saved.hidden, vec!["C:/scrap".to_owned()]);
        assert_eq!(load_rules(&connection).unwrap(), saved);
    }

    #[test]
    fn corrupt_stored_rules_degrade_to_defaults() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .unwrap();
        crate::storage::set_app_setting(&connection, RULES_SETTING_KEY, "not json").unwrap();

        assert_eq!(load_rules(&connection).unwrap(), ProjectRules::default());
    }
}
