//! Strings in the app's languages (English and Simplified Chinese).
//!
//! One lookup, one table per language: a missing key falls back to English and
//! then to the key itself, so the UI never shows an empty label while the
//! tables are being filled in.

/// A UI language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    Zh,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Zh => "zh",
        }
    }

    /// The name shown in a language picker, written in that language.
    pub fn label(self) -> &'static str {
        match self {
            Language::En => "English",
            Language::Zh => "简体中文",
        }
    }

    pub const ALL: [Language; 2] = [Language::En, Language::Zh];
}

/// The table for `language`.
fn table(language: Language) -> &'static [(&'static str, &'static str)] {
    match language {
        Language::En => EN,
        Language::Zh => ZH,
    }
}

/// Whether `key` has a translation in `language` (no English fallback).
///
/// Prefer this to comparing [`text`] with the `"<missing>"` sentinel: callers
/// that branch on "this language has a singular form" (views' `count_text`)
/// should not depend on the fallback's spelling.
pub fn has_key(language: Language, key: &str) -> bool {
    table(language)
        .iter()
        .any(|(candidate, _)| *candidate == key)
}

/// The string for `key` in `language`.
///
/// Misses fall back to English and then to `"<missing>"`, so the UI never
/// renders an empty label. The tables are linear scans, measured at ~2.9 µs
/// per lookup in an unoptimized build; a
/// binding that re-runs at the progress rate calls this a few times per event,
/// which is far below a frame, so no map/index is warranted. The tests below
/// keep the tables duplicate-free and at EN/ZH parity, but a misspelt key in
/// code is not checked: it renders `"<missing>"`.
pub fn text(language: Language, key: &str) -> &'static str {
    table(language)
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .or_else(|| EN.iter().find(|(candidate, _)| *candidate == key))
        .map(|(_, value)| *value)
        .unwrap_or("<missing>")
}

/// Fills `{name}` placeholders in a translated template.
///
/// One implementation for the views and the macOS menu bar: the tables are
/// tested to use the same placeholders in every language, so both must
/// substitute them the same way.
pub fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (name, value) in values {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// English strings.
const EN: &[(&str, &str)] = &[
    // App
    ("app.name", "LocalSend"),
    // Navigation
    ("nav.send", "Send"),
    ("nav.receive", "Receive"),
    ("nav.history", "History"),
    ("nav.settings", "Settings"),
    ("nav.nearby", "Nearby devices"),
    // Send
    ("send.title", "Send"),
    ("send.subtitle", "Send files to a nearby device"),
    ("send.drop_title", "Drop files to send"),
    ("send.drop_hint", "or click to browse — files and folders"),
    ("send.pick", "Choose files"),
    ("send.pick_files", "Choose files"),
    ("send.pick_folder", "Choose folder"),
    ("send.add_text", "Send text"),
    ("send.paste_files", "Paste files"),
    ("send.clear", "Clear"),
    ("send.remove", "Remove"),
    ("send.send", "Send"),
    ("send.send_to", "Send to {name}"),
    ("send.no_peer", "Select a device to send to"),
    ("send.offline", "The local network is not ready"),
    ("send.nothing_added", "Nothing was added"),
    ("send.clipboard_empty", "The clipboard is empty"),
    ("send.choose_device", "Choose a device"),
    ("send.items_count", "{count} items"),
    ("send.item_one", "1 item"),
    ("send.total_size", "{size} in total"),
    ("send.text_preview", "Text message"),
    ("send.text_placeholder", "Type a message"),
    ("send.add", "Add"),
    // Devices
    ("device.online", "Available"),
    ("device.add_favorite", "Add to favorites"),
    ("device.remove_favorite", "Remove from favorites"),
    ("device.no_devices", "No nearby devices"),
    ("device.refresh", "Refresh nearby devices"),
    ("device.empty_hint", "Open LocalSend on the same network"),
    ("device.searching", "Looking for devices on your network…"),
    ("device.this_mac", "This device"),
    ("device.type.desktop", "Desktop"),
    ("device.type.mobile", "Mobile"),
    ("device.type.web", "Web"),
    ("device.type.headless", "Headless"),
    ("device.type.server", "Server"),
    ("device.type.unknown", "Unknown"),
    // Device verification: both devices compare these before relying on the
    // other one's identity (LocalSend's Verify page: icons or text).
    ("verify.action", "Verify"),
    ("verify.title", "Verify device"),
    ("verify.icons", "Icons"),
    ("verify.text", "Text"),
    (
        "verify.question",
        "Does it look the same on the other device?",
    ),
    (
        "verify.hint",
        "Built from both devices' certificate fingerprints; stop the transfer if they differ.",
    ),
    (
        "verify.unavailable",
        "The network is not running yet, so this device has no fingerprint.",
    ),
    // Receive
    ("receive.discoverable", "Discoverable"),
    ("receive.pin", "Receive PIN"),
    (
        "receive.pin_hint",
        "Devices must enter this PIN before sending",
    ),
    ("receive.pin_unset", "No PIN"),
    ("receive.downloads", "Save to"),
    ("receive.change", "Change"),
    ("receive.incoming", "Incoming transfer"),
    ("receive.accept", "Accept"),
    ("receive.decline", "Decline"),
    ("receive.from", "From {name}"),
    ("receive.more", "{count} more files"),
    ("receive.more_one", "1 more file"),
    ("receive.quick_save", "Accept automatically"),
    (
        "receive.quick_save_hint",
        "Save incoming files without asking",
    ),
    ("receive.quick_save_favorites", "Only from favorites"),
    (
        "receive.quick_save_favorites_hint",
        "Other devices still have to be accepted by hand",
    ),
    ("receive.waiting", "Waiting for incoming transfers"),
    (
        "receive.waiting_hint",
        "Keep this window open; devices on your network will appear here",
    ),
    // Transfers
    ("transfer.sending", "Sending"),
    ("transfer.cancel", "Cancel transfer"),
    ("transfer.completed", "Completed"),
    ("transfer.completed_to", "Saved to {dir}"),
    ("transfer.cancelled", "Cancelled"),
    ("transfer.failed", "Failed"),
    ("transfer.open", "Open"),
    ("transfer.reveal", "Show in folder"),
    ("transfer.copy_text", "Copy text"),
    ("transfer.files", "{count} files"),
    ("transfer.file_one", "1 file"),
    ("transfer.total", "{size}"),
    ("transfer.active", "Transfers"),
    ("transfer.details", "Transfer details"),
    // History
    ("history.title", "History"),
    ("history.empty", "No transfers yet"),
    (
        "history.empty_hint",
        "Files you send and receive will show up here",
    ),
    ("history.clear", "Clear history"),
    ("history.clear_confirm", "Clear all history?"),
    (
        "history.clear_confirm_hint",
        "This cannot be undone. Files on disk are not deleted.",
    ),
    ("history.today", "Today"),
    ("history.yesterday", "Yesterday"),
    // Status
    ("status.transferring", "Transferring"),
    ("status.completed", "Completed"),
    ("status.cancelled", "Cancelled"),
    ("status.failed", "Failed"),
    // Settings
    ("settings.title", "Settings"),
    ("settings.general", "General"),
    ("settings.name", "Device name"),
    ("settings.device_type", "Device type"),
    ("settings.receiving", "Receiving"),
    ("settings.appearance", "Appearance"),
    ("settings.language", "Language"),
    ("settings.about", "About"),
    ("settings.version", "Version {version}"),
    ("settings.port", "Port"),
    ("settings.https", "Encrypt transfers"),
    (
        "settings.https_hint",
        "Uses HTTPS certificates. Turn off for older devices.",
    ),
    ("settings.discoverable", "Allow discovery"),
    (
        "settings.discoverable_hint",
        "Let other devices find this one on the network",
    ),
    (
        "settings.run_background",
        "Keep running when the window closes",
    ),
    (
        "settings.run_background_hint",
        "Transfers continue in the background",
    ),
    ("settings.applies_after_transfer", "After transfer"),
    ("settings.open_downloads", "Open download folder"),
    ("settings.saved", "Settings saved"),
    ("settings.github", "GitHub"),
    ("settings.protocol", "Protocol v{version}"),
    ("settings.auto_update", "Update automatically"),
    (
        "settings.auto_update_hint",
        "Checks GitHub daily and downloads new versions",
    ),
    ("settings.update", "Software update"),
    ("update.check", "Check for updates"),
    ("update.checking", "Checking for updates…"),
    ("update.up_to_date", "LocalSend is up to date"),
    ("update.downloading", "Downloading {version}… {percent}%"),
    ("update.downloading_button", "Downloading…"),
    (
        "update.ready",
        "Version {version} is downloaded and verified",
    ),
    (
        "update.ready_toast",
        "LocalSend {version} is ready: restart to update",
    ),
    ("update.restart", "Restart to update"),
    ("update.installing", "Installing the update…"),
    ("update.available", "Version {version} is available"),
    (
        "update.available_manual",
        "LocalSend {version} is available: download it from GitHub",
    ),
    ("update.download_page", "Download"),
    (
        "update.manual_unsupported",
        "Install it from the release page",
    ),
    (
        "update.manual_portable",
        "The portable version updates by hand",
    ),
    (
        "update.manual_translocated",
        "Move LocalSend to Applications to update it in place",
    ),
    (
        "update.manual_read_only",
        "LocalSend's folder is read-only for this account",
    ),
    (
        "update.failed_network",
        "Cannot reach GitHub. Check the connection and try again",
    ),
    (
        "update.failed_verification",
        "The update failed verification and was discarded",
    ),
    ("update.failed_install", "Cannot install the update"),
    (
        "update.wait_for_transfers",
        "Update after the current transfer finishes",
    ),
    // PIN
    ("pin.title", "Enter PIN"),
    (
        "pin.hint",
        "This device needs a PIN before it accepts files",
    ),
    ("pin.placeholder", "PIN"),
    ("pin.confirm", "Send"),
    ("pin.cancel", "Cancel"),
    // Appearance
    ("appearance.system", "System"),
    ("appearance.light", "Light"),
    ("appearance.dark", "Dark"),
    // Common
    ("common.cancel", "Cancel"),
    ("common.close", "Close"),
    ("common.copied", "Copied"),
    ("common.dismiss", "Dismiss"),
    // Menus (macOS wording; Windows and Linux use the same labels)
    ("menu.file", "File"),
    ("menu.edit", "Edit"),
    ("menu.view", "View"),
    ("menu.window", "Window"),
    ("menu.help", "Help"),
    ("menu.about", "About LocalSend"),
    ("menu.settings", "Settings…"),
    ("menu.hide", "Hide LocalSend"),
    ("menu.hide_others", "Hide Others"),
    ("menu.show_all", "Show All"),
    ("menu.quit", "Quit LocalSend"),
    ("menu.choose_files", "Choose Files…"),
    ("menu.choose_folder", "Choose Folder…"),
    ("menu.close_window", "Close Window"),
    ("menu.undo", "Undo"),
    ("menu.redo", "Redo"),
    ("menu.cut", "Cut"),
    ("menu.copy", "Copy"),
    ("menu.paste", "Paste"),
    ("menu.select_all", "Select All"),
    ("menu.restart_network", "Restart Network"),
    ("menu.toggle_full_screen", "Toggle Full Screen"),
    ("menu.minimize", "Minimize"),
    ("menu.zoom", "Zoom"),
    ("menu.bring_all_to_front", "Bring All to Front"),
    ("menu.website", "LocalSend Website"),
    ("menu.source", "LocalSend on GitHub"),
    // Notifications
    ("notify.incoming_title", "Incoming transfer"),
    ("notify.incoming_body", "{name} wants to send you files"),
    ("notify.finished_title", "Transfer complete"),
    ("notify.finished_body", "The transfer with {name} finished"),
    ("notify.failed_title", "Transfer failed"),
    ("notify.failed_body", "The transfer with {name} failed"),
    // Toasts
    (
        "toast.settings_deferred",
        "Settings will apply after the current transfer",
    ),
    // Menu bar (status item) menu
    ("tray.devices", "{count} nearby devices"),
    ("tray.devices_one", "1 nearby device"),
    ("tray.devices_none", "No devices nearby"),
    ("tray.ready", "Ready to receive"),
    ("tray.hidden", "Ready · hidden from other devices"),
    ("tray.offline", "Not connected to the local network"),
    ("tray.request", "{name} wants to send you files"),
    ("tray.to", "To {name} — {status}"),
    ("tray.from", "From {name} — {status}"),
    ("tray.more", "{count} more transfers"),
    ("tray.recent", "Recent transfers"),
    ("tray.recent_empty", "No recent transfers"),
    ("tray.open", "Open LocalSend"),
    ("tray.quit", "Quit"),
    // Quit confirmation (an active transfer must not be interrupted silently)
    ("quit.active_title", "Quit LocalSend?"),
    (
        "quit.active_body",
        "A transfer is still in progress. Quitting will cancel it.",
    ),
    ("quit.confirm", "Quit Anyway"),
    // Errors
    (
        "error.start_failed",
        "Could not start the local network service",
    ),
    ("error.open_failed", "Could not open the file"),
    (
        "error.open_risky",
        "This file type can run programs, so it is shown in its folder instead",
    ),
    (
        "error.request_invalid",
        "Declined a transfer request that LocalSend cannot accept",
    ),
    (
        "error.request_no_space",
        "Declined a transfer: there is not enough free space in the receive folder",
    ),
    (
        "error.request_unencrypted",
        "Declined a transfer that did not use an encrypted connection",
    ),
    (
        "error.too_many_files",
        "Send at most {count} files at a time",
    ),
    (
        "error.pin_too_short",
        "Use a PIN of at least {count} characters",
    ),
    (
        "error.reveal_failed",
        "Could not show the file in its folder",
    ),
    ("error.choose_failed", "Could not show the file dialog"),
    ("error.peer_offline", "That device is no longer available"),
    (
        "error.network_lost",
        "The local network connection was lost",
    ),
    ("error.send_failed", "Could not start the transfer"),
    ("error.pin_failed", "Could not send the PIN"),
    ("error.decide_failed", "Could not answer the request"),
    ("error.cancel_failed", "Could not cancel the transfer"),
    ("error.settings_not_saved", "Could not save the settings"),
    (
        "error.no_writable",
        "Could not write to the download folder",
    ),
];

/// Simplified Chinese strings.
const ZH: &[(&str, &str)] = &[
    ("app.name", "LocalSend"),
    ("nav.send", "发送"),
    ("nav.receive", "接收"),
    ("nav.history", "历史"),
    ("nav.settings", "设置"),
    ("nav.nearby", "附近设备"),
    ("send.title", "发送"),
    ("send.subtitle", "把文件发送到附近的设备"),
    ("send.drop_title", "拖放文件到此处"),
    ("send.drop_hint", "或点击选择文件与文件夹"),
    ("send.pick", "选择文件"),
    ("send.pick_files", "选择文件"),
    ("send.pick_folder", "选择文件夹"),
    ("send.add_text", "发送文本"),
    ("send.paste_files", "粘贴文件"),
    ("send.clear", "清空"),
    ("send.remove", "移除"),
    ("send.send", "发送"),
    ("send.send_to", "发送到 {name}"),
    ("send.no_peer", "请先选择接收设备"),
    ("send.offline", "局域网未就绪"),
    ("send.nothing_added", "没有添加任何内容"),
    ("send.clipboard_empty", "剪贴板为空"),
    ("send.choose_device", "选择设备"),
    ("send.items_count", "{count} 项"),
    ("send.item_one", "1 项"),
    ("send.total_size", "共 {size}"),
    ("send.text_preview", "文本消息"),
    ("send.text_placeholder", "输入要发送的文本"),
    ("send.add", "添加"),
    ("device.online", "可连接"),
    ("device.add_favorite", "添加到收藏"),
    ("device.remove_favorite", "取消收藏"),
    ("device.no_devices", "暂无附近设备"),
    ("device.refresh", "刷新附近设备"),
    ("device.empty_hint", "请在同一网络下打开 LocalSend"),
    ("device.searching", "正在局域网中查找设备…"),
    ("device.this_mac", "本机"),
    ("device.type.desktop", "桌面"),
    ("device.type.mobile", "手机"),
    ("device.type.web", "网页"),
    ("device.type.headless", "无界面"),
    ("device.type.server", "服务器"),
    ("device.type.unknown", "未知"),
    // 设备验证：确认对方身份前，两边比对同一组图标或文字。
    ("verify.action", "验证"),
    ("verify.title", "验证设备"),
    ("verify.icons", "图标"),
    ("verify.text", "文本"),
    ("verify.question", "在另一台设备上显示的内容相同吗？"),
    (
        "verify.hint",
        "由两台设备的证书指纹生成；不一致时请停止传输。",
    ),
    (
        "verify.unavailable",
        "局域网服务尚未运行，暂时无法生成验证信息。",
    ),
    ("receive.discoverable", "允许被发现"),
    ("receive.pin", "接收 PIN"),
    ("receive.pin_hint", "对方发送前需要输入此 PIN"),
    ("receive.pin_unset", "未设置"),
    ("receive.downloads", "保存到"),
    ("receive.change", "更改"),
    ("receive.incoming", "收到传输请求"),
    ("receive.accept", "接收"),
    ("receive.decline", "拒绝"),
    ("receive.from", "来自 {name}"),
    ("receive.more", "还有 {count} 个文件"),
    ("receive.more_one", "还有 1 个文件"),
    ("receive.quick_save", "自动接收"),
    ("receive.quick_save_hint", "收到文件时不再询问，直接保存"),
    ("receive.quick_save_favorites", "仅限收藏的设备"),
    (
        "receive.quick_save_favorites_hint",
        "其他设备发来的文件仍需手动接收",
    ),
    ("receive.waiting", "等待接收"),
    (
        "receive.waiting_hint",
        "保持窗口打开，局域网中的设备会出现在这里",
    ),
    ("transfer.sending", "正在发送"),
    ("transfer.cancel", "取消传输"),
    ("transfer.completed", "已完成"),
    ("transfer.completed_to", "已保存到 {dir}"),
    ("transfer.cancelled", "已取消"),
    ("transfer.failed", "失败"),
    ("transfer.open", "打开"),
    ("transfer.reveal", "在文件夹中显示"),
    ("transfer.copy_text", "复制文本"),
    ("transfer.files", "{count} 个文件"),
    ("transfer.file_one", "1 个文件"),
    ("transfer.total", "{size}"),
    ("transfer.active", "传输中"),
    ("transfer.details", "传输详情"),
    ("history.title", "历史"),
    ("history.empty", "还没有传输记录"),
    ("history.empty_hint", "发送或接收的文件会显示在这里"),
    ("history.clear", "清除历史"),
    ("history.clear_confirm", "清除全部历史记录？"),
    (
        "history.clear_confirm_hint",
        "此操作无法撤销，磁盘上的文件不会被删除。",
    ),
    ("history.today", "今天"),
    ("history.yesterday", "昨天"),
    ("status.transferring", "传输中"),
    ("status.completed", "已完成"),
    ("status.cancelled", "已取消"),
    ("status.failed", "失败"),
    ("settings.title", "设置"),
    ("settings.general", "通用"),
    ("settings.name", "设备名称"),
    ("settings.device_type", "设备类型"),
    ("settings.receiving", "接收"),
    ("settings.appearance", "外观"),
    ("settings.language", "语言"),
    ("settings.about", "关于"),
    ("settings.version", "版本 {version}"),
    ("settings.port", "端口"),
    ("settings.https", "加密传输"),
    (
        "settings.https_hint",
        "使用 HTTPS 证书；关闭后兼容旧版本设备",
    ),
    ("settings.discoverable", "允许被发现"),
    (
        "settings.discoverable_hint",
        "让局域网中的其他设备找到这台设备",
    ),
    ("settings.run_background", "关闭窗口后继续运行"),
    ("settings.run_background_hint", "传输会在后台继续"),
    ("settings.applies_after_transfer", "当前传输结束后生效"),
    ("settings.open_downloads", "打开下载文件夹"),
    ("settings.saved", "设置已保存"),
    ("settings.github", "GitHub"),
    ("settings.protocol", "协议 v{version}"),
    ("settings.auto_update", "自动更新"),
    (
        "settings.auto_update_hint",
        "每天检查 GitHub，自动下载新版本",
    ),
    ("settings.update", "软件更新"),
    ("update.check", "检查更新"),
    ("update.checking", "正在检查更新…"),
    ("update.up_to_date", "LocalSend 已是最新版本"),
    ("update.downloading", "正在下载 {version}… {percent}%"),
    ("update.downloading_button", "下载中…"),
    ("update.ready", "{version} 已下载并通过校验"),
    (
        "update.ready_toast",
        "LocalSend {version} 已就绪，重启即可更新",
    ),
    ("update.restart", "重启以更新"),
    ("update.installing", "正在安装更新…"),
    ("update.available", "发现新版本 {version}"),
    (
        "update.available_manual",
        "LocalSend {version} 已发布，请从 GitHub 下载",
    ),
    ("update.download_page", "前往下载"),
    ("update.manual_unsupported", "请从发布页面下载安装"),
    ("update.manual_portable", "便携版需要手动下载更新"),
    (
        "update.manual_translocated",
        "把 LocalSend 移到「应用程序」文件夹后才能自动更新",
    ),
    (
        "update.manual_read_only",
        "当前账户没有 LocalSend 所在文件夹的写入权限",
    ),
    ("update.failed_network", "无法连接 GitHub，请检查网络后重试"),
    ("update.failed_verification", "更新文件校验失败，已丢弃"),
    ("update.failed_install", "无法安装更新"),
    ("update.wait_for_transfers", "请在当前传输结束后再更新"),
    ("pin.title", "输入 PIN"),
    ("pin.hint", "对方设备需要 PIN 才能接收文件"),
    ("pin.placeholder", "PIN"),
    ("pin.confirm", "发送"),
    ("pin.cancel", "取消"),
    ("appearance.system", "跟随系统"),
    ("appearance.light", "浅色"),
    ("appearance.dark", "深色"),
    ("common.cancel", "取消"),
    ("common.close", "关闭"),
    ("common.copied", "已复制"),
    ("common.dismiss", "忽略"),
    // Menus
    ("menu.file", "文件"),
    ("menu.edit", "编辑"),
    ("menu.view", "显示"),
    ("menu.window", "窗口"),
    ("menu.help", "帮助"),
    ("menu.about", "关于 LocalSend"),
    ("menu.settings", "设置…"),
    ("menu.hide", "隐藏 LocalSend"),
    ("menu.hide_others", "隐藏其他"),
    ("menu.show_all", "全部显示"),
    ("menu.quit", "退出 LocalSend"),
    ("menu.choose_files", "选取文件…"),
    ("menu.choose_folder", "选取文件夹…"),
    ("menu.close_window", "关闭窗口"),
    ("menu.undo", "撤销"),
    ("menu.redo", "重做"),
    ("menu.cut", "剪切"),
    ("menu.copy", "复制"),
    ("menu.paste", "粘贴"),
    ("menu.select_all", "全选"),
    ("menu.restart_network", "重新启动网络"),
    ("menu.toggle_full_screen", "切换全屏"),
    ("menu.minimize", "最小化"),
    ("menu.zoom", "缩放"),
    ("menu.bring_all_to_front", "全部置于顶层"),
    ("menu.website", "LocalSend 网站"),
    ("menu.source", "GitHub 上的 LocalSend"),
    // Notifications
    ("notify.incoming_title", "收到传输请求"),
    ("notify.incoming_body", "{name} 想向你发送文件"),
    ("notify.finished_title", "传输完成"),
    ("notify.finished_body", "与 {name} 的传输已完成"),
    ("notify.failed_title", "传输失败"),
    ("notify.failed_body", "与 {name} 的传输失败"),
    // Toasts
    ("toast.settings_deferred", "设置将在当前传输结束后生效"),
    // Menu bar (status item) menu
    ("tray.devices", "{count} 台附近设备"),
    ("tray.devices_one", "1 台附近设备"),
    ("tray.devices_none", "附近暂无设备"),
    ("tray.ready", "就绪 · 可以接收文件"),
    ("tray.hidden", "就绪 · 其他设备看不到本机"),
    ("tray.offline", "局域网服务未运行"),
    ("tray.request", "{name} 想给你发送文件"),
    ("tray.to", "发送到 {name} — {status}"),
    ("tray.from", "来自 {name} — {status}"),
    ("tray.more", "还有 {count} 个传输"),
    ("tray.recent", "最近传输"),
    ("tray.recent_empty", "暂无最近传输"),
    ("tray.open", "打开 LocalSend"),
    ("tray.quit", "退出"),
    // Quit confirmation
    ("quit.active_title", "退出 LocalSend？"),
    ("quit.active_body", "仍有传输进行中。退出会取消该传输。"),
    ("quit.confirm", "仍然退出"),
    // Errors
    ("error.start_failed", "无法启动局域网服务"),
    ("error.open_failed", "无法打开文件"),
    (
        "error.open_risky",
        "这类文件可能会运行程序，已改为在文件夹中显示",
    ),
    ("error.request_invalid", "已拒绝一个无法接收的传输请求"),
    (
        "error.request_no_space",
        "已拒绝传输：接收文件夹所在磁盘空间不足",
    ),
    ("error.request_unencrypted", "已拒绝一个未加密的传输请求"),
    ("error.too_many_files", "一次最多发送 {count} 个文件"),
    ("error.pin_too_short", "PIN 至少需要 {count} 个字符"),
    ("error.reveal_failed", "无法在文件夹中显示文件"),
    ("error.choose_failed", "无法显示文件选择器"),
    ("error.peer_offline", "该设备已离线"),
    ("error.network_lost", "局域网连接已断开"),
    ("error.send_failed", "无法开始传输"),
    ("error.pin_failed", "无法发送 PIN"),
    ("error.decide_failed", "无法回应请求"),
    ("error.cancel_failed", "无法取消传输"),
    ("error.settings_not_saved", "无法保存设置"),
    ("error.no_writable", "无法写入下载文件夹"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The keys of one table.
    fn keys(table: &'static [(&'static str, &'static str)]) -> HashSet<&'static str> {
        table.iter().map(|(key, _)| *key).collect()
    }

    #[test]
    fn tables_have_no_duplicate_keys() {
        for (name, table) in [("EN", EN), ("ZH", ZH)] {
            let mut seen = HashSet::new();
            for (key, _) in table {
                assert!(seen.insert(*key), "{name} has the key {key:?} twice");
            }
        }
    }

    #[test]
    fn english_and_chinese_have_the_same_keys() {
        let en = keys(EN);
        let zh = keys(ZH);
        let missing_zh: Vec<_> = en.difference(&zh).collect();
        let missing_en: Vec<_> = zh.difference(&en).collect();
        assert!(
            missing_zh.is_empty() && missing_en.is_empty(),
            "missing in ZH: {missing_zh:?}; missing in EN: {missing_en:?}"
        );
    }

    #[test]
    fn every_english_key_resolves_in_both_languages() {
        for language in Language::ALL {
            for (key, _) in EN {
                assert_ne!(
                    text(language, key),
                    "<missing>",
                    "{key} is missing in {}",
                    language.code()
                );
            }
        }
    }

    #[test]
    fn has_key_reports_the_missing_sentinel() {
        assert!(has_key(Language::En, "app.name"));
        assert!(has_key(Language::Zh, "app.name"));
        assert!(!has_key(Language::En, "no.such.key"));
        assert!(!has_key(Language::Zh, "no.such.key"));
        // Unknown keys still render *something*.
        assert_eq!(text(Language::Zh, "no.such.key"), "<missing>");
    }

    /// `fill()` replaces `{placeholders}` literally: a translation that drops
    /// or renames one would leak `{name}` into the UI. EN and ZH must use the
    /// same placeholder set for every key.
    #[test]
    fn translations_use_the_same_placeholders() {
        for (key, english) in EN {
            let chinese = text(Language::Zh, key);
            let placeholders = |value: &str| {
                let mut names: Vec<String> = value
                    .split('{')
                    .skip(1)
                    .filter_map(|rest| rest.split('}').next())
                    .map(str::to_string)
                    .collect();
                names.sort_unstable();
                names
            };
            assert_eq!(
                placeholders(english),
                placeholders(chinese),
                "{key} has different {{{{placeholders}}}} in EN and ZH"
            );
        }
    }
}
