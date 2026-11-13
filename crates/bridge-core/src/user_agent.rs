//! Bridge product and platform identity, shared by HTTP and WebSocket.
use std::sync::OnceLock;

pub(crate) fn get() -> String {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let version = macos_version();
        #[cfg(not(target_os = "macos"))]
        let version: Option<String> = None;
        format_agent(
            std::env::consts::OS,
            std::env::consts::ARCH,
            version.as_deref(),
        )
    })
    .clone()
}

#[cfg(target_os = "macos")]
fn macos_version() -> Option<String> {
    let output = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_macos_version(&output.stdout).map(str::to_owned)
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos_version(output: &[u8]) -> Option<&str> {
    let version = std::str::from_utf8(output).ok()?.trim();
    ((2..=3).contains(&version.split('.').count())
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())))
    .then_some(version)
}

fn format_agent(os: &str, arch: &str, macos_version: Option<&str>) -> String {
    // The generic UA parser needs "Mac OS X <version>" for our own product name.
    // Linux/Windows and architecture fields retain the Desktop templates.
    let platform = match os {
        "macos" => macos_version
            .map(|version| format!("Mac OS X {version}"))
            .unwrap_or_else(|| "Mac OS X".into()),
        "linux" => "X11; Linux".into(),
        "windows" => "Windows NT 10.0".into(),
        other => other.to_owned(),
    };
    let arch = match arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        "powerpc64" => "ppc64",
        "loongarch64" => "loong64",
        other => other,
    };
    format!(
        "codex-rc-bridge/{} ({platform}; {arch})",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_user_agent_preserves_platform_and_architecture_templates() {
        // Linux/Windows retain Desktop fields; macOS uses the generic parser's template.
        for (os, arch, expected) in [
            ("macos", "aarch64", "Mac OS X 27.0.1; arm64"),
            ("macos", "x86_64", "Mac OS X 27.0.1; x64"),
            ("linux", "aarch64", "X11; Linux; arm64"),
            ("linux", "x86_64", "X11; Linux; x64"),
            ("windows", "aarch64", "Windows NT 10.0; arm64"),
            ("windows", "x86_64", "Windows NT 10.0; x64"),
            ("windows", "x86", "Windows NT 10.0; ia32"),
            ("freebsd", "x86_64", "freebsd; x64"),
        ] {
            let ua = format_agent(os, arch, Some("27.0.1"));
            assert_eq!(
                ua,
                format!("codex-rc-bridge/{} ({expected})", env!("CARGO_PKG_VERSION"))
            );
            assert!(reqwest::header::HeaderValue::from_str(&ua).is_ok());
        }
    }

    #[test]
    fn macos_version_is_parsed_without_inventing_a_fallback_version() {
        assert_eq!(parse_macos_version(b"27.0.1\n"), Some("27.0.1"));
        assert_eq!(parse_macos_version(b"10.15.7\n"), Some("10.15.7"));
        for output in [b"".as_slice(), b"unknown", b"27..1", b"27.0.1\r\nOther: x"] {
            assert_eq!(parse_macos_version(output), None);
        }
        assert_eq!(
            format_agent("macos", "aarch64", None),
            format!(
                "codex-rc-bridge/{} (Mac OS X; arm64)",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}
