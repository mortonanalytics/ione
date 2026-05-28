use anyhow::bail;

pub fn validate_json_pointer(ptr: &str) -> anyhow::Result<()> {
    if ptr.is_empty() {
        return Ok(());
    }
    if !ptr.starts_with('/') {
        bail!("JSON Pointer must be empty or start with '/'");
    }
    let mut chars = ptr.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0') | Some('1') => {}
                _ => bail!("JSON Pointer contains invalid '~' escape"),
            }
        }
    }
    Ok(())
}
