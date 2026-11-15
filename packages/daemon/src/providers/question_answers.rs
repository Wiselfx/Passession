//! Reads one question card's answer from a permission response.
//!
//! Every client sends `updatedInput.answers[header]` as one string: a multi-select joins its
//! labels and custom answer with ", ". Current clients also send
//! `updatedInput.selections[header]` as `{"options":[label, ...],"other":"text"}`, which keeps a
//! label containing ", " whole. The string stays the path for clients without `selections`.

use serde_json::Value;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Answer {
    /// Chosen option labels, in the client's order.
    pub options: Vec<String>,
    /// The trimmed custom answer, when one was typed.
    pub other: Option<String>,
}

/// `None` when the response carries no answer for `header`.
pub fn read(response: &Value, header: &str, labels: &[&str], multi: bool) -> Option<Answer> {
    let input = &response["updatedInput"];
    if let Some(selection) = input["selections"].get(header).filter(|v| v.is_object()) {
        return Some(Answer {
            options: selection["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            other: custom(selection["other"].as_str().unwrap_or("")),
        });
    }
    let text = input["answers"].get(header)?.as_str()?;
    Some(if multi {
        split(text, labels)
    } else if labels.contains(&text) {
        Answer {
            options: vec![text.to_owned()],
            other: None,
        }
    } else {
        Answer {
            options: Vec::new(),
            other: custom(text),
        }
    })
}

/// Splits a joined multi-select answer against the option labels, longest label first, so a
/// label containing ", " stays whole. The unmatched remainder is the custom answer.
pub fn split(answer: &str, labels: &[&str]) -> Answer {
    let mut remaining = answer.trim();
    let mut options: Vec<String> = Vec::new();
    while !remaining.is_empty() {
        let Some(label) = labels
            .iter()
            .copied()
            .filter(|label| !options.iter().any(|chosen| chosen == label))
            .filter(|label| {
                remaining == *label
                    || remaining
                        .strip_prefix(*label)
                        .is_some_and(|rest| rest.starts_with(", "))
            })
            .max_by_key(|label| label.len())
        else {
            break;
        };
        options.push(label.to_owned());
        remaining = remaining.get(label.len() + 2..).unwrap_or("");
    }
    Answer {
        options,
        other: custom(remaining),
    }
}

fn custom(text: &str) -> Option<String> {
    Some(text.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structured_selection_keeps_labels_with_commas() {
        let response = json!({"updatedInput":{
            "answers":{"Pick":"A, B, C, typed"},
            "selections":{"Pick":{"options":["A, B"],"other":" typed "}}
        }});
        assert_eq!(
            read(&response, "Pick", &["A, B", "C"], true),
            Some(Answer {
                options: vec!["A, B".into()],
                other: Some("typed".into())
            })
        );
    }

    #[test]
    fn legacy_string_prefers_longest_label_and_keeps_custom_tail() {
        let response = json!({"updatedInput":{"answers":{"Pick":"A, B, C, my, own"}}});
        assert_eq!(
            read(&response, "Pick", &["A", "A, B", "C"], true),
            Some(Answer {
                options: vec!["A, B".into(), "C".into()],
                other: Some("my, own".into())
            })
        );
        let single = json!({"updatedInput":{"answers":{"Pick":"something else"}}});
        assert_eq!(
            read(&single, "Pick", &["A"], false)
                .unwrap()
                .other
                .as_deref(),
            Some("something else")
        );
        assert_eq!(read(&single, "Missing", &["A"], false), None);
    }
}
