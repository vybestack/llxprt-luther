use luther_workflow::workflow::schema::WorkflowConfig;

pub fn interpolate_config_variables(raw: &str, config: &WorkflowConfig) -> Result<String, String> {
    let mut value = raw.to_string();
    for _ in 0..config.variables.len().max(1) {
        let previous = value.clone();
        for (key, replacement) in &config.variables {
            value = value.replace(&format!("{{{key}}}"), replacement);
        }
        if value == previous {
            return Ok(value);
        }
    }
    if has_unresolved_config_token(&value) {
        Err(format!(
            "unresolved variable interpolation in artifact path: {value}"
        ))
    } else {
        Ok(value)
    }
}

pub fn has_unresolved_config_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        let close = match value[index + 1..].find('}') {
            // A stray opening brace with no matching '}' cannot be a valid
            // config-token reference, so treat it as literal text rather than
            // reporting an unresolved variable.
            Some(close) => close,
            None => {
                index += 1;
                continue;
            }
        };
        let token = &value[index + 1..index + 1 + close];
        if is_config_token_name(token) {
            return true;
        }
        index += close + 2;
    }
    false
}

pub fn is_config_token_name(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    for byte in token.bytes() {
        if !is_config_token_byte(byte) {
            return false;
        }
    }
    true
}

pub const CONFIG_TOKEN_UNDERSCORE: u8 = b'_';
pub const CONFIG_TOKEN_DOT: u8 = b'.';

pub fn is_config_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == CONFIG_TOKEN_UNDERSCORE || byte == CONFIG_TOKEN_DOT
}

#[cfg(test)]
mod tests {
    use super::*;
    use luther_workflow::workflow::config_loader::parse_workflow_config_toml;

    fn config(pairs: &[(&str, &str)]) -> WorkflowConfig {
        let mut toml = String::from(
            "config_id = \"cfg\"\nworkflow_type_id = \"wf\"\n\n\
             [runtime]\ntimeout_seconds = 1\nmax_retries = 0\n\n\
             [repository]\nworkspace_strategy = \"reuse\"\n\
             branch_template = \"issue{issue_number}\"\nbase_branch = \"main\"\n\n\
             [guards]\n",
        );
        if !pairs.is_empty() {
            toml.push_str("\n[variables]\n");
            for (key, value) in pairs {
                toml.push_str(&format!("{key} = \"{value}\"\n"));
            }
        }
        parse_workflow_config_toml(&toml).expect("parse config")
    }

    #[test]
    fn interpolates_single_variable() {
        let cfg = config(&[("issue_number", "42")]);
        let out = interpolate_config_variables("issue-{issue_number}", &cfg).unwrap();
        assert_eq!(out, "issue-42");
    }

    #[test]
    fn interpolates_chained_variables() {
        let cfg = config(&[("base", "root"), ("full", "{base}/child")]);
        let out = interpolate_config_variables("{full}/leaf", &cfg).unwrap();
        assert_eq!(out, "root/child/leaf");
    }

    #[test]
    fn unresolved_variable_is_error() {
        // A single-variable config allows one interpolation pass. `{a}` expands
        // to `{b}`, which is a valid-looking token that never resolves, so the
        // pass budget is exhausted and the unresolved token is reported.
        let cfg = config(&[("a", "{b}")]);
        let err = interpolate_config_variables("{a}", &cfg).unwrap_err();
        assert!(err.contains("unresolved variable interpolation"));
    }

    #[test]
    fn literal_string_without_tokens_passes_through() {
        let cfg = config(&[]);
        let out = interpolate_config_variables("plain/path", &cfg).unwrap();
        assert_eq!(out, "plain/path");
    }

    #[test]
    fn stray_unmatched_open_brace_is_literal_not_unresolved() {
        // A lone '{' with no closing '}' (e.g. a shell/glob snippet) must not be
        // treated as an unresolved config token.
        assert!(!has_unresolved_config_token("echo ${HOME"));
        assert!(!has_unresolved_config_token("prefix { suffix"));
    }

    #[test]
    fn brace_group_without_close_is_literal() {
        let cfg = config(&[]);
        let out = interpolate_config_variables("a{b,c}d", &cfg).unwrap();
        // `{b,c}` is not a valid token name (comma), so it is treated as literal.
        assert_eq!(out, "a{b,c}d");
    }

    #[test]
    fn valid_token_name_detection() {
        assert!(is_config_token_name("work_dir"));
        assert!(is_config_token_name("parent.artifact_dir"));
        assert!(is_config_token_name("abc123"));
        assert!(!is_config_token_name(""));
        assert!(!is_config_token_name("has space"));
        assert!(!is_config_token_name("has-dash"));
    }

    #[test]
    fn unresolved_detection_finds_real_token() {
        assert!(has_unresolved_config_token("path/{artifact_dir}/x"));
        assert!(!has_unresolved_config_token("path/{with space}/x"));
    }

    #[test]
    fn config_token_byte_classification() {
        assert!(is_config_token_byte(b'a'));
        assert!(is_config_token_byte(b'Z'));
        assert!(is_config_token_byte(b'5'));
        assert!(is_config_token_byte(CONFIG_TOKEN_UNDERSCORE));
        assert!(is_config_token_byte(CONFIG_TOKEN_DOT));
        assert!(!is_config_token_byte(b'-'));
        assert!(!is_config_token_byte(b' '));
    }
}
