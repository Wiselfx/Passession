//! Pure parts of the local-directory session creation form.

use serde_json::{Value, json};

use super::rpc::str_of;

/// Directory preparation choices; all session creation uses an existing directory.
pub const SOURCES: [&str; 3] = ["directory", "clone", "new-directory"];

/// The durable directory owner backing a new session.
pub fn workspace_source(cwd: &str) -> Value {
    json!({ "kind": "directory", "path": cwd })
}

/// Feature value of `feature` in `config` (`featureValues[id]`, else the offered value).
pub fn feature_value<'a>(config: &'a Value, feature: &'a Value) -> &'a Value {
    let id = str_of(feature, "id");
    config
        .get("featureValues")
        .and_then(|values| values.get(id))
        .or_else(|| feature.get("value"))
        .unwrap_or(&Value::Null)
}

/// `withFeature`: the config with one feature value set.
pub fn with_feature(config: &Value, id: &str, value: Value) -> Value {
    let mut next = config.as_object().cloned().unwrap_or_default();
    let mut values = next
        .get("featureValues")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    values.insert(id.to_owned(), value);
    next.insert("featureValues".into(), Value::Object(values));
    Value::Object(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_source_uses_existing_directory() {
        assert_eq!(
            workspace_source("/existing/worktree"),
            json!({"kind": "directory", "path": "/existing/worktree"})
        );
        assert_eq!(SOURCES, ["directory", "clone", "new-directory"]);
    }
}
