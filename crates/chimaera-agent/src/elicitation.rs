//! MCP human input is distinct from permission to call a tool. Keep schema
//! interpretation and reply validation shared across both native drivers.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ELICITATION_BYTES: usize = 64 * 1024;
pub const ELICITATION_PENDING: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationAction {
    Accept,
    Decline,
    Cancel,
}

impl ElicitationAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Elicitation {
    pub mode: String,
    pub fields: Vec<ElicitationField>,
    pub url: Option<String>,
    /// Never pretend an unfamiliar schema is an empty form. The user can
    /// decline/cancel, and the runtime receives no fabricated accepted data.
    pub unsupported: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ElicitationField {
    pub name: String,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub required: bool,
    pub options: Vec<ElicitationOption>,
    pub fields: Vec<ElicitationField>,
    pub default: Option<Value>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    pub min_items: Option<u64>,
    pub max_items: Option<u64>,
    pub format: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ElicitationOption {
    pub value: String,
    pub label: String,
}

impl Elicitation {
    pub fn parse(mode: &str, schema: &Value, url: Option<&str>) -> Self {
        let mut elicitation = Self {
            mode: if ["url", "form", "openai/form", "openaiForm"].contains(&mode) {
                mode.into()
            } else {
                "unsupported".into()
            },
            fields: Vec::new(),
            url: None,
            unsupported: None,
        };
        if !["url", "form", "openai/form", "openaiForm"].contains(&mode) {
            elicitation.unsupported = Some("This MCP request uses an unsupported mode.".into());
        } else if mode == "url" {
            if let Some(url) = url.filter(|url| safe_url(url)) {
                elicitation.url = Some(url.into());
            } else {
                elicitation.unsupported =
                    Some("The server supplied an invalid web address.".into());
            }
        } else {
            match parse_fields(schema) {
                Ok(fields) => elicitation.fields = fields,
                Err(error) => elicitation.unsupported = Some(error.into()),
            }
        }
        if serde_json::to_vec(&elicitation).map_or(true, |v| v.len() > ELICITATION_BYTES) {
            elicitation.fields.clear();
            elicitation.unsupported = Some("The form exceeds the supported size.".into());
        }
        elicitation
    }

    pub fn validate(&self, action: ElicitationAction, content: &Value) -> Result<(), String> {
        if action != ElicitationAction::Accept {
            return Ok(());
        }
        if let Some(reason) = &self.unsupported {
            return Err(reason.clone());
        }
        if self.mode == "url" {
            if !content.is_null() && content.as_object().is_none_or(|v| !v.is_empty()) {
                return Err("A browser request cannot submit form data.".into());
            }
            return Ok(());
        }
        let values = content.as_object().ok_or("Form data must be an object.")?;
        if values
            .keys()
            .any(|name| !self.fields.iter().any(|field| &field.name == name))
        {
            return Err("The form contains an unknown field.".into());
        }
        for field in &self.fields {
            match values.get(&field.name) {
                None if field.required => return Err(format!("{} is required.", field.title)),
                None => {}
                Some(value) => field
                    .validate(value)
                    .map_err(|reason| format!("{}: {reason}", field.title))?,
            }
        }
        Ok(())
    }
}

fn safe_url(url: &str) -> bool {
    url.len() <= 8192
        && !url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        && (url.starts_with("https://") || url.starts_with("http://"))
        && url.split_once("://").is_some_and(|(_, host)| {
            !host.is_empty()
                && !host.starts_with('/')
                && !host
                    .split(['/', '?', '#'])
                    .next()
                    .unwrap_or_default()
                    .contains('@')
        })
}

fn parse_fields(schema: &Value) -> Result<Vec<ElicitationField>, &'static str> {
    parse_fields_at(schema, 0)
}

fn parse_fields_at(schema: &Value, depth: usize) -> Result<Vec<ElicitationField>, &'static str> {
    if depth > 8 {
        return Err("The form is nested too deeply.");
    }
    if serde_json::to_vec(schema).map_or(true, |v| v.len() > ELICITATION_BYTES) {
        return Err("The form exceeds the supported size.");
    }
    let root = schema
        .as_object()
        .ok_or("The server did not supply a form schema.")?;
    if schema["type"] != "object"
        || root.keys().any(|key| {
            ![
                "$schema",
                "type",
                "properties",
                "required",
                "title",
                "description",
                "default",
                "additionalProperties",
            ]
            .contains(&key.as_str())
        })
    {
        return Err("This form uses an unsupported schema. Please cancel and ask the agent for another way to continue.");
    }
    let properties = schema["properties"]
        .as_object()
        .ok_or("The form has no field definitions.")?;
    if properties.len() > 64 {
        return Err("The form has too many fields.");
    }
    if let Some(required) = schema.get("required") {
        if required.as_array().is_none_or(|values| {
            values
                .iter()
                .any(|v| v.as_str().is_none_or(|s| !properties.contains_key(s)))
        }) {
            return Err("The form's required fields are invalid.");
        }
    }
    properties
        .iter()
        .map(|(name, property)| {
            let object = property.as_object().ok_or("A form field is invalid.")?;
            if name.len() > 256
                || object.keys().any(|key| {
                    ![
                        "type",
                        "title",
                        "description",
                        "default",
                        "enum",
                        "enumNames",
                        "oneOf",
                        "items",
                        "minimum",
                        "maximum",
                        "minLength",
                        "maxLength",
                        "minItems",
                        "maxItems",
                        "format",
                        "properties",
                        "required",
                        "additionalProperties",
                    ]
                    .contains(&key.as_str())
                })
            {
                return Err("A form field uses unsupported constraints.");
            }
            let kind = property["type"].as_str().unwrap_or("");
            if !["string", "number", "integer", "boolean", "array", "object"].contains(&kind) {
                return Err("This form contains an unsupported field type.");
            }
            for key in ["minLength", "maxLength", "minItems", "maxItems"] {
                if property
                    .get(key)
                    .is_some_and(|value| value.as_u64().is_none())
                {
                    return Err("A form field has an invalid size constraint.");
                }
            }
            for key in ["minimum", "maximum"] {
                if property
                    .get(key)
                    .is_some_and(|value| value.as_f64().is_none())
                {
                    return Err("A form field has an invalid numeric constraint.");
                }
            }
            let mut options = Vec::new();
            let choices = if kind == "array" {
                &property["items"]
            } else {
                property
            };
            if kind == "array"
                && choices.as_object().is_none_or(|object| {
                    object
                        .keys()
                        .any(|key| !["type", "enum", "enumNames", "anyOf"].contains(&key.as_str()))
                })
            {
                return Err("The list uses unsupported constraints.");
            }
            if !["array", "string"].contains(&kind)
                && (choices.get("enum").is_some() || choices.get("oneOf").is_some())
            {
                return Err("Only text choices are supported.");
            }
            if let Some(values) = choices.get("enum") {
                for (index, value) in values
                    .as_array()
                    .ok_or("The form choices are invalid.")?
                    .iter()
                    .enumerate()
                {
                    let value = value.as_str().ok_or("The form choices must be text.")?;
                    options.push(ElicitationOption {
                        value: value.into(),
                        label: choices["enumNames"][index].as_str().unwrap_or(value).into(),
                    });
                }
            } else if let Some(values) = choices.get("oneOf").or_else(|| choices.get("anyOf")) {
                for value in values.as_array().ok_or("The form choices are invalid.")? {
                    if value.as_object().is_none_or(|object| {
                        object
                            .keys()
                            .any(|key| !["const", "title", "description"].contains(&key.as_str()))
                    }) {
                        return Err("A form choice uses unsupported constraints.");
                    }
                    let choice = value["const"]
                        .as_str()
                        .ok_or("The form choices must be text.")?;
                    options.push(ElicitationOption {
                        value: choice.into(),
                        label: value["title"].as_str().unwrap_or(choice).into(),
                    });
                }
            }
            if options.len() > 128 || (kind == "array" && options.is_empty()) {
                return Err("The form choices are unsupported.");
            }
            if let Some(format) = property.get("format") {
                if !["email", "uri", "date", "date-time"].contains(&format.as_str().unwrap_or("")) {
                    return Err("The form uses an unsupported text format.");
                }
            }
            let field = ElicitationField {
                name: name.clone(),
                title: property["title"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(name)
                    .into(),
                description: property["description"].as_str().unwrap_or_default().into(),
                kind: kind.into(),
                required: schema["required"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == name)),
                fields: if kind == "object" {
                    parse_fields_at(property, depth + 1)?
                } else {
                    Vec::new()
                },
                options,
                default: property.get("default").cloned(),
                minimum: property["minimum"].as_f64(),
                maximum: property["maximum"].as_f64(),
                min_length: property["minLength"].as_u64(),
                max_length: property["maxLength"].as_u64(),
                min_items: property["minItems"].as_u64(),
                max_items: property["maxItems"].as_u64(),
                format: property["format"].as_str().map(String::from),
            };
            if field
                .default
                .as_ref()
                .is_some_and(|value| field.validate(value).is_err())
            {
                return Err("A form field has an invalid default.");
            }
            Ok(field)
        })
        .collect()
}

impl ElicitationField {
    fn validate(&self, value: &Value) -> Result<(), &'static str> {
        match self.kind.as_str() {
            "string" => {
                let text = value.as_str().ok_or("enter text")?;
                let length = text.chars().count() as u64;
                if self.min_length.is_some_and(|n| length < n)
                    || self.max_length.is_some_and(|n| length > n)
                {
                    return Err("text length is outside the allowed range");
                }
                if !self.options.is_empty() && !self.options.iter().any(|o| o.value == text) {
                    return Err("choose one of the offered values");
                }
                if let Some(format) = &self.format {
                    if !valid_format(format, text) {
                        return Err("enter a valid value for this format");
                    }
                }
            }
            "number" | "integer" => {
                let number = value.as_f64().ok_or("enter a number")?;
                if self.kind == "integer" && number.fract() != 0.0 {
                    return Err("enter a whole number");
                }
                if self.minimum.is_some_and(|n| number < n)
                    || self.maximum.is_some_and(|n| number > n)
                {
                    return Err("number is outside the allowed range");
                }
            }
            "object" => {
                let object = value.as_object().ok_or("enter an object")?;
                if object
                    .keys()
                    .any(|key| !self.fields.iter().any(|field| &field.name == key))
                {
                    return Err("the object contains an unknown field");
                }
                for field in &self.fields {
                    match object.get(&field.name) {
                        None if field.required => return Err("a required nested field is missing"),
                        None => {}
                        Some(value) => field.validate(value)?,
                    }
                }
            }
            "boolean" if !value.is_boolean() => return Err("choose yes or no"),
            "array" => {
                let choices = value.as_array().ok_or("choose values from the list")?;
                if self.min_items.is_some_and(|n| (choices.len() as u64) < n)
                    || self.max_items.is_some_and(|n| (choices.len() as u64) > n)
                {
                    return Err("choose the allowed number of values");
                }
                for (index, choice) in choices.iter().enumerate() {
                    if !self.options.iter().any(|o| choice == &o.value)
                        || choices[..index].contains(choice)
                    {
                        return Err("choose distinct values from the list");
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn valid_format(format: &str, text: &str) -> bool {
    match format {
        "email" => {
            text.split_once('@').is_some_and(|(a, b)| {
                !a.is_empty()
                    && b.contains('.')
                    && !b.starts_with('.')
                    && !b.ends_with('.')
                    && !b.contains('@')
            }) && !text.chars().any(char::is_whitespace)
        }
        "uri" => {
            text.split_once(':').is_some_and(|(scheme, rest)| {
                !rest.is_empty()
                    && !scheme.is_empty()
                    && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                    && scheme
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            }) && !text.chars().any(char::is_whitespace)
        }
        "date" => valid_date(text),
        "date-time" => text
            .split_once(['T', 't'])
            .is_some_and(|(date, time)| valid_date(date) && valid_time(time)),
        _ => false,
    }
}

fn valid_date(text: &str) -> bool {
    if !text.is_ascii() {
        return false;
    }
    let parts: Vec<_> = text.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        parts[0].parse::<u32>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
    ) else {
        return false;
    };
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 0,
    };
    day >= 1 && day <= days
}

fn valid_time(text: &str) -> bool {
    if !text.is_ascii() {
        return false;
    }
    let (clock, zone) = if text.ends_with(['Z', 'z']) {
        (&text[..text.len() - 1], "Z")
    } else if let Some(index) = text.find(['+', '-']) {
        (&text[..index], &text[index..])
    } else {
        return false;
    };
    let parts: Vec<_> = clock.split(':').collect();
    if parts.len() != 3 || parts[0].len() != 2 || parts[1].len() != 2 {
        return false;
    }
    let (Ok(h), Ok(m), Ok(s)) = (
        parts[0].parse::<u32>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<f64>(),
    ) else {
        return false;
    };
    let offset_ok = zone == "Z"
        || (zone.len() == 6
            && zone.as_bytes()[3] == b':'
            && zone[1..3].parse::<u32>().is_ok_and(|h| h <= 23)
            && zone[4..].parse::<u32>().is_ok_and(|m| m <= 59));
    h < 24 && m < 60 && (0.0..61.0).contains(&s) && offset_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn nested_openai_form_validates_each_object() {
        let form = Elicitation::parse(
            "openai/form",
            &json!({"type":"object","properties":{"profile":{"type":"object","title":"Profile","required":["name"],"properties":{"name":{"type":"string","minLength":1}}}},"required":["profile"]}),
            None,
        );
        assert!(form.unsupported.is_none(), "{:?}", form.unsupported);
        assert!(form
            .validate(
                ElicitationAction::Accept,
                &json!({"profile":{"name":"Ada"}})
            )
            .is_ok());
        assert!(form
            .validate(
                ElicitationAction::Accept,
                &json!({"profile":{"name":false}})
            )
            .is_err());
        assert!(form
            .validate(ElicitationAction::Accept, &json!({"profile":{}}))
            .is_err());
    }
    #[test]
    fn typed_validation_preserves_false_zero_and_rejects_forged_values() {
        let form = Elicitation::parse(
            "form",
            &json!({"type":"object", "required":["enabled","count"], "properties":{
                "enabled":{"type":"boolean"}, "count":{"type":"integer","minimum":0,"maximum":10},
                "tags":{"type":"array","minItems":1,"items":{"anyOf":[{"const":"a","title":"A"},{"const":"b","title":"B"}]}},
                "date":{"type":"string","format":"date"}
            }}),
            None,
        );
        assert!(form.unsupported.is_none());
        assert!(form
            .validate(
                ElicitationAction::Accept,
                &json!({"enabled":false,"count":0,"tags":["a"],"date":"2024-02-29"})
            )
            .is_ok());
        for data in [
            json!({"enabled":"false","count":0}),
            json!({"enabled":false,"count":0.1}),
            json!({"enabled":false,"count":0,"tags":["a","a"]}),
            json!({"enabled":false,"count":0,"date":"2025-02-29"}),
            json!({"enabled":false,"count":0,"extra":"x"}),
        ] {
            assert!(form.validate(ElicitationAction::Accept, &data).is_err());
        }
    }
    #[test]
    fn unsupported_forms_can_only_be_declined_or_cancelled() {
        let form = Elicitation::parse(
            "form",
            &json!({"type":"object","properties":{"value":{"type":"string","pattern":"secret"}}}),
            None,
        );
        assert!(form
            .validate(ElicitationAction::Accept, &json!({}))
            .is_err());
        assert!(form
            .validate(ElicitationAction::Cancel, &Value::Null)
            .is_ok());
        assert!(form
            .validate(ElicitationAction::Decline, &Value::Null)
            .is_ok());
        assert!(
            Elicitation::parse("url", &Value::Null, Some("javascript:alert(1)"))
                .unsupported
                .is_some()
        );
    }
}
