use std::path::Path;
use std::time::Duration;

#[derive(PartialEq)]
pub enum UserChoice {
    Accept,
    Reject,
}

pub async fn prompt_user(from: &Path, to: &Path, timeout: Duration) -> UserChoice {
    let from_s = from.display().to_string();
    let to_s = to.display().to_string();

    #[cfg(target_os = "macos")]
    {
        // osascript dialog — runs synchronously, offload to blocking thread
        let result = tokio::task::spawn_blocking(move || {
            let script = format!(
                r#"display dialog "Convert file?\n{from_s} → {to_s}" buttons {{"Skip", "Convert"}} default button "Convert" with title "PowerEXT" giving up after {secs}"#,
                secs = timeout.as_secs()
            );
            std::process::Command::new("osascript")
                .args(["-e", &script])
                .output()
        })
        .await;

        return match result {
            Ok(Ok(out)) if String::from_utf8_lossy(&out.stdout).contains("Convert") => UserChoice::Accept,
            _ => UserChoice::Reject,
        };
    }

    #[cfg(target_os = "linux")]
    {
        use notify_rust::Notification;

        let body = format!("{from_s} → {to_s}");
        let result = tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel::<bool>();
            let handle = Notification::new()
                .summary("PowerEXT: convert file?")
                .body(&body)
                .action("accept", "Convert")
                .action("reject", "Skip")
                .show();
            match handle {
                Ok(h) => {
                    h.wait_for_action(|a| { let _ = tx.send(a == "accept"); });
                    rx.recv_timeout(timeout).unwrap_or(false)
                }
                Err(_) => false,
            }
        })
        .await;

        return match result {
            Ok(true) => UserChoice::Accept,
            _ => UserChoice::Reject,
        };
    }

    #[cfg(target_os = "windows")]
    {
        let body = format!("{from_s} -> {to_s}");
        let secs = timeout.as_secs();
        let result = tokio::task::spawn_blocking(move || {
            let script = format!(
                r#"Add-Type -AssemblyName PresentationFramework; $r = [System.Windows.MessageBox]::Show('Convert file?`n{body}', 'PowerEXT', 'YesNo', 'Question'); if ($r -eq 'Yes') {{ exit 0 }} else {{ exit 1 }}"#,
            );
            std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &script])
                .output()
        })
        .await;

        return match result {
            Ok(Ok(out)) if out.status.success() => UserChoice::Accept,
            _ => UserChoice::Reject,
        };
    }

    #[allow(unreachable_code)]
    UserChoice::Reject
}

pub fn notify_info(summary: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(r#"display notification "{body}" with title "{summary}""#);
        let _ = std::process::Command::new("osascript").args(["-e", &script]).status();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = notify_rust::Notification::new().summary(summary).body(body).show();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = winrt_notification::Toast::new(winrt_notification::Toast::POWERSHELL_APP_ID)
            .title(summary)
            .text1(body)
            .show();
    }
}
