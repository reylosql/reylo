//! The JSON `reylo analyze` prints to a terminal: each object and array on one line where it fits
//! within [`WIDTH`] columns, counted from the line's start, its indentation, key and a trailing
//! comma included, and broken over indented lines, two spaces a level, where it does not. So a type
//! reads `{ "kind": "integer", "bits": 64, "label": "int8" }` on one line, as a descriptor's are
//! short. It is JSON a parser reads back as the same value.

use serde_json::Value;

/// The column a line stays within.
pub const WIDTH: usize = 80;

/// `value` laid out compactly, with no newline after it.
pub fn compact(value: &Value) -> String {
    let mut out = String::new();
    write(value, 0, 0, 0, &mut out);
    out
}

/// `value` at a line whose indentation is `indent`, starting after `used` columns of it, and before
/// `after` columns that follow it on its last line (a comma).
fn write(value: &Value, indent: usize, used: usize, after: usize, out: &mut String) {
    let one = flat(value);
    let empty = match value {
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => true,
    };
    if empty || used + one.chars().count() + after <= WIDTH {
        out.push_str(&one);
        return;
    }
    let pad = " ".repeat(indent + 2);
    match value {
        Value::Object(map) => {
            out.push_str("{\n");
            for (i, (key, item)) in map.iter().enumerate() {
                let last = i + 1 == map.len();
                let key = Value::String(key.clone()).to_string();
                out.push_str(&pad);
                out.push_str(&key);
                out.push_str(": ");
                write(item, indent + 2, indent + 2 + key.chars().count() + 2, usize::from(!last), out);
                out.push_str(if last { "\n" } else { ",\n" });
            }
            out.push_str(&" ".repeat(indent));
            out.push('}');
        }
        Value::Array(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                let last = i + 1 == items.len();
                out.push_str(&pad);
                write(item, indent + 2, indent + 2, usize::from(!last), out);
                out.push_str(if last { "\n" } else { ",\n" });
            }
            out.push_str(&" ".repeat(indent));
            out.push(']');
        }
        _ => unreachable!("a scalar is written on one line"),
    }
}

/// A value on one line: an object's members inside spaces, `{ "a": 1 }`, an array's without, `[1, 2]`.
fn flat(value: &Value) -> String {
    match value {
        Value::Object(map) if map.is_empty() => "{}".into(),
        Value::Object(map) => format!("{{ {} }}", map.iter().map(|(k, v)| format!("{}: {}", Value::String(k.clone()), flat(v))).collect::<Vec<_>>().join(", ")),
        Value::Array(items) => format!("[{}]", items.iter().map(flat).collect::<Vec<_>>().join(", ")),
        scalar => scalar.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_value_that_fits_stays_on_one_line() {
        assert_eq!(compact(&json!({ "kind": "integer", "bits": 64, "label": "int8" })), r#"{ "kind": "integer", "bits": 64, "label": "int8" }"#);
        assert_eq!(compact(&json!([[1, 1114111]])), "[[1, 1114111]]");
        assert_eq!(compact(&json!("a\nb")), r#""a\nb""#);
        assert_eq!(compact(&json!(null)), "null");
    }

    #[test]
    fn an_empty_object_and_array_are_written_bare() {
        assert_eq!(compact(&json!({})), "{}");
        assert_eq!(compact(&json!([])), "[]");
        let long = "x".repeat(90);
        assert_eq!(compact(&json!({ "a": long, "params": [], "o": {} })), format!("{{\n  \"a\": \"{long}\",\n  \"params\": [],\n  \"o\": {{}}\n}}"));
    }

    #[test]
    fn a_value_too_wide_breaks_and_its_parts_that_fit_stay_whole() {
        let value = json!({
            "sql": "select id, email, bio from users where id = $1 and email is not null and bio is not null",
            "params": [{ "name": "id", "type": { "kind": "integer", "bits": 64, "label": "int8" } }],
            "diagnostics": []
        });
        assert_eq!(
            compact(&value),
            r#"{
  "sql": "select id, email, bio from users where id = $1 and email is not null and bio is not null",
  "params": [
    { "name": "id", "type": { "kind": "integer", "bits": 64, "label": "int8" } }
  ],
  "diagnostics": []
}"#
        );
    }

    #[test]
    fn nested_values_break_level_by_level() {
        let value = json!({ "result": { "kind": "table", "cardinality": "at_most_one", "columns": [{ "name": "email", "type": { "kind": "text", "code_points": [[1, 1114111]], "size": { "max": 255 }, "label": "varchar" } }] } });
        assert_eq!(
            compact(&value),
            r#"{
  "result": {
    "kind": "table",
    "cardinality": "at_most_one",
    "columns": [
      {
        "name": "email",
        "type": {
          "kind": "text",
          "code_points": [[1, 1114111]],
          "size": { "max": 255 },
          "label": "varchar"
        }
      }
    ]
  }
}"#
        );
    }

    #[test]
    fn a_line_holds_to_80_columns_its_comma_included() {
        // `  "k": ` is 7 columns, and a comma follows the value: 7 + 72 + 1 = 80 fits, 73 does not.
        for (width, fits) in [(72, true), (73, false)] {
            let value = json!({ "k": ["x".repeat(width - 18), "aaaaaaaaaa"], "z": 1 });
            assert_eq!(flat(&value["k"]).chars().count(), width);
            let out = compact(&value);
            assert_eq!(out.lines().nth(1).unwrap().ends_with("\"aaaaaaaaaa\"],"), fits, "{out}");
            assert_eq!(out.lines().count(), if fits { 4 } else { 7 }, "{out}");
        }
    }

    #[test]
    fn a_descriptor_reads_back_as_itself() {
        let examples: Value = serde_json::from_str(include_str!("../../../spec/examples.json")).unwrap();
        for (name, example) in examples.as_object().unwrap() {
            let descriptor = &example["descriptor"];
            let text = compact(descriptor);
            assert_eq!(&serde_json::from_str::<Value>(&text).unwrap(), descriptor, "{name}");
            // A line past the width holds one scalar too long for it, never an object or array.
            for line in text.lines().filter(|line| line.chars().count() > WIDTH) {
                let value: Value = serde_json::from_str(&format!("{{{}}}", line.trim().trim_end_matches(','))).or_else(|_| serde_json::from_str(line.trim().trim_end_matches(','))).unwrap();
                let scalar = value.as_object().map_or(!value.is_object() && !value.is_array(), |m| m.values().all(|v| !v.is_object() && !v.is_array()));
                assert!(scalar, "{name}: {line}");
            }
        }
    }
}
