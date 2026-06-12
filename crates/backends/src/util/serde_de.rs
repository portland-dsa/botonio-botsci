use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Deserializes `Some` only for a non-empty JSON string.
///
/// Any other JSON type (number, bool, object, array), a JSON `null`, an empty
/// string, or a missing key (when combined with `#[serde(default)]`) all yield
/// `None`. This preserves the graceful `.as_str()`-or-`None` behavior the
/// hand-written custom-field readers had, so a field that comes back as a
/// number or bool never hard-fails the whole response.
pub fn nonempty_string<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(de)?;
    Ok(v.as_ref()
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned))
}

/// Deserializes `Some(label)` from a Solidarity Tech *select*-type custom property.
///
/// ST serializes dropdown / multi-select custom properties as an array of
/// `{ "label": "...", "value": "..." }` objects - the `value` is an opaque id, the
/// `label` is the human-meaningful option text. This returns the first element's
/// non-empty `label`. For resilience it also accepts a bare non-empty string or an
/// array of bare strings. Anything else - `null`, `[]`, a number, an object with no
/// usable `label` - yields `None`, mirroring [`nonempty_string`]'s contract that a
/// surprising shape never hard-fails the whole response.
pub fn select_label<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(de)?;
    Ok(first_label(v.as_ref()))
}

fn first_label(v: Option<&Value>) -> Option<String> {
    fn non_empty(s: &str) -> Option<String> {
        (!s.is_empty()).then(|| s.to_owned())
    }
    match v {
        Some(Value::String(s)) => non_empty(s),
        Some(Value::Array(items)) => items.iter().find_map(|item| match item {
            Value::String(s) => non_empty(s),
            Value::Object(o) => o.get("label").and_then(Value::as_str).and_then(non_empty),
            _ => None,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Wrapper {
        #[serde(default, deserialize_with = "nonempty_string")]
        val: Option<String>,
    }

    fn de(json: &str) -> Option<String> {
        serde_json::from_str::<Wrapper>(json).unwrap().val
    }

    #[test]
    fn string_yields_some() {
        assert_eq!(de(r#"{"val":"hello"}"#), Some("hello".to_owned()));
    }

    #[test]
    fn empty_string_yields_none() {
        assert_eq!(de(r#"{"val":""}"#), None);
    }

    #[test]
    fn null_yields_none() {
        assert_eq!(de(r#"{"val":null}"#), None);
    }

    #[test]
    fn number_yields_none() {
        assert_eq!(de(r#"{"val":42}"#), None);
    }

    #[test]
    fn bool_yields_none() {
        assert_eq!(de(r#"{"val":true}"#), None);
    }

    #[test]
    fn missing_key_yields_none() {
        assert_eq!(de(r#"{}"#), None);
    }

    #[derive(Deserialize)]
    struct SelectWrapper {
        #[serde(default, deserialize_with = "select_label")]
        val: Option<String>,
    }

    fn de_select(json: &str) -> Option<String> {
        serde_json::from_str::<SelectWrapper>(json).unwrap().val
    }

    #[test]
    fn select_label_value_array_yields_label() {
        assert_eq!(
            de_select(r#"{"val":[{"label":"Member in Good Standing","value":"AfVqfj0n"}]}"#),
            Some("Member in Good Standing".to_owned())
        );
    }

    #[test]
    fn select_lapsed_label_value_array_yields_label() {
        assert_eq!(
            de_select(r#"{"val":[{"label":"Lapsed","value":"x"}]}"#),
            Some("Lapsed".to_owned())
        );
    }

    #[test]
    fn select_bare_string_yields_some() {
        assert_eq!(de_select(r#"{"val":"plain"}"#), Some("plain".to_owned()));
    }

    #[test]
    fn select_array_of_bare_strings_yields_first() {
        assert_eq!(de_select(r#"{"val":["a","b"]}"#), Some("a".to_owned()));
    }

    #[test]
    fn select_null_yields_none() {
        assert_eq!(de_select(r#"{"val":null}"#), None);
    }

    #[test]
    fn select_empty_array_yields_none() {
        assert_eq!(de_select(r#"{"val":[]}"#), None);
    }

    #[test]
    fn select_object_with_no_label_yields_none() {
        assert_eq!(de_select(r#"{"val":[{"value":"x"}]}"#), None);
    }

    #[test]
    fn select_number_yields_none() {
        assert_eq!(de_select(r#"{"val":42}"#), None);
    }
}
