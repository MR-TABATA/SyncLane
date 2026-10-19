//! 保存先と、取り込み元の場所。OS ごとの違いは、ここだけに置く。
//!
//! macOS の場所は、これまでと同じ(`~/Library/Application Support/SyncLane`)。
//! 環境変数は、差し替えられる形(`*_with`)で読むので、別の OS の分岐も、この Mac で試せる。

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Windows,
    Other,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Other
        }
    }
}

type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

fn real_env(key: &str) -> Option<OsString> {
    std::env::var_os(key)
}

/// ホームのフォルダ。macOS・Linux は `HOME`、Windows は `USERPROFILE`(無ければ `HOMEDRIVE` + `HOMEPATH`)。
pub fn home_dir() -> Option<PathBuf> {
    home_dir_with(Os::current(), &real_env)
}

pub fn home_dir_with(os: Os, env: Env) -> Option<PathBuf> {
    match os {
        Os::Windows => env("USERPROFILE").map(PathBuf::from).or_else(|| {
            let mut path = env("HOMEDRIVE")?;
            path.push(env("HOMEPATH")?);
            Some(PathBuf::from(path))
        }),
        Os::MacOs | Os::Other => env("HOME").map(PathBuf::from),
    }
}

/// データベースを置くフォルダ。
/// macOS は `~/Library/Application Support/SyncLane`、Windows は `%LOCALAPPDATA%\SyncLane`、
/// それ以外は `$XDG_DATA_HOME/SyncLane`(無ければ `~/.local/share/SyncLane`)。
pub fn app_data_dir() -> Option<PathBuf> {
    app_data_dir_with(Os::current(), &real_env)
}

pub fn app_data_dir_with(os: Os, env: Env) -> Option<PathBuf> {
    let home = || home_dir_with(os, env);
    match os {
        Os::MacOs => Some(home()?.join("Library").join("Application Support").join("SyncLane")),
        Os::Windows => Some(env("LOCALAPPDATA").map(PathBuf::from).or_else(|| Some(home()?.join("AppData").join("Local")))?.join("SyncLane")),
        Os::Other => Some(env("XDG_DATA_HOME").map(PathBuf::from).or_else(|| Some(home()?.join(".local").join("share")))?.join("SyncLane")),
    }
}

/// Claude Code の会話(`~/.claude/projects`)。ホームが分からないときは、相対パスになる(これまでと同じ)。
pub fn claude_projects_dir() -> PathBuf {
    home_dir().unwrap_or_default().join(".claude").join("projects")
}

/// Codex の会話(`~/.codex/sessions`)。
pub fn codex_sessions_dir() -> PathBuf {
    home_dir().unwrap_or_default().join(".codex").join("sessions")
}

/// Gemini CLI の会話(`~/.gemini/tmp`)。
pub fn gemini_tmp_dir() -> PathBuf {
    home_dir().unwrap_or_default().join(".gemini").join("tmp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |key| map.get(key).map(OsString::from)
    }

    #[test]
    fn macos_keeps_the_location_it_always_had() {
        let env = env_of(&[("HOME", "/Users/tanaka")]);
        assert_eq!(home_dir_with(Os::MacOs, &env), Some(PathBuf::from("/Users/tanaka")));
        assert_eq!(
            app_data_dir_with(Os::MacOs, &env),
            Some(PathBuf::from("/Users/tanaka").join("Library").join("Application Support").join("SyncLane"))
        );
    }

    #[test]
    fn windows_uses_userprofile_and_localappdata() {
        let env = env_of(&[("USERPROFILE", r"C:\Users\tanaka"), ("LOCALAPPDATA", r"C:\Users\tanaka\AppData\Local"), ("HOME", "/ignored")]);
        assert_eq!(home_dir_with(Os::Windows, &env), Some(PathBuf::from(r"C:\Users\tanaka")));
        assert_eq!(app_data_dir_with(Os::Windows, &env), Some(PathBuf::from(r"C:\Users\tanaka\AppData\Local").join("SyncLane")));
    }

    #[test]
    fn windows_falls_back_to_homedrive_and_to_the_default_local_folder() {
        let env = env_of(&[("HOMEDRIVE", "C:"), ("HOMEPATH", r"\Users\tanaka")]);
        assert_eq!(home_dir_with(Os::Windows, &env), Some(PathBuf::from(r"C:\Users\tanaka")));
        assert_eq!(
            app_data_dir_with(Os::Windows, &env),
            Some(PathBuf::from(r"C:\Users\tanaka").join("AppData").join("Local").join("SyncLane"))
        );
        // HOMEDRIVE だけでは、決められない
        assert_eq!(home_dir_with(Os::Windows, &env_of(&[("HOMEDRIVE", "C:")])), None);
    }

    #[test]
    fn linux_follows_xdg_and_falls_back_to_the_home_folder() {
        let xdg = env_of(&[("HOME", "/home/tanaka"), ("XDG_DATA_HOME", "/data")]);
        assert_eq!(app_data_dir_with(Os::Other, &xdg), Some(PathBuf::from("/data").join("SyncLane")));
        let plain = env_of(&[("HOME", "/home/tanaka")]);
        assert_eq!(app_data_dir_with(Os::Other, &plain), Some(PathBuf::from("/home/tanaka").join(".local").join("share").join("SyncLane")));
    }

    #[test]
    fn nothing_is_decided_without_a_home_folder() {
        let none = env_of(&[]);
        for os in [Os::MacOs, Os::Windows, Os::Other] {
            assert_eq!(home_dir_with(os, &none), None);
            assert_eq!(app_data_dir_with(os, &none), None);
        }
    }

    #[test]
    fn this_machine_resolves_to_the_same_places_as_before() {
        // macOS の開発機では、これまでの固定の場所と同じになる
        if Os::current() == Os::MacOs {
            let home = PathBuf::from(std::env::var_os("HOME").unwrap());
            assert_eq!(app_data_dir(), Some(home.join("Library/Application Support/SyncLane")));
            assert_eq!(claude_projects_dir(), home.join(".claude/projects"));
            assert_eq!(codex_sessions_dir(), home.join(".codex/sessions"));
            assert_eq!(gemini_tmp_dir(), home.join(".gemini/tmp"));
        }
    }
}
