//! Lossless-enough `package.json` dependency editing primitives.
use serde_json::Value;
use std::{fmt, fs, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyType {
    Dependencies,
    DevDependencies,
    PeerDependencies,
    OptionalDependencies,
    BundledDependencies,
}
impl DependencyType {
    fn key(self) -> &'static str {
        match self {
            Self::Dependencies => "dependencies",
            Self::DevDependencies => "devDependencies",
            Self::PeerDependencies => "peerDependencies",
            Self::OptionalDependencies => "optionalDependencies",
            Self::BundledDependencies => "bundledDependencies",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    Prefix,
    Exact,
    Preserve,
}
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("{path}: invalid JSON at line {line}: {message}")]
    Json {
        path: String,
        line: usize,
        message: String,
    },
    #[error("{path}: invalid package name `{name}` (key `{key}`)")]
    InvalidName {
        path: String,
        key: String,
        name: String,
    },
    #[error("{path}: malformed range `{range}` for package `{name}` (key `{key}`)")]
    InvalidRange {
        path: String,
        key: String,
        name: String,
        range: String,
    },
    #[error("{path}: dependency field `{field}` is not an object")]
    InvalidField { path: String, field: String },
    #[error("{path}: dependency field `{field}` not found")]
    MissingField { path: String, field: String },
    #[error("{path}: cannot edit dependency field `{field}`")]
    Edit { path: String, field: String },
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}
#[derive(Debug, Clone)]
pub struct Manifest {
    pub path: String,
    source: String,
    pub value: Value,
}
impl Manifest {
    pub fn parse(
        path: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<Self, ManifestError> {
        let path = path.into();
        let source = source.into();
        let value = serde_json::from_str(&source).map_err(|e| ManifestError::Json {
            path: path.clone(),
            line: e.line(),
            message: e.to_string(),
        })?;
        Ok(Self {
            path,
            source,
            value,
        })
    }
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let p = path.as_ref();
        Self::parse(p.display().to_string(), fs::read_to_string(p)?)
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn dependency(&self, kind: DependencyType, name: &str) -> Option<&str> {
        self.value.get(kind.key())?.get(name)?.as_str()
    }
    pub fn validate_dependencies(&self) -> Result<(), ManifestError> {
        for kind in [
            DependencyType::Dependencies,
            DependencyType::DevDependencies,
            DependencyType::PeerDependencies,
            DependencyType::OptionalDependencies,
        ] {
            if let Some(v) = self.value.get(kind.key()) {
                let Some(m) = v.as_object() else {
                    return Err(ManifestError::InvalidField {
                        path: self.path.clone(),
                        field: kind.key().into(),
                    });
                };
                for (n, r) in m {
                    validate_name(&self.path, kind.key(), n)?;
                    if !valid_range(r.as_str().unwrap_or("")) {
                        return Err(ManifestError::InvalidRange {
                            path: self.path.clone(),
                            key: kind.key().into(),
                            name: n.clone(),
                            range: r.as_str().unwrap_or("").into(),
                        });
                    }
                }
            }
        }
        Ok(())
    }
    pub fn add_dependency(
        &mut self,
        kind: DependencyType,
        name: &str,
        version: &str,
        mode: SaveMode,
        prefix: char,
    ) -> Result<(), ManifestError> {
        validate_name(&self.path, kind.key(), name)?;
        if !self.value.is_object() {
            return Err(ManifestError::Edit {
                path: self.path.clone(),
                field: kind.key().into(),
            });
        }
        if self
            .value
            .get(kind.key())
            .is_some_and(|value| !value.is_object())
        {
            return Err(ManifestError::InvalidField {
                path: self.path.clone(),
                field: kind.key().into(),
            });
        }
        let range = match mode {
            SaveMode::Exact => exact(version),
            SaveMode::Preserve => version.to_owned(),
            SaveMode::Prefix => {
                if version.starts_with('^') || version.starts_with('~') {
                    version.into()
                } else {
                    format!("{prefix}{version}")
                }
            }
        };
        if !valid_range(&range) {
            return Err(ManifestError::InvalidRange {
                path: self.path.clone(),
                key: kind.key().into(),
                name: name.into(),
                range,
            });
        }
        let mut obj = self.value.as_object().cloned().unwrap_or_default();
        let mut deps = obj
            .remove(kind.key())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        deps.insert(name.into(), Value::String(range.clone()));
        obj.insert(kind.key().into(), Value::Object(deps));
        let source =
            edit_dependency_object(&self.source, kind.key(), name, &range).ok_or_else(|| {
                ManifestError::Edit {
                    path: self.path.clone(),
                    field: kind.key().into(),
                }
            })?;
        self.value = Value::Object(obj);
        self.source = source;
        Ok(())
    }
    pub fn remove_dependency(
        &mut self,
        kind: DependencyType,
        name: &str,
    ) -> Result<bool, ManifestError> {
        if self.dependency(kind, name).is_none() {
            return Ok(false);
        };
        let Some(s) = remove_member(&self.source, kind.key(), name) else {
            return Err(ManifestError::Edit {
                path: self.path.clone(),
                field: kind.key().into(),
            });
        };
        self.source = s;
        if let Some(m) = self
            .value
            .get_mut(kind.key())
            .and_then(Value::as_object_mut)
        {
            m.remove(name);
        }
        Ok(true)
    }
}
fn exact(v: &str) -> String {
    v.strip_prefix('^')
        .or_else(|| v.strip_prefix('~'))
        .unwrap_or(v)
        .to_string()
}
fn validate_name(path: &str, key: &str, n: &str) -> Result<(), ManifestError> {
    if n.is_empty()
        || n.chars().any(|c| {
            c.is_whitespace() || c == '/' && n.starts_with('/') || c == '@' && n.len() == 1
        })
        || (!n.starts_with('@') && n.contains('@'))
    {
        return Err(ManifestError::InvalidName {
            path: path.into(),
            key: key.into(),
            name: n.into(),
        });
    }
    Ok(())
}
fn valid_range(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && !s.chars().any(|c| c == '\n' || c == '\r' || c == '\t')
}
fn root_object_bounds(src: &str) -> Option<(usize, usize)> {
    let open = src.find('{')?;
    let end = skip_json_value(src, open)?;
    Some((open, end))
}
fn skip_json_string(src: &str, start: usize) -> Option<usize> {
    if src.as_bytes().get(start) != Some(&b'"') {
        return None;
    }
    let mut index = start + 1;
    while index < src.len() {
        match src.as_bytes()[index] {
            b'\\' => index = index.checked_add(2)?,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}
fn skip_json_value(src: &str, start: usize) -> Option<usize> {
    let first = *src.as_bytes().get(start)?;
    if first == b'"' {
        return skip_json_string(src, start);
    }
    if matches!(first, b'{' | b'[') {
        let mut expected = vec![if first == b'{' { b'}' } else { b']' }];
        let mut index = start + 1;
        while index < src.len() {
            match src.as_bytes()[index] {
                b'"' => index = skip_json_string(src, index)?,
                b'{' => {
                    expected.push(b'}');
                    index += 1;
                }
                b'[' => {
                    expected.push(b']');
                    index += 1;
                }
                b'}' | b']' => {
                    if expected.pop()? != src.as_bytes()[index] {
                        return None;
                    }
                    index += 1;
                    if expected.is_empty() {
                        return Some(index);
                    }
                }
                _ => index += 1,
            }
        }
        return None;
    }
    let mut index = start;
    while index < src.len()
        && !src.as_bytes()[index].is_ascii_whitespace()
        && !matches!(src.as_bytes()[index], b',' | b'}' | b']')
    {
        index += 1;
    }
    (index > start).then_some(index)
}
fn find_object(src: &str, key: &str) -> Option<(usize, usize)> {
    let (root_open, root_end) = root_object_bounds(src)?;
    let mut index = root_open + 1;
    while index < root_end - 1 {
        while src
            .as_bytes()
            .get(index)
            .is_some_and(u8::is_ascii_whitespace)
            || src.as_bytes().get(index) == Some(&b',')
        {
            index += 1;
        }
        if index >= root_end - 1 {
            break;
        }
        let key_end = skip_json_string(src, index)?;
        let member: String = serde_json::from_str(&src[index..key_end]).ok()?;
        index = key_end;
        while src
            .as_bytes()
            .get(index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            index += 1;
        }
        if src.as_bytes().get(index) != Some(&b':') {
            return None;
        }
        index += 1;
        while src
            .as_bytes()
            .get(index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            index += 1;
        }
        let value_start = index;
        let value_end = skip_json_value(src, value_start)?;
        if member == key && src.as_bytes().get(value_start) == Some(&b'{') {
            return Some((value_start, value_end));
        }
        index = value_end;
        while src
            .as_bytes()
            .get(index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            index += 1;
        }
        if src.as_bytes().get(index) == Some(&b',') {
            index += 1;
        } else if index < root_end - 1 {
            return None;
        }
    }
    None
}
fn edit_dependency_object(src: &str, key: &str, name: &str, range: &str) -> Option<String> {
    let Some((a, b)) = find_object(src, key) else {
        return insert_dependency_field(src, key, name, range);
    };
    let obj = &src[a..b];
    let q = format!("\"{name}\"");
    if let Some(k) = obj.find(&q) {
        let colon = obj[k..].find(':')? + k;
        let val_start = colon + 1;
        let mut value_start = val_start;
        while value_start < obj.len() && obj.as_bytes()[value_start].is_ascii_whitespace() {
            value_start += 1
        }
        let existing_spacing = &obj[val_start..value_start];
        let mut end = value_start;
        if obj.as_bytes().get(value_start) == Some(&b'"') {
            end = value_start + 1;
            while end < obj.len() {
                if obj.as_bytes()[end] == b'"' && obj.as_bytes()[end - 1] != b'\\' {
                    end += 1;
                    break;
                }
                end += 1
            }
        } else {
            while end < obj.len() && !b",}".contains(&obj.as_bytes()[end]) {
                end += 1
            }
        }
        let mut out = src.to_string();
        let encoded_range = serde_json::to_string(range).ok()?;
        out.replace_range(
            a + val_start..a + end,
            &format!("{existing_spacing}{encoded_range}"),
        );
        return Some(out);
    }
    let parent_indent = src[..a]
        .rsplit_once('\n')
        .map(|(_, line)| line.chars().take_while(|c| c.is_whitespace()).count())
        .unwrap_or(0);
    let pad = " ".repeat(parent_indent + 2);
    let parent_pad = " ".repeat(parent_indent);
    let inner = obj[1..obj.len() - 1].trim();
    let mut out = src.to_string();
    if inner.is_empty() {
        out.replace_range(
            a + 1..b - 1,
            &format!("\n{pad}\"{name}\": \"{range}\"\n{parent_pad}"),
        );
        return Some(out);
    }
    let mut insertion = b - 1;
    while insertion > a + 1 && src.as_bytes()[insertion - 1].is_ascii_whitespace() {
        insertion -= 1;
    }
    out.insert_str(insertion, &format!(",\n{pad}\"{name}\": \"{range}\""));
    Some(out)
}
fn insert_dependency_field(src: &str, key: &str, name: &str, range: &str) -> Option<String> {
    let (open, close) = root_object_bounds(src)?;
    let close = close - 1;
    if open >= close {
        return None;
    }
    let content = &src[open + 1..close];
    let trimmed = content.trim_end();
    let insertion_point = open + 1 + trimmed.len();
    let has_fields = !content.trim().is_empty();
    let root_indent = src[..open]
        .rsplit_once('\n')
        .map(|(_, line)| line.chars().take_while(|c| c.is_whitespace()).count())
        .unwrap_or(0);
    let property_indent = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().take_while(|c| c.is_whitespace()).count())
        .filter(|indent| *indent > root_indent)
        .min()
        .unwrap_or(root_indent + 2);
    let newline = if src.contains("\r\n") {
        "\r\n"
    } else if src.contains('\n') {
        "\n"
    } else {
        ""
    };
    let quoted_key = serde_json::to_string(key).ok()?;
    let quoted_name = serde_json::to_string(name).ok()?;
    let quoted_range = serde_json::to_string(range).ok()?;
    let comma = if has_fields { "," } else { "" };
    let insertion = if newline.is_empty() {
        format!("{comma}{quoted_key}:{{{quoted_name}:{quoted_range}}}")
    } else {
        let property_pad = " ".repeat(property_indent);
        let item_pad = " ".repeat(property_indent + 2);
        let root_pad = " ".repeat(root_indent);
        format!(
            "{comma}{newline}{property_pad}{quoted_key}: {{{newline}{item_pad}{quoted_name}: {quoted_range}{newline}{property_pad}}}{newline}{root_pad}"
        )
    };
    let mut output = src[..insertion_point].to_owned();
    output.push_str(&insertion);
    output.push_str(&src[close..]);
    Some(output)
}
fn remove_member(src: &str, key: &str, name: &str) -> Option<String> {
    let (a, b) = find_object(src, key)?;
    let obj = &src[a..b];
    let q = format!("\"{name}\"");
    let k = obj.find(&q)?;
    let mut st = k;
    while st > a && src.as_bytes()[st - 1].is_ascii_whitespace() {
        st -= 1
    }
    let mut en = obj[k..]
        .find(',')
        .map(|x| k + x + 1)
        .unwrap_or_else(|| obj[k..].find('}').unwrap_or(obj.len() - 1) + k);
    if en == obj.len() {
        en -= 1
    }
    let mut out = src.to_string();
    out.replace_range(a + st..a + en, "");
    Some(out)
}
impl fmt::Display for DependencyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_newline_and_order() {
        let s = "{\n  \"name\": \"x\",\n  \"dependencies\": {\n    \"a\": \"^1.0.0\"\n  }\n}\n";
        let mut m = Manifest::parse("package.json", s).unwrap();
        m.add_dependency(
            DependencyType::Dependencies,
            "b",
            "2.0.0",
            SaveMode::Exact,
            '^',
        )
        .unwrap();
        assert!(m.source.ends_with("\n"));
        assert!(m.source.find("a").unwrap() < m.source.find("b").unwrap());
        assert_eq!(
            m.source,
            "{\n  \"name\": \"x\",\n  \"dependencies\": {\n    \"a\": \"^1.0.0\",\n    \"b\": \"2.0.0\"\n  }\n}\n"
        );
        let parsed: Value = serde_json::from_str(&m.source).unwrap();
        assert_eq!(parsed["dependencies"]["b"], "2.0.0");
    }

    #[test]
    fn updating_existing_range_preserves_compact_json_spacing() {
        let source =
            "{\"name\":\"x\",\"dependencies\":{\"a\":\"^1.0.0\"},\"description\":\"kept\"}\n";
        let mut manifest = Manifest::parse("package.json", source).unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "a",
                "^2.0.0",
                SaveMode::Preserve,
                '^',
            )
            .unwrap();
        assert_eq!(manifest.source(), source.replace("^1.0.0", "^2.0.0"));
    }

    #[test]
    fn preserves_other_supported_manifest_fields() {
        let source = r#"{
  "name": "x",
  "scripts": {"build": "node build.js"},
  "bin": {"x": "./bin/x.js"},
  "engines": {"node": ">=20"},
  "os": ["linux"],
  "cpu": ["x64"],
  "workspaces": ["packages/*"],
  "overrides": {"dep": "1.2.3"},
  "exports": {".": "./index.js"},
  "files": ["dist"],
  "dependencies": {"a": "^1.0.0"}
}
"#;
        let mut manifest = Manifest::parse("package.json", source).unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "b",
                "2.0.0",
                SaveMode::Exact,
                '^',
            )
            .unwrap();
        let parsed: Value = serde_json::from_str(&manifest.source).unwrap();
        assert_eq!(parsed["scripts"]["build"], "node build.js");
        assert_eq!(parsed["bin"]["x"], "./bin/x.js");
        assert_eq!(parsed["engines"]["node"], ">=20");
        assert_eq!(parsed["os"][0], "linux");
        assert_eq!(parsed["cpu"][0], "x64");
        assert_eq!(parsed["workspaces"][0], "packages/*");
        assert_eq!(parsed["overrides"]["dep"], "1.2.3");
        assert_eq!(parsed["exports"]["."], "./index.js");
        assert_eq!(parsed["files"][0], "dist");
    }

    #[test]
    fn dependency_edit_targets_only_the_root_object_member() {
        let source = r#"{
  "description": "literal } and \"dependencies\" text",
  "overrides": {"dependencies": {"a": "2.0.0"}},
  "dependencies": {"a": "1.0.0"}
}
"#;
        let mut manifest = Manifest::parse("package.json", source).unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "a",
                "3.0.0",
                SaveMode::Exact,
                '^',
            )
            .unwrap();
        let parsed: Value = serde_json::from_str(manifest.source()).unwrap();
        assert_eq!(parsed["dependencies"]["a"], "3.0.0");
        assert_eq!(parsed["overrides"]["dependencies"]["a"], "2.0.0");
        assert!(manifest.source().contains("literal } and"));
    }

    #[test]
    fn save_modes_prefix_exact_or_preserve_specs() {
        let mut manifest = Manifest::parse("package.json", "{\"dependencies\": {}}\n").unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "prefixed",
                "1.2.3",
                SaveMode::Prefix,
                '^',
            )
            .unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "exact",
                "^2.3.4",
                SaveMode::Exact,
                '^',
            )
            .unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "explicit-range",
                ">=3.0.0 <4.0.0",
                SaveMode::Preserve,
                '^',
            )
            .unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "tag",
                "latest",
                SaveMode::Preserve,
                '^',
            )
            .unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "wildcard",
                "*",
                SaveMode::Preserve,
                '^',
            )
            .unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "exact-comparator",
                ">=1.0.0 <2.0.0",
                SaveMode::Exact,
                '^',
            )
            .unwrap();

        assert_eq!(
            manifest.dependency(DependencyType::Dependencies, "prefixed"),
            Some("^1.2.3")
        );
        assert_eq!(
            manifest.dependency(DependencyType::Dependencies, "exact"),
            Some("2.3.4")
        );
        assert_eq!(
            manifest.dependency(DependencyType::Dependencies, "explicit-range"),
            Some(">=3.0.0 <4.0.0")
        );
        assert_eq!(
            manifest.dependency(DependencyType::Dependencies, "tag"),
            Some("latest")
        );
        assert_eq!(
            manifest.dependency(DependencyType::Dependencies, "wildcard"),
            Some("*")
        );
        assert_eq!(
            manifest.dependency(DependencyType::Dependencies, "exact-comparator"),
            Some(">=1.0.0 <2.0.0")
        );
    }

    #[test]
    fn inserts_first_dependency_section_without_reformatting_existing_fields() {
        let original =
            "{\n  \"name\": \"app\",\n  \"scripts\": {\n    \"test\": \"node test.js\"\n  }\n}\n";
        let mut manifest = Manifest::parse("package.json", original).unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "left-pad",
                "1.3.0",
                SaveMode::Exact,
                '^',
            )
            .unwrap();

        assert_eq!(
            manifest.source(),
            "{\n  \"name\": \"app\",\n  \"scripts\": {\n    \"test\": \"node test.js\"\n  },\n  \"dependencies\": {\n    \"left-pad\": \"1.3.0\"\n  }\n}\n"
        );
    }

    #[test]
    fn rejects_bad_name() {
        let m = Manifest::parse("p", "{}").unwrap();
        assert!(
            m.clone()
                .add_dependency(
                    DependencyType::Dependencies,
                    "bad name",
                    "1",
                    SaveMode::Exact,
                    '^'
                )
                .is_err()
        );
    }

    #[test]
    fn malformed_json_reports_path_line_and_parser_message() {
        let error = Manifest::parse(
            "/tmp/scoped-package/package.json",
            "{\n  \"name\": \"@scope/pkg\",\n  \"dependencies\": {\n",
        )
        .unwrap_err();
        match error {
            ManifestError::Json {
                path,
                line,
                message,
            } => {
                assert_eq!(path, "/tmp/scoped-package/package.json");
                assert_eq!(line, 4);
                assert!(!message.is_empty());
            }
            other => panic!("expected JSON diagnostic, got {other:?}"),
        }
    }

    #[test]
    fn diverse_real_manifest_shape_has_exact_minimal_diff() {
        // Redacted shape modeled on scoped and unscoped npm package manifests.
        let original = r#"{
  "name": "@acme/cli",
  "version": "3.2.1",
  "scripts": {"test": "node test.js", "build": "node build.js"},
  "bin": {"acme": "./bin/acme.js"},
  "engines": {"node": ">=18"},
  "os": ["linux", "darwin"],
  "cpu": ["x64", "arm64"],
  "libc": ["glibc"],
  "workspaces": ["packages/*"],
  "overrides": {"kleur": "^4.1.5"},
  "exports": {".": {"types": "./dist/index.d.ts", "default": "./dist/index.js"}},
  "main": "./dist/index.js",
  "files": ["dist", "bin"],
  "dependencies": {"kleur": "^4.1.5"},
  "devDependencies": {"ava": "^5.3.0"},
  "peerDependencies": {"node": ">=18"},
  "peerDependenciesMeta": {"node": {"optional": true}},
  "optionalDependencies": {"fsevents": "^2.3.3"},
  "bundledDependencies": ["embedded-addon"]
}
"#;
        let mut manifest = Manifest::parse("@acme-cli.package.json", original).unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "chalk",
                "5.3.0",
                SaveMode::Prefix,
                '^',
            )
            .unwrap();
        let expected = original.replace(
            "\"dependencies\": {\"kleur\": \"^4.1.5\"}",
            "\"dependencies\": {\"kleur\": \"^4.1.5\",\n    \"chalk\": \"^5.3.0\"}",
        );
        assert_eq!(manifest.source(), expected);
        let parsed: Value = serde_json::from_str(manifest.source()).unwrap();
        assert_eq!(parsed["name"], "@acme/cli");
        assert_eq!(parsed["exports"]["."]["types"], "./dist/index.d.ts");
        assert_eq!(
            manifest.dependency(DependencyType::Dependencies, "chalk"),
            Some("^5.3.0")
        );
    }

    #[test]
    fn dependency_type_flags_cover_all_supported_editable_fields() {
        let mut manifest = Manifest::parse("package.json", "{\n  \"name\": \"app\"\n}\n").unwrap();
        for (kind, name) in [
            (DependencyType::Dependencies, "prod"),
            (DependencyType::DevDependencies, "dev"),
            (DependencyType::PeerDependencies, "peer"),
            (DependencyType::OptionalDependencies, "optional"),
        ] {
            manifest
                .add_dependency(kind, name, "1.2.3", SaveMode::Exact, '^')
                .unwrap();
            assert_eq!(manifest.dependency(kind, name), Some("1.2.3"));
        }
        let value: Value = serde_json::from_str(manifest.source()).unwrap();
        assert_eq!(value["dependencies"]["prod"], "1.2.3");
        assert_eq!(value["devDependencies"]["dev"], "1.2.3");
        assert_eq!(value["peerDependencies"]["peer"], "1.2.3");
        assert_eq!(value["optionalDependencies"]["optional"], "1.2.3");
    }

    #[test]
    fn save_prefix_preserves_explicit_prefix_and_exact_strips_caret_or_tilde() {
        let mut manifest = Manifest::parse("package.json", "{\"dependencies\":{}}\n").unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "tilde",
                "~1.2.3",
                SaveMode::Prefix,
                '^',
            )
            .unwrap();
        manifest
            .add_dependency(
                DependencyType::Dependencies,
                "exact-tilde",
                "~2.0.0",
                SaveMode::Exact,
                '^',
            )
            .unwrap();
        assert_eq!(
            manifest.dependency(DependencyType::Dependencies, "tilde"),
            Some("~1.2.3")
        );
        assert_eq!(
            manifest.dependency(DependencyType::Dependencies, "exact-tilde"),
            Some("2.0.0")
        );
    }

    #[test]
    fn real_npm_manifest_corpus_preserves_existing_fields() {
        let fixtures = [
            include_str!("../tests/fixtures/real-manifests/inquirer-core.package.json"),
            include_str!("../tests/fixtures/real-manifests/ansi-regex.package.json"),
            include_str!("../tests/fixtures/real-manifests/commander.package.json"),
            include_str!("../tests/fixtures/real-manifests/debug.package.json"),
            include_str!("../tests/fixtures/real-manifests/ms.package.json"),
            include_str!("../tests/fixtures/real-manifests/semver.package.json"),
        ];

        for (index, source) in fixtures.iter().enumerate() {
            let mut manifest = Manifest::parse(format!("real-manifest-{index}"), *source)
                .unwrap_or_else(|error| panic!("fixture {index} did not parse: {error}"));
            let mut expected: Value = serde_json::from_str(source).unwrap();
            manifest
                .add_dependency(
                    DependencyType::Dependencies,
                    "jsm-phase1-manifest-probe",
                    "1.2.3",
                    SaveMode::Exact,
                    '^',
                )
                .unwrap_or_else(|error| panic!("fixture {index} edit failed: {error}"));

            let actual: Value = serde_json::from_str(manifest.source()).unwrap();
            expected
                .as_object_mut()
                .unwrap()
                .entry("dependencies")
                .or_insert_with(|| Value::Object(Default::default()))
                .as_object_mut()
                .unwrap()
                .insert(
                    "jsm-phase1-manifest-probe".into(),
                    Value::String("1.2.3".into()),
                );
            assert_eq!(
                actual, expected,
                "fixture {index} changed pre-existing data"
            );
        }
    }
}
