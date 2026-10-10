//! Desktop message alerts, independent of the voice playback stream.

/// Dispatch the alert off the UI thread. Call only for messages that warrant an alert.
pub fn notify(author: &str, content: &str) {
    let author = author.to_owned();
    let content = content.to_owned();
    if let Err(error) = std::thread::Builder::new()
        .name("message-notification".into())
        .spawn(move || deliver(&author, &content))
    {
        log::warn!("Could not start message notification: {error}");
    }
}

#[cfg(target_os = "linux")]
fn notification_command(author: &str, content: &str) -> std::process::Command {
    let mut command = std::process::Command::new("notify-send");
    // Notification bodies support markup; message text must remain literal.
    let body = if content.is_empty() {
        "Nova mensagem".to_owned()
    } else {
        content
            .chars()
            .take(300)
            .collect::<String>()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    command.args([
        "--app-name=FastDiscord",
        "--icon=mail-unread",
        "--hint=boolean:suppress-sound:true",
        "--",
        author,
        &body,
    ]);
    command
}

#[cfg(target_os = "linux")]
fn deliver(author: &str, content: &str) {
    let notification = notification_command(author, content).spawn();
    // shortcut: use the desktop's freedesktop sound theme; add a bundled tone
    // when packaging for desktops that do not ship this theme.
    match std::process::Command::new("paplay")
        .args([
            "--client-name=FastDiscord",
            "/usr/share/sounds/freedesktop/stereo/message-new-instant.oga",
        ])
        .status()
    {
        Ok(status) if status.success() => {}
        result => log::warn!("Message sound failed: {result:?}"),
    }
    match notification.and_then(|mut child| child.wait()) {
        Ok(status) if status.success() => {}
        result => log::warn!("Desktop notification failed: {result:?}"),
    }
}

#[cfg(windows)]
fn deliver(author: &str, content: &str) {
    // Environment values keep message text out of executable PowerShell code.
    let result = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; \
             $n = New-Object System.Windows.Forms.NotifyIcon; \
             $n.Icon = [System.Drawing.SystemIcons]::Information; \
             $n.Visible = $true; \
             [System.Media.SystemSounds]::Asterisk.Play(); \
             $n.ShowBalloonTip(5000, $env:FASTDISCORD_AUTHOR, $env:FASTDISCORD_BODY, \
             [System.Windows.Forms.ToolTipIcon]::Info); \
             Start-Sleep -Seconds 6; $n.Dispose()",
        ])
        .env("FASTDISCORD_AUTHOR", author)
        .env("FASTDISCORD_BODY", content)
        .status();
    match result {
        Ok(status) if status.success() => {}
        result => log::warn!("Desktop notification failed: {result:?}"),
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn deliver(_author: &str, _content: &str) {
    log::warn!("Desktop notifications are not supported on this platform");
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn untrusted_message_is_literal_and_cannot_become_an_option() {
        let command = notification_command("--urgency=critical", "<b>A & B</b> $(id)");
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            &args[3..],
            &[
                "--",
                "--urgency=critical",
                "&lt;b&gt;A &amp; B&lt;/b&gt; $(id)"
            ]
        );
        let command = notification_command("A", "");
        assert_eq!(command.get_args().last().unwrap(), "Nova mensagem");
        let command = notification_command("A", &"é".repeat(301));
        assert_eq!(
            command
                .get_args()
                .last()
                .unwrap()
                .to_str()
                .unwrap()
                .chars()
                .count(),
            300
        );
    }
}
