use super::*;

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
            Some(close) => close,
            None => return true,
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
