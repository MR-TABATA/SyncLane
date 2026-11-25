use notify::event::EventKind;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

/// 監視対象のツール種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    Claude,
    Antigravity,
    Codex,
    Copilot,
    Gemini,
}

/// ウィンドウ Close 要求時に、閉じるのを防いで隠す（hide）べきかを判定する。
/// メインウィンドウかつ終了処理中でない場合のみ true。
pub fn should_hide_on_close(window_label: &str, is_quitting: bool) -> bool {
    window_label == "main" && !is_quitting
}

/// ファイルイベントの種類が、会話ログの更新として扱うべきものか判定する。
pub fn is_relevant_event_kind(kind: &EventKind) -> bool {
    kind.is_modify() || kind.is_create() || kind.is_remove()
}

/// 更新されたファイルパスから、対象の会話ツールを判定する。
/// 会話ログに関係ない一時ファイルや拡張子の場合は None を返す。
pub fn detect_tool_from_path(path: &Path) -> Option<ToolKind> {
    let path_str = path.to_string_lossy();

    // 一時ファイルやシステムファイルは除外
    if let Some(file_name) = path.file_name().and_then(|s| s.to_str()) {
        if file_name.starts_with('.') || file_name.ends_with(".tmp") || file_name == ".DS_Store" {
            return None;
        }
    }

    if path_str.contains(".gemini/antigravity-cli/conversations") {
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext == "db" || ext == "db-wal" || ext == "db-shm" {
            return Some(ToolKind::Antigravity);
        }
        return None;
    }

    if path_str.contains(".claude/projects") {
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext == "json" || ext == "md" || ext == "txt" {
            return Some(ToolKind::Claude);
        }
        return None;
    }

    if path_str.contains(".codex/sessions") {
        return Some(ToolKind::Codex);
    }

    if path_str.contains(".copilot/sessions") {
        return Some(ToolKind::Copilot);
    }

    if path_str.contains(".gemini/tmp") {
        return Some(ToolKind::Gemini);
    }

    None
}

/// 待機結果
#[derive(Debug, PartialEq, Eq)]
pub enum WaitResult<T> {
    /// デバウンスされたイベント群を受信した
    Event(Vec<T>),
    /// タイムアウトした（定期収集）
    Timeout,
    /// チャンネルが切断された
    Disconnected,
}

/// チャンネルからイベントを待機し、届いた場合はデバウンス待機を行って後続のイベントをまとめて返す。
/// タイムアウト時は WaitResult::Timeout を返す。切断時は WaitResult::Disconnected を返す。
pub fn wait_with_debounce<T>(
    rx: &Receiver<T>,
    timeout: Duration,
    debounce: Duration,
) -> WaitResult<T> {
    match rx.recv_timeout(timeout) {
        Ok(first) => {
            std::thread::sleep(debounce);
            let mut items = vec![first];
            while let Ok(item) = rx.try_recv() {
                items.push(item);
            }
            WaitResult::Event(items)
        }
        Err(RecvTimeoutError::Timeout) => WaitResult::Timeout,
        Err(RecvTimeoutError::Disconnected) => WaitResult::Disconnected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn test_should_hide_on_close_main_window() {
        assert!(should_hide_on_close("main", false));
        // 終了処理中なら隠さずに閉じる
        assert!(!should_hide_on_close("main", true));
    }

    #[test]
    fn test_should_close_search_window() {
        // 検索ウィンドウは隠さず閉じる
        assert!(!should_hide_on_close("search", false));
        assert!(!should_hide_on_close("search", true));
        assert!(!should_hide_on_close("other", false));
    }

    #[test]
    fn test_is_relevant_event_kind() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        assert!(is_relevant_event_kind(&EventKind::Create(CreateKind::File)));
        assert!(is_relevant_event_kind(&EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content))));
        assert!(is_relevant_event_kind(&EventKind::Remove(RemoveKind::File)));
        assert!(!is_relevant_event_kind(&EventKind::Access(notify::event::AccessKind::Read)));
        assert!(!is_relevant_event_kind(&EventKind::Other));
    }

    #[test]
    fn test_detect_tool_from_path() {
        // Antigravity
        assert_eq!(
            detect_tool_from_path(Path::new("/Users/test/.gemini/antigravity-cli/conversations/1234.db")),
            Some(ToolKind::Antigravity)
        );
        assert_eq!(
            detect_tool_from_path(Path::new("/Users/test/.gemini/antigravity-cli/conversations/1234.db-wal")),
            Some(ToolKind::Antigravity)
        );
        assert_eq!(
            detect_tool_from_path(Path::new("/Users/test/.gemini/antigravity-cli/conversations/other.txt")),
            None
        );

        // Claude
        assert_eq!(
            detect_tool_from_path(Path::new("/Users/test/.claude/projects/proj1/conversations.json")),
            Some(ToolKind::Claude)
        );
        assert_eq!(
            detect_tool_from_path(Path::new("/Users/test/.claude/projects/proj1/.DS_Store")),
            None
        );

        // Codex, Copilot, Gemini
        assert_eq!(
            detect_tool_from_path(Path::new("/Users/test/.codex/sessions/session.jsonl")),
            Some(ToolKind::Codex)
        );
        assert_eq!(
            detect_tool_from_path(Path::new("/Users/test/.copilot/sessions/session.jsonl")),
            Some(ToolKind::Copilot)
        );
        assert_eq!(
            detect_tool_from_path(Path::new("/Users/test/.gemini/tmp/chat.json")),
            Some(ToolKind::Gemini)
        );

        // 無関係なパス
        assert_eq!(detect_tool_from_path(Path::new("/Users/test/Documents/memo.txt")), None);
    }

    #[test]
    fn test_wait_with_debounce_coalesces_events() {
        let (tx, rx) = channel();

        // 連続で 5 件送信
        for i in 0..5 {
            tx.send(i).unwrap();
        }

        let res = wait_with_debounce(&rx, Duration::from_millis(50), Duration::from_millis(20));
        match res {
            WaitResult::Event(items) => {
                assert_eq!(items, vec![0, 1, 2, 3, 4]);
            }
            _ => panic!("expected Event with items"),
        }
    }

    #[test]
    fn test_wait_with_debounce_timeout() {
        let (_tx, rx) = channel::<i32>();
        let res = wait_with_debounce(&rx, Duration::from_millis(20), Duration::from_millis(10));
        assert_eq!(res, WaitResult::Timeout);
    }

    #[test]
    fn test_wait_with_debounce_disconnected() {
        let (tx, rx) = channel::<i32>();
        drop(tx);
        let res = wait_with_debounce(&rx, Duration::from_millis(20), Duration::from_millis(10));
        assert_eq!(res, WaitResult::Disconnected);
    }
}
