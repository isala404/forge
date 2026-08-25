use crate::{ForgeError, Result};

const MAX_COMPONENT_BYTES: usize = 255;
const KV_LOGICAL_BUDGET: usize = 383;
const BLOB_LOGICAL_BUDGET: usize = 895;

fn validate_component(label: &str, component: &str) -> Result<()> {
    if component.is_empty() || component.len() > MAX_COMPONENT_BYTES {
        return Err(ForgeError::invalid(format!(
            "scope {label} must contain 1 to 255 bytes"
        )));
    }
    if component.chars().any(char::is_control) {
        return Err(ForgeError::invalid(format!(
            "scope {label} must not contain control characters"
        )));
    }
    Ok(())
}

fn render_scoped_name(
    kind: &str,
    budget: usize,
    application: &str,
    tenant: &str,
    user: &str,
    resource: &str,
) -> Result<String> {
    for (label, component) in [
        ("application", application),
        ("tenant", tenant),
        ("user", user),
        ("resource", resource),
    ] {
        validate_component(label, component)?;
    }
    let value = format!(
        "v1|{kind}|{}:{}{}:{}{}:{}{}:{}",
        application.len(),
        application,
        tenant.len(),
        tenant,
        user.len(),
        user,
        resource.len(),
        resource
    );
    if value.len() > budget {
        return Err(ForgeError::limit(format!(
            "scoped {kind} name exceeds its backend-safe length"
        )));
    }
    Ok(value)
}

pub fn scope_kv_key(application: &str, tenant: &str, user: &str, resource: &str) -> Result<String> {
    render_scoped_name("kv", KV_LOGICAL_BUDGET, application, tenant, user, resource)
}

pub fn scope_blob_key(
    application: &str,
    tenant: &str,
    user: &str,
    resource: &str,
) -> Result<String> {
    render_scoped_name(
        "blob",
        BLOB_LOGICAL_BUDGET,
        application,
        tenant,
        user,
        resource,
    )
}

pub fn scope_rate_limit_subject(
    application: &str,
    tenant: &str,
    user: &str,
    resource: &str,
) -> Result<String> {
    render_scoped_name(
        "rate",
        KV_LOGICAL_BUDGET,
        application,
        tenant,
        user,
        resource,
    )
}

pub fn scope_topic(application: &str, tenant: &str, user: &str, resource: &str) -> Result<String> {
    render_scoped_name(
        "topic",
        KV_LOGICAL_BUDGET,
        application,
        tenant,
        user,
        resource,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedScope {
    pub kind: String,
    pub application: String,
    pub tenant: String,
    pub user: String,
    pub resource: String,
}

pub fn parse_scoped_name(value: &str) -> Result<ParsedScope> {
    let rest = value
        .strip_prefix("v1|")
        .ok_or_else(|| ForgeError::invalid("scoped name must use v1"))?;
    let (kind, mut encoded) = rest
        .split_once('|')
        .ok_or_else(|| ForgeError::invalid("scoped name is malformed"))?;
    let budget = match kind {
        "blob" => BLOB_LOGICAL_BUDGET,
        "kv" | "rate" | "topic" => KV_LOGICAL_BUDGET,
        _ => return Err(ForgeError::invalid("scoped name kind is unknown")),
    };
    if value.len() > budget {
        return Err(ForgeError::limit(format!(
            "scoped {kind} name exceeds its backend-safe length"
        )));
    }
    let mut components = Vec::with_capacity(4);
    for label in ["application", "tenant", "user", "resource"] {
        let (length, tail) = encoded
            .split_once(':')
            .ok_or_else(|| ForgeError::invalid("scoped name is malformed"))?;
        if length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ForgeError::invalid("scoped name length is malformed"));
        }
        let length: usize = length
            .parse()
            .map_err(|_| ForgeError::invalid("scoped name length is malformed"))?;
        let component = tail
            .as_bytes()
            .get(..length)
            .and_then(|part| std::str::from_utf8(part).ok())
            .ok_or_else(|| ForgeError::invalid("scoped name component length is invalid"))?;
        validate_component(label, component)?;
        components.push(component.to_string());
        encoded = tail
            .get(length..)
            .ok_or_else(|| ForgeError::invalid("scoped name component length is invalid"))?;
    }
    if !encoded.is_empty() {
        return Err(ForgeError::invalid("scoped name has trailing data"));
    }
    let [application, tenant, user, resource]: [String; 4] = components
        .try_into()
        .map_err(|_| ForgeError::invalid("scoped name has the wrong component count"))?;
    Ok(ParsedScope {
        kind: kind.to_string(),
        application,
        tenant,
        user,
        resource,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn scoped_names_are_reversible_and_primitive_specific() {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../contract/scope-vectors.json")).unwrap();
        let valid = vectors.get("valid").unwrap();
        let component = |name: &str| valid.get(name).and_then(serde_json::Value::as_str).unwrap();
        let args = (
            component("application"),
            component("tenant"),
            component("user"),
            component("resource"),
        );
        let kv = scope_kv_key(args.0, args.1, args.2, args.3).unwrap();
        assert_eq!(kv, component("kv"));
        assert_eq!(
            scope_blob_key(args.0, args.1, args.2, args.3).unwrap(),
            component("blob")
        );
        assert_eq!(
            scope_rate_limit_subject(args.0, args.1, args.2, args.3).unwrap(),
            component("rate")
        );
        assert_eq!(
            scope_topic(args.0, args.1, args.2, args.3).unwrap(),
            component("topic")
        );
        assert_eq!(
            parse_scoped_name(&kv).unwrap(),
            ParsedScope {
                kind: "kv".into(),
                application: args.0.into(),
                tenant: args.1.into(),
                user: args.2.into(),
                resource: args.3.into(),
            }
        );
    }

    #[test]
    fn invalid_names_fail_before_backend_io() {
        assert!(scope_kv_key("", "t", "u", "r").is_err());
        assert!(scope_topic("app", "t", "u", "a\nb").is_err());
        assert!(parse_scoped_name("v1|kv|+3:app1:t1:u1:r").is_err());
        let long = "x".repeat(100);
        assert!(scope_kv_key(&long, &long, &long, &long).is_err());
        assert!(scope_blob_key(&long, &long, &long, &long).is_ok());
    }
}
