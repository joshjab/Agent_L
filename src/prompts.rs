use std::path::PathBuf;

/// Resolve the prompts directory: `$AGENT_L_PROMPTS_DIR` if set, else `prompts/`.
fn prompts_dir() -> PathBuf {
    std::env::var("AGENT_L_PROMPTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("prompts"))
}

/// Strip HTML comments (`<!-- ... -->`) and trim leading/trailing whitespace.
fn clean(s: String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("-->") {
            rest = &rest[start + end + 3..];
        } else {
            // Unclosed comment — drop the rest.
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Load a prompt from `<prompts_dir>/<name>.md`.
///
/// HTML comments (`<!-- ... -->`) are stripped and the result is trimmed.
/// On any I/O error (missing dir, missing file, permissions) logs a warning
/// and returns `fallback`.
pub fn load(name: &str, fallback: &str) -> String {
    let path = prompts_dir().join(format!("{name}.md"));
    match std::fs::read_to_string(&path) {
        Ok(content) => clean(content),
        Err(e) => {
            eprintln!("prompts: could not load {path:?}: {e} — using built-in fallback");
            fallback.to_string()
        }
    }
}

/// Like [`load`], but performs simple `{key}` → `value` substitutions in the
/// loaded (or fallback) string. Each entry in `vars` is a `("key", "value")`
/// pair; the placeholder in the template must be written `{key}`.
pub fn load_with(name: &str, fallback: &str, vars: &[(&str, &str)]) -> String {
    let mut content = load(name, fallback);
    for (key, value) in vars {
        content = content.replace(&format!("{{{key}}}"), value);
    }
    content
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::Mutex;

    // Serialise all env-var manipulation to prevent parallel test races.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn write_temp_prompt(name: &str, content: &str) -> (tempfile::TempDir, ()) {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join(format!("{name}.md"));
        let mut f = std::fs::File::create(&path).expect("create file");
        f.write_all(content.as_bytes()).expect("write");
        (dir, ())
    }

    #[test]
    fn load_returns_file_content_when_present() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let (dir, _) = write_temp_prompt("test_prompt", "hello from file\n");
        unsafe { std::env::set_var("AGENT_L_PROMPTS_DIR", dir.path()) };
        let result = load("test_prompt", "fallback");
        unsafe { std::env::remove_var("AGENT_L_PROMPTS_DIR") };
        // trailing newline is trimmed by clean()
        assert_eq!(result, "hello from file");
    }

    #[test]
    fn load_strips_html_comments() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let (dir, _) = write_temp_prompt("commented_prompt", "<!-- comment -->\nactual content\n");
        unsafe { std::env::set_var("AGENT_L_PROMPTS_DIR", dir.path()) };
        let result = load("commented_prompt", "fallback");
        unsafe { std::env::remove_var("AGENT_L_PROMPTS_DIR") };
        assert_eq!(result, "actual content");
    }

    #[test]
    fn load_returns_fallback_when_dir_missing() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe { std::env::set_var("AGENT_L_PROMPTS_DIR", "/nonexistent_dir_xyz_123") };
        let result = load("test_prompt", "my fallback");
        unsafe { std::env::remove_var("AGENT_L_PROMPTS_DIR") };
        assert_eq!(result, "my fallback");
    }

    #[test]
    fn load_with_substitutes_placeholders() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let (dir, _) = write_temp_prompt("tpl_prompt", "Today is {now}, user is {user}.");
        unsafe { std::env::set_var("AGENT_L_PROMPTS_DIR", dir.path()) };
        let result = load_with(
            "tpl_prompt",
            "fallback",
            &[("now", "2026-04-11"), ("user", "Alice")],
        );
        unsafe { std::env::remove_var("AGENT_L_PROMPTS_DIR") };
        assert_eq!(result, "Today is 2026-04-11, user is Alice.");
    }

    #[test]
    fn load_with_substitutes_in_fallback_when_file_missing() {
        let _guard = ENV_MUTEX.lock().unwrap();
        unsafe { std::env::set_var("AGENT_L_PROMPTS_DIR", "/nonexistent_dir_xyz_123") };
        let result = load_with("missing", "date={now}", &[("now", "2026-04-11")]);
        unsafe { std::env::remove_var("AGENT_L_PROMPTS_DIR") };
        assert_eq!(result, "date=2026-04-11");
    }
}
