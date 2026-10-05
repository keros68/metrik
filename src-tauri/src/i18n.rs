//! 界面语言：持久化的设置值、生效语言的解析，以及后端自己产出的界面文字。
//!
//! 后端是语言的唯一权威：设置存在 `app_setting`（[`SETTING_KEY`]），启动时在
//! 任何窗口加载、托盘菜单构建之前解析出生效语言并写入 [`current`]；设置页改动
//! 经 `set_ui_language` 命令落库、更新托盘并广播 [`CHANGED_EVENT`]。
//!
//! 后端界面文字一律写成中英两份放在调用点：
//!
//! ```ignore
//! i18n::tr!("显示 / 隐藏", "Show / hide")            // -> String
//! i18n::tr!("剩余 {percent}%", "{percent}% left")      // 内联捕获的格式参数
//! i18n::tr_in!(lang, "置顶", "Pin")                    // 指定语言（纯函数、测试用）
//! ```
//!
//! 两份文字都按 `format!` 展开，参数用内联捕获的变量名；中文那份必须与转换前
//! 的字面量逐字节一致。日志（`eprintln!`）、写进数据库或文件的内容、与外部
//! 程序输出比对的文字都不是界面文字，不经过这里。

use serde::Serialize;
use std::sync::atomic::{AtomicU8, Ordering};

/// `app_setting` 中保存语言设置的键，值为 `auto` / `zh` / `en`。
pub const SETTING_KEY: &str = "ui_language";

/// 语言变化时广播给所有窗口的事件，载荷为 [`LanguageState`]。
pub const CHANGED_EVENT: &str = "metrik://ui-language";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Zh,
    En,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Setting {
    Auto,
    Zh,
    En,
}

impl Setting {
    /// 未知或缺失的值一律回到 `auto`。
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("zh") => Setting::Zh,
            Some("en") => Setting::En,
            _ => Setting::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Setting::Auto => "auto",
            Setting::Zh => "zh",
            Setting::En => "en",
        }
    }
}

/// 前端读取和事件广播共用的形状：用户的设置值与解析后的生效语言。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LanguageState {
    pub setting: Setting,
    pub language: Lang,
}

/// 系统 locale 的主语言子标签为 `zh` 时用中文，其余（含未知、`C`、`POSIX`、
/// 读不到）一律英文。接受 BCP 47（`zh-Hans-CN`）与 POSIX（`zh_CN.UTF-8`）两种写法。
pub fn locale_is_chinese(locale: Option<&str>) -> bool {
    let Some(locale) = locale else {
        return false;
    };
    let primary = locale
        .trim()
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or_default();
    primary.eq_ignore_ascii_case("zh")
}

pub fn resolve(setting: Setting, system_locale: Option<&str>) -> Lang {
    match setting {
        Setting::Zh => Lang::Zh,
        Setting::En => Lang::En,
        Setting::Auto if locale_is_chinese(system_locale) => Lang::Zh,
        Setting::Auto => Lang::En,
    }
}

/// Linux 按 POSIX 的消息类优先级读环境变量：`LC_ALL` > `LC_MESSAGES` > `LANG`，
/// 取第一个非空值。其它平台读系统界面语言。
pub fn system_locale() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux_locale(|name| std::env::var(name).ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        sys_locale::get_locale()
    }
}

#[cfg(any(target_os = "linux", test))]
fn linux_locale(read: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(read)
        .find(|value| !value.trim().is_empty())
}

// 0 = 中文，1 = 英文。未初始化时保持中文，与转换前的行为一致；
// 桌面启动时 `run()` 的 setup 会先写入生效语言。
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Lang {
    match CURRENT.load(Ordering::Acquire) {
        1 => Lang::En,
        _ => Lang::Zh,
    }
}

pub fn set_current(lang: Lang) {
    CURRENT.store(
        match lang {
            Lang::Zh => 0,
            Lang::En => 1,
        },
        Ordering::Release,
    );
}

/// 英文按数量取单复数形式：`plural(count, "session", "sessions")`。
pub fn plural<T: PartialEq + From<u8>>(
    count: T,
    one: &'static str,
    other: &'static str,
) -> &'static str {
    if count == T::from(1) {
        one
    } else {
        other
    }
}

/// 按指定语言二选一并格式化，返回 `String`。格式参数用内联捕获的变量名。
macro_rules! tr_in {
    ($lang:expr, $zh:literal, $en:literal $(,)?) => {{
        #[allow(clippy::useless_format)]
        let text = match $lang {
            $crate::i18n::Lang::Zh => ::std::format!($zh),
            $crate::i18n::Lang::En => ::std::format!($en),
        };
        text
    }};
}

/// 按当前生效语言二选一，见 [`tr_in`]。
macro_rules! tr {
    ($zh:literal, $en:literal $(,)?) => {
        $crate::i18n::tr_in!($crate::i18n::current(), $zh, $en)
    };
}

pub(crate) use tr;
pub(crate) use tr_in;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_the_primary_language_subtag() {
        for locale in ["zh-CN", "zh_CN.UTF-8", "zh-TW", "zh-Hans-CN", "ZH_cn", "zh"] {
            assert_eq!(resolve(Setting::Auto, Some(locale)), Lang::Zh, "{locale}");
        }
        for locale in [
            "en-US", "de-DE", "C", "POSIX", "", "  ", "zhx-CN", "C.UTF-8",
        ] {
            assert_eq!(resolve(Setting::Auto, Some(locale)), Lang::En, "{locale:?}");
        }
        assert_eq!(resolve(Setting::Auto, None), Lang::En);
    }

    #[test]
    fn explicit_setting_overrides_the_system_locale() {
        assert_eq!(resolve(Setting::Zh, Some("en-US")), Lang::Zh);
        assert_eq!(resolve(Setting::En, Some("zh-CN")), Lang::En);
        assert_eq!(resolve(Setting::Zh, None), Lang::Zh);
    }

    #[test]
    fn setting_values_parse_with_auto_as_the_fallback() {
        assert_eq!(Setting::parse(Some("zh")), Setting::Zh);
        assert_eq!(Setting::parse(Some("en")), Setting::En);
        assert_eq!(Setting::parse(Some("auto")), Setting::Auto);
        assert_eq!(Setting::parse(Some("fr")), Setting::Auto);
        assert_eq!(Setting::parse(None), Setting::Auto);
        for setting in [Setting::Auto, Setting::Zh, Setting::En] {
            assert_eq!(Setting::parse(Some(setting.as_str())), setting);
        }
    }

    #[test]
    fn setting_round_trips_through_app_setting() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::ensure_schema(&connection).unwrap();
        assert_eq!(
            Setting::parse(
                crate::storage::get_app_setting(&connection, SETTING_KEY)
                    .unwrap()
                    .as_deref()
            ),
            Setting::Auto
        );
        for setting in [Setting::En, Setting::Zh, Setting::Auto] {
            crate::storage::set_app_setting(&connection, SETTING_KEY, setting.as_str()).unwrap();
            let stored = crate::storage::get_app_setting(&connection, SETTING_KEY).unwrap();
            assert_eq!(Setting::parse(stored.as_deref()), setting);
        }
    }

    #[test]
    fn linux_locale_uses_posix_message_precedence() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        assert_eq!(
            linux_locale(env(&[("LANG", "zh_CN.UTF-8"), ("LC_ALL", "en_US.UTF-8")])).as_deref(),
            Some("en_US.UTF-8")
        );
        assert_eq!(
            linux_locale(env(&[
                ("LANG", "en_US.UTF-8"),
                ("LC_MESSAGES", "zh_CN.UTF-8")
            ]))
            .as_deref(),
            Some("zh_CN.UTF-8")
        );
        assert_eq!(
            linux_locale(env(&[("LC_ALL", ""), ("LANG", "zh_TW.UTF-8")])).as_deref(),
            Some("zh_TW.UTF-8")
        );
        assert_eq!(linux_locale(env(&[])), None);
    }

    #[test]
    fn tr_formats_inline_arguments_in_either_language() {
        let percent = 42;
        assert_eq!(
            tr_in!(Lang::Zh, "剩余 {percent}%", "{percent}% left"),
            "剩余 42%"
        );
        assert_eq!(
            tr_in!(Lang::En, "剩余 {percent}%", "{percent}% left"),
            "42% left"
        );
        assert_eq!(tr_in!(Lang::En, "置顶", "Pin"), "Pin");
    }

    #[test]
    fn plural_picks_the_singular_only_for_one() {
        assert_eq!(plural(1_usize, "day", "days"), "day");
        assert_eq!(plural(0_i64, "day", "days"), "days");
        assert_eq!(plural(2_u32, "day", "days"), "days");
    }
}
