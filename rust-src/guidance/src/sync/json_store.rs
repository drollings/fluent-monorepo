use std::path::Path;

use guidance_common::types::{CapabilityEval, GuidanceDoc, Member, MemberType, Meta, Param, Skill};
use smol_str::SmolStr;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum JsonError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("missing required field: {0}")]
    MissingField(String),
}

fn parse_member_type(s: &str) -> Option<MemberType> {
    Some(match s {
        "fn_decl" => MemberType::FnDecl,
        "fn_private" => MemberType::FnPrivate,
        "struct" => MemberType::Struct,
        "enum" => MemberType::Enum,
        "union" => MemberType::Union,
        "enum_field" => MemberType::EnumField,
        "test_decl" => MemberType::TestDecl,
        "comptime_block" => MemberType::ComptimeBlock,
        "method" => MemberType::Method,
        "method_private" => MemberType::MethodPrivate,
        _ => return None,
    })
}

fn parse_param(v: &serde_json::Value) -> Option<Param> {
    let obj = v.as_object()?;
    Some(Param {
        name: obj.get("name")?.as_str()?.into(),
        type_name: obj.get("type").and_then(|v| v.as_str()).map(SmolStr::from),
        default: obj.get("default").and_then(|v| v.as_str()).map(SmolStr::from),
    })
}

fn parse_member(v: &serde_json::Value) -> Option<Member> {
    let obj = v.as_object()?;
    let member_type = parse_member_type(obj.get("type")?.as_str()?)?;
    let name = obj.get("name")?.as_str()?;

    let params = obj
        .get("params")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(parse_param).collect())
        .unwrap_or_default();

    let members = obj
        .get("members")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(parse_member).collect())
        .unwrap_or_default();

    let equivalents = obj
        .get("equivalents")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(SmolStr::from).collect())
        .unwrap_or_default();

    let tags = obj
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(SmolStr::from).collect())
        .unwrap_or_default();

    Some(Member {
        type_name: member_type,
        name: name.into(),
        match_hash: obj.get("match_hash").and_then(|v| v.as_str()).map(SmolStr::from),
        signature: obj.get("signature").and_then(|v| v.as_str()).map(SmolStr::from),
        params,
        returns: obj.get("returns").and_then(|v| v.as_str()).map(SmolStr::from),
        comment: obj.get("comment").and_then(|v| v.as_str()).map(SmolStr::from),
        tags,
        is_pub: obj.get("is_pub").and_then(|v| v.as_bool()).unwrap_or(false),
        members,
        equivalents,
        line: obj.get("line").and_then(|v| v.as_u64()).map(|l| l as u32),
        comment_generated: obj.get("comment_generated").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

fn load_guidance_from_value(v: &serde_json::Value) -> Option<GuidanceDoc> {
    let root = v.as_object()?;
    let meta_obj = root.get("meta")?.as_object()?;
    let module = meta_obj.get("module")?.as_str()?;
    let source = meta_obj.get("source")?.as_str()?;

    let members = root
        .get("members")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(parse_member).collect())
        .unwrap_or_default();

    let skills = root
        .get("skills")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|sv| {
                    let so = sv.as_object()?;
                    Some(Skill {
                        ref_path: so.get("ref")?.as_str()?.into(),
                        context: so.get("context").and_then(|v| v.as_str()).map(SmolStr::from),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let parse_str_vec = |key: &str| -> Vec<SmolStr> {
        root.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(SmolStr::from).collect())
            .unwrap_or_default()
    };

    let capability_eval = root.get("capability_eval").and_then(|v| {
        let o = v.as_object()?;
        Some(CapabilityEval {
            capability_name: o.get("capability_name")?.as_str()?.into(),
            confidence: o.get("confidence")?.as_f64()? as f32,
            evaluated_at_hash: o.get("evaluated_at_hash")?.as_str()?.into(),
        })
    });

    Some(GuidanceDoc {
        meta: Meta {
            module: module.into(),
            source: source.into(),
            language: meta_obj.get("language").and_then(|v| v.as_str()).unwrap_or("zig").into(),
        },
        comment: root.get("comment").and_then(|v| v.as_str()).map(SmolStr::from),
        detail: root.get("detail").and_then(|v| v.as_str()).map(SmolStr::from),
        keywords: parse_str_vec("keywords"),
        skills,
        capabilities: parse_str_vec("capabilities"),
        hashtags: parse_str_vec("hashtags"),
        used_by: parse_str_vec("used_by"),
        members,
        equivalents: parse_str_vec("equivalents"),
        capability_eval,
    })
}

pub fn load_guidance(path: &Path) -> Result<Option<GuidanceDoc>, JsonError> {
    let content = std::fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let v: serde_json::Value = serde_json::from_str(&content)?;
    Ok(load_guidance_from_value(&v))
}

pub fn save_guidance(path: &Path, doc: &GuidanceDoc) -> Result<(), JsonError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json_str = super::json_writer::doc_to_json_string(doc);
    std::fs::write(path, json_str)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use guidance_common::types::{GuidanceDoc, Member, MemberType, Meta};

    #[test]
    fn test_load_guidance_minimal() {
        let json = r#"{
            "meta": {
                "module": "test",
                "source": "src/test.zig",
                "language": "zig"
            },
            "comment": "A test module"
        }"#;
        let v: serde_json::Value = serde_json::from_str(json).expect("valid json");
        let doc = load_guidance_from_value(&v).expect("should load");
        assert_eq!(doc.meta.module.as_str(), "test");
        assert_eq!(doc.comment.as_ref().map(|c| c.as_str()), Some("A test module"));
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.json");

        let doc = GuidanceDoc {
            meta: Meta {
                module: "roundtrip".into(),
                source: "src/rt.zig".into(),
                language: "zig".into(),
            },
            comment: Some("Round trip test".into()),
            members: vec![Member {
                type_name: MemberType::FnDecl,
                name: "foo".into(),
                signature: Some("fn foo() void".into()),
                is_pub: true,
                ..Member::default()
            }],
            ..GuidanceDoc::default()
        };

        save_guidance(&path, &doc).expect("should save");
        let loaded = load_guidance(&path).expect("should load").expect("should be Some");
        assert_eq!(loaded.meta.module.as_str(), "roundtrip");
        assert_eq!(loaded.members.len(), 1);
        assert_eq!(loaded.members[0].name.as_str(), "foo");
    }
}
