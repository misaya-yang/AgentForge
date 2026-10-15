//! Canonical quiz request validation and response allowlists. Identity resolution
//! deliberately retains the host's guest/API-key/cookie modes.
pub use crate::artifact_shares_api::{
    ArtifactSharesError as QuizError, ArtifactSharesFuture as QuizFuture,
    ArtifactSharesIdentity as QuizIdentity,
};
use crate::{
    artifact_shares_api::parse_uuid,
    rate_config_api::{boolean, integer},
};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug)]
pub struct QuizSubmit {
    pub answers: Map<String, Value>,
    pub attempt_id: Option<String>,
    pub display_name: Option<String>,
    pub attempt_token: Option<String>,
}
fn problem(field: &str, typ: &str, msg: &str, input: Value) -> Value {
    json!({"type":typ,"loc":["body",field],"msg":msg,"input":input})
}
fn object(value: &Value) -> Result<&Map<String, Value>, QuizError> {
    if value.is_null() {
        return Err(QuizError::detail(
            422,
            json!([{"type":"missing","loc":["body"],"msg":"Field required","input":null}]),
        ));
    }
    value.as_object().ok_or_else(||QuizError::detail(422,json!([{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":value}])))
}
fn optional_string(v: &Map<String, Value>, field: &str, errors: &mut Vec<Value>) -> Option<String> {
    match v.get(field) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(x) => {
            errors.push(problem(
                field,
                "string_type",
                "Input should be a valid string",
                x.clone(),
            ));
            None
        }
    }
}
impl QuizSubmit {
    pub fn parse(raw: &Value, shared: bool) -> Result<Self, QuizError> {
        let v = object(raw)?;
        let mut errors = vec![];
        let answers = match v.get("answers") {
            None => {
                errors.push(problem("answers", "missing", "Field required", raw.clone()));
                Map::new()
            }
            Some(Value::Object(m)) => {
                for (key, item) in m {
                    if !item.is_string() {
                        errors.push(json!({"type":"string_type","loc":["body","answers",key],"msg":"Input should be a valid string","input":item}));
                    }
                }
                m.clone()
            }
            Some(x) => {
                errors.push(problem(
                    "answers",
                    "dict_type",
                    "Input should be a valid dictionary",
                    x.clone(),
                ));
                Map::new()
            }
        };
        let mut attempt_id = None;
        let mut display_name = None;
        let mut attempt_token = None;
        if shared {
            display_name = optional_string(v, "display_name", &mut errors);
            attempt_token = optional_string(v, "attempt_token", &mut errors);
        } else {
            match v.get("attempt_id") {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) => match parse_uuid(s, "body", "attempt_id") {
                    Ok(id) => attempt_id = Some(id),
                    Err(QuizError::Http(_, b)) => {
                        errors.extend(b["detail"].as_array().unwrap().clone())
                    }
                    Err(e) => return Err(e),
                },
                Some(x) => errors.push(problem(
                    "attempt_id",
                    "uuid_type",
                    "UUID input should be a string, bytes or UUID object",
                    x.clone(),
                )),
            }
        }
        if !errors.is_empty() {
            return Err(QuizError::detail(422, json!(errors)));
        }
        Ok(Self {
            answers,
            attempt_id,
            display_name,
            attempt_token,
        })
    }
}
pub fn result_token(raw: &Value) -> Result<String, QuizError> {
    let v = object(raw)?;
    match v.get("attempt_token") {
        None => Err(QuizError::detail(
            422,
            json!([problem(
                "attempt_token",
                "missing",
                "Field required",
                raw.clone()
            )]),
        )),
        Some(Value::String(s)) => {
            let n = s.chars().count();
            if n == 0 || n > 200 {
                let (typ, msg, key, bound) = if n == 0 {
                    (
                        "string_too_short",
                        "String should have at least 1 character",
                        "min_length",
                        1,
                    )
                } else {
                    (
                        "string_too_long",
                        "String should have at most 200 characters",
                        "max_length",
                        200,
                    )
                };
                let mut p = problem("attempt_token", typ, msg, json!(s));
                p["ctx"] = json!({key:bound});
                Err(QuizError::detail(422, json!([p])))
            } else {
                Ok(s.clone())
            }
        }
        Some(x) => Err(QuizError::detail(
            422,
            json!([problem(
                "attempt_token",
                "string_type",
                "Input should be a valid string",
                x.clone()
            )]),
        )),
    }
}

pub fn pagination(
    query: &std::collections::HashMap<String, String>,
) -> Result<(i64, i64), QuizError> {
    let mut errors = vec![];
    let mut out = [100, 0];
    for (i, (field, default, min, max)) in [("limit", 100, 1, Some(1000)), ("offset", 0, 0, None)]
        .into_iter()
        .enumerate()
    {
        let raw = query.get(field).map(|s| json!(s)).unwrap_or(json!(default));
        match integer(&raw){Ok(n)=>{out[i]=n;let violation=if n<min{Some(("greater_than_equal",format!("Input should be greater than or equal to {min}"),json!({"ge":min})))}else if max.is_some_and(|m|n>m){Some(("less_than_equal",format!("Input should be less than or equal to {}",max.unwrap()),json!({"le":max.unwrap()})))}else{None};if let Some((t,m,c))=violation{errors.push(json!({"type":t,"loc":["query",field],"msg":m,"input":raw,"ctx":c}));}},Err(t)=>errors.push(json!({"type":t,"loc":["query",field],"msg":"Input should be a valid integer, unable to parse string as an integer","input":raw}))}
    }
    if errors.is_empty() {
        Ok((out[0], out[1]))
    } else {
        Err(QuizError::detail(422, json!(errors)))
    }
}

fn required<'a>(v: &'a Value, k: &str) -> Result<&'a Value, QuizError> {
    v.get(k).ok_or(QuizError::Internal)
}
fn field(v: &Value, k: &str, kind: &str, nullable: bool) -> Result<Value, QuizError> {
    let x = required(v, k)?;
    if nullable && x.is_null() {
        return Ok(Value::Null);
    };
    match kind {
        "string" => x.as_str().map(|s| json!(s)).ok_or(QuizError::Internal),
        "uuid" => x
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(|id| json!(id.to_string()))
            .ok_or(QuizError::Internal),
        "int" => integer(x)
            .map(|n| json!(n))
            .map_err(|_| QuizError::Internal),
        "float" => x
            .as_f64()
            .or_else(|| x.as_str().and_then(|s| s.parse::<f64>().ok()))
            .filter(|n| n.is_finite())
            .map(|n| json!(n))
            .ok_or(QuizError::Internal),
        "bool" => boolean(x)
            .map(|b| json!(b))
            .map_err(|_| QuizError::Internal),
        "date" => x
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| json!(d.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)))
            .ok_or(QuizError::Internal),
        _ => Err(QuizError::Internal),
    }
}
fn project(v: &Value, fields: &[(&str, &str, bool)]) -> Result<Value, QuizError> {
    let mut out = Map::new();
    for (k, t, n) in fields {
        out.insert((*k).into(), field(v, k, t, *n)?);
    }
    Ok(Value::Object(out))
}
pub fn project_attempt(v: &Value) -> Result<Value, QuizError> {
    let mut o = project(
        v,
        &[
            ("attempt_id", "uuid", false),
            ("total_score", "float", false),
            ("correct_count", "int", false),
            ("total_count", "int", false),
        ],
    )?;
    o["cached"] = match v.get("cached") {
        None => json!(false),
        Some(_) => field(v, "cached", "bool", false)?,
    };
    let qs = required(v, "per_question")?
        .as_array()
        .filter(|a| a.iter().all(Value::is_object))
        .ok_or(QuizError::Internal)?;
    o["per_question"] = json!(qs);
    Ok(o)
}
pub fn project_quiz(v: &Value, shared: bool) -> Result<Value, QuizError> {
    let mut o = if shared {
        project(
            v,
            &[
                ("share_code", "string", false),
                ("kind", "string", false),
                ("title", "string", false),
                ("require_name", "bool", false),
                ("time_limit_minutes", "int", true),
                ("quiz_id", "uuid", true),
                ("question_count", "int", false),
                ("difficulty", "string", false),
            ],
        )?
    } else {
        project(
            v,
            &[
                ("quiz_id", "uuid", false),
                ("title", "string", false),
                ("description", "string", true),
                ("topic", "string", true),
                ("difficulty", "string", false),
                ("question_count", "int", false),
                ("status", "string", false),
                ("created_at", "date", true),
            ],
        )?
    };
    if shared {
        o["description"] = if v.get("description").is_none() {
            Value::Null
        } else {
            field(v, "description", "string", true)?
        };
        let a = v.get("audience").cloned().unwrap_or(json!("public"));
        if a != "public" && a != "internal" {
            return Err(QuizError::Internal);
        }
        o["audience"] = a;
    }
    let mut qs = vec![];
    for q in required(v, "questions")?
        .as_array()
        .ok_or(QuizError::Internal)?
    {
        let mut p = project(
            q,
            &[
                ("id", "uuid", false),
                ("question_num", "int", false),
                ("question_type", "string", false),
                ("question_text", "string", false),
            ],
        )?;
        p["options"] = json!(
            required(q, "options")?
                .as_array()
                .ok_or(QuizError::Internal)?
        );
        qs.push(p);
    }
    o["questions"] = json!(qs);
    Ok(o)
}
pub fn project_attempt_list(v: &Value) -> Result<Value, QuizError> {
    let mut items = vec![];
    for row in required(v, "attempts")?
        .as_array()
        .ok_or(QuizError::Internal)?
    {
        items.push(project(
            row,
            &[
                ("attempt_id", "uuid", false),
                ("user_id", "string", true),
                ("display_name", "string", true),
                ("total_score", "float", true),
                ("correct_count", "int", true),
                ("total_count", "int", true),
                ("started_at", "date", true),
                ("completed_at", "date", true),
                ("status", "string", false),
            ],
        )?);
    }
    Ok(json!({"attempts":items,"total":field(v,"total","int",false)?}))
}
pub fn project_start(v: &Value) -> Result<Value, QuizError> {
    project(
        v,
        &[
            ("attempt_token", "string", false),
            ("started_at", "date", false),
            ("expires_at", "date", false),
        ],
    )
}
