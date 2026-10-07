//! Conservative, local-only Terraform/HCL block and reference extraction.
//!
//! This scanner intentionally recognizes a documented HCL subset rather than
//! pretending to implement all HCL syntax. Unknown or malformed constructs are
//! returned as diagnostics so callers can surface coverage gaps.
use hilo_metadata::inventory::Edge;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerraformEntity {
    pub kind: String,
    pub address: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerraformDiagnostic {
    pub file: String,
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerraformParse {
    pub entities: Vec<TerraformEntity>,
    pub edges: Vec<Edge>,
    pub diagnostics: Vec<TerraformDiagnostic>,
}

/// Create a deployment edge only when the caller supplies explicit evidence
/// (for example, a user mapping from an Express environment key to a module).
pub fn explicit_deployment_link(
    application_surface: &str,
    terraform_address: &str,
    evidence: &str,
) -> Result<Edge, &'static str> {
    if application_surface.trim().is_empty() || terraform_address.trim().is_empty() {
        return Err("application surface and Terraform address are required");
    }
    if evidence.trim().is_empty() {
        return Err("deployment links require explicit evidence");
    }
    let target = if terraform_address.starts_with("terraform:") {
        terraform_address.to_string()
    } else {
        format!("terraform:{terraform_address}")
    };
    Ok(Edge::with_provenance(
        application_surface,
        target,
        "application_deploys",
        format!("user_mapping:{}", evidence.trim()),
        1.0,
    ))
}

/// Parse a root-module file. Use [`parse_in_module`] when the caller has
/// resolved this file from a parent module's explicit `source` attribute.
pub fn parse(file: &str, source: &str) -> TerraformParse {
    parse_in_module(file, source, "")
}

/// Parse a module file with its evidence-derived canonical module address.
/// The address must come from a Terraform `module.source` resolution, not a
/// directory/name heuristic.
pub fn parse_in_module(file: &str, source: &str, module_address: &str) -> TerraformParse {
    let mut result = TerraformParse::default();
    let base_module = module_address.to_string();
    let mut current_module = base_module.clone();
    let lines: Vec<&str> = source.lines().collect();
    let mut index = 0;
    let tf_json = file.ends_with(".tf.json");
    if tf_json {
        result.diagnostics.push(TerraformDiagnostic {
            file: file.into(),
            line: 1,
            message: "Terraform JSON syntax is recognized but semantic decoding is unsupported"
                .into(),
        });
    }
    while index < lines.len() && !tf_json {
        let line = lines[index];
        let trimmed = strip_comment(line).trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }
        let Some((kind, labels)) = block_header(trimmed) else {
            if trimmed.contains('{') && !trimmed.starts_with("#") {
                result.diagnostics.push(TerraformDiagnostic {
                    file: file.into(),
                    line: index + 1,
                    message: "unsupported or malformed block header".into(),
                });
            }
            index += 1;
            continue;
        };
        let (address, _label) = match kind.as_str() {
            "provider" | "module" | "resource" | "data" | "variable" | "output" => {
                if labels.is_empty() {
                    result.diagnostics.push(TerraformDiagnostic {
                        file: file.into(),
                        line: index + 1,
                        message: format!("{kind} block requires a label"),
                    });
                    index += 1;
                    continue;
                }
                let name = labels.last().cloned().unwrap_or_default();
                let addr = match kind.as_str() {
                    "resource" | "data" if labels.len() >= 2 => format!(
                        "{}{}.{}",
                        if current_module.is_empty() {
                            String::new()
                        } else {
                            format!("{current_module}.")
                        },
                        kind,
                        labels.join(".")
                    ),
                    "module" => {
                        let module = if current_module.is_empty() {
                            format!("module.{name}")
                        } else {
                            format!("{current_module}.module.{name}")
                        };
                        module
                    }
                    _ => format!(
                        "{}{}.{name}",
                        if current_module.is_empty() {
                            String::new()
                        } else {
                            format!("{current_module}.")
                        },
                        kind
                    ),
                };
                (addr, name)
            }
            "locals" => (
                format!(
                    "{}locals",
                    if current_module.is_empty() {
                        String::new()
                    } else {
                        format!("{current_module}.")
                    }
                ),
                String::new(),
            ),
            _ => {
                result.diagnostics.push(TerraformDiagnostic {
                    file: file.into(),
                    line: index + 1,
                    message: format!("unsupported HCL block type `{kind}`"),
                });
                index += 1;
                continue;
            }
        };
        let entity_kind = if kind == "locals" {
            "local"
        } else {
            kind.as_str()
        };
        let body_start = index;
        let mut depth =
            trimmed.matches('{').count() as isize - trimmed.matches('}').count() as isize;
        index += 1;
        while index < lines.len() && depth > 0 {
            let s = strip_comment(lines[index]);
            depth += s.matches('{').count() as isize - s.matches('}').count() as isize;
            index += 1;
        }
        if depth != 0 {
            result.diagnostics.push(TerraformDiagnostic {
                file: file.into(),
                line: body_start + 1,
                message: format!("unterminated `{kind}` block"),
            });
        }
        let body = &lines[body_start..index.min(lines.len())];
        if kind == "module" {
            current_module = address.clone();
        }
        if kind == "locals" {
            for (offset, body_line) in body.iter().enumerate().skip(1) {
                if let Some((name, _)) = body_line.split_once('=') {
                    let name = name.trim();
                    if valid_ident(name) {
                        result.entities.push(TerraformEntity {
                            kind: "local".into(),
                            address: format!(
                                "{}local.{name}",
                                if current_module.is_empty() {
                                    String::new()
                                } else {
                                    format!("{current_module}.")
                                }
                            ),
                            file: file.into(),
                            line: body_start + offset + 1,
                        });
                    }
                }
            }
        } else {
            result.entities.push(TerraformEntity {
                kind: entity_kind.into(),
                address: address.clone(),
                file: file.into(),
                line: body_start + 1,
            });
        }
        let origin = format!("terraform:{address}");
        let body_text = body.join("\n");
        if kind == "module" {
            if let Some(source_value) = attribute_value(&body_text, "source") {
                result.edges.push(edge(
                    &origin,
                    &format!("terraform:module-source:{source_value}"),
                    "module_source",
                ));
            }
        }
        for (offset, body_line) in body.iter().enumerate() {
            let s = strip_comment(body_line).trim();
            if s.contains("<<") || s.contains("%{") || s.starts_with("dynamic ") {
                result.diagnostics.push(TerraformDiagnostic {
                    file: file.into(),
                    line: body_start + offset + 1,
                    message: "unsupported heredoc, template directive, or dynamic block".into(),
                });
            }
            let line_origin = if kind == "locals" {
                s.split_once('=')
                    .map(|(name, _)| {
                        format!(
                            "terraform:{}local.{}",
                            if current_module.is_empty() {
                                String::new()
                            } else {
                                format!("{current_module}.")
                            },
                            name.trim()
                        )
                    })
                    .unwrap_or_else(|| origin.clone())
            } else {
                origin.clone()
            };
            if s.starts_with("depends_on") {
                for reference in references(s) {
                    result.edges.push(edge(
                        &line_origin,
                        &format!("terraform:{}", canonical_ref(&current_module, &reference)),
                        "depends_on",
                    ));
                }
            }
            for reference in references(s) {
                let target = format!("terraform:{}", canonical_ref(&current_module, &reference));
                let rel = if reference.starts_with("var.") || reference.starts_with("local.") {
                    "terraform_value_ref"
                } else if reference.starts_with("module.") {
                    "module_output_ref"
                } else if reference.starts_with("data.") {
                    "terraform_data_ref"
                } else {
                    "terraform_resource_ref"
                };
                if kind == "module" && reference.starts_with("var.") {
                    result.edges.push(edge(
                        &format!(
                            "terraform:{current_module}.input.{}",
                            reference.trim_start_matches("var.")
                        ),
                        &origin,
                        "module_input",
                    ));
                } else if kind == "output" && reference.starts_with("module.") {
                    result.edges.push(edge(&origin, &target, "module_output"));
                } else if !s.starts_with("depends_on") {
                    result.edges.push(edge(&line_origin, &target, rel));
                }
            }
        }
        if kind == "module" {
            current_module = base_module.clone();
        }
    }
    // Index every recognized Terraform file and declaration even when a
    // declaration has no references. This makes infrastructure visible to
    // graph inventories and component rollups without inventing relationships.
    result.edges.push(edge(
        file,
        &format!("terraform:file:{file}"),
        "infrastructure_surface",
    ));
    result.edges.push(Edge::with_provenance(
        file,
        format!("unknown:deployment-link:{file}:no_explicit_application_mapping"),
        "deployment_link_unknown",
        "unresolved",
        0.0,
    ));
    for entity in &result.entities {
        let target = format!("terraform:{}", entity.address);
        if !base_module.is_empty() {
            result.edges.push(edge(
                &format!("terraform:{base_module}"),
                &target,
                "module_contains",
            ));
        }
        result
            .edges
            .push(edge(&entity.file, &target, "terraform_declares"));
    }
    // Preserve diagnostics in the graph itself: a parser caller that only
    // consumes Edge values must not silently erase unsupported syntax.
    for diagnostic in &result.diagnostics {
        let reason = diagnostic.message.replace([' ', '/', ':'], "_");
        result.edges.push(Edge::with_provenance(
            &diagnostic.file,
            format!("unknown:terraform:{}:{reason}", diagnostic.line),
            "parse_coverage_gap",
            "unresolved",
            0.0,
        ));
    }
    result
        .edges
        .sort_by(|a, b| (&a.from, &a.to, &a.rel).cmp(&(&b.from, &b.to, &b.rel)));
    result
        .edges
        .dedup_by(|a, b| a.from == b.from && a.to == b.to && a.rel == b.rel);
    result
}

fn edge(from: &str, to: &str, rel: &str) -> Edge {
    Edge::with_provenance(from, to, rel, "ast_exact", 1.0)
}
fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
fn strip_comment(s: &str) -> &str {
    s.split('#')
        .next()
        .unwrap_or(s)
        .split("//")
        .next()
        .unwrap_or(s)
}
fn block_header(s: &str) -> Option<(String, Vec<String>)> {
    if !s.ends_with('{') {
        return None;
    }
    let head = s.trim_end_matches('{').trim();
    let mut parts = head.split_whitespace();
    let kind = parts.next()?.to_string();
    let labels = parts.map(|p| p.trim_matches('"').to_string()).collect();
    Some((kind, labels))
}
fn attribute_value(body: &str, name: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == name).then(|| value.trim().trim_matches('"').to_string())
    })
}
fn references(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || b"_-.[]\"".contains(&bytes[i]))
            {
                i += 1;
            }
            let token = s[start..i]
                .trim_matches('"')
                .trim_end_matches([',', ']', ')', '}'])
                .to_string();
            if token.starts_with("var.")
                || token.starts_with("local.")
                || token.starts_with("module.")
                || token.split('.').count() >= 2
                    && token.contains('.')
                    && token
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_alphabetic)
                    && ![
                        "resource",
                        "depends_on",
                        "source",
                        "name",
                        "tags",
                        "count",
                        "for_each",
                        "output",
                        "variable",
                        "locals",
                        "provider",
                        "data",
                        "terraform",
                    ]
                    .contains(&token.as_str())
            {
                out.push(token);
            }
        } else {
            i += 1;
        }
    }
    out.sort();
    out.dedup();
    out
}
fn canonical_ref(module: &str, reference: &str) -> String {
    let reference = reference.trim_start_matches("${").trim_end_matches('}');
    let (namespace, suffix) = if let Some(name) = reference.strip_prefix("var.") {
        ("variable", name)
    } else if let Some(name) = reference.strip_prefix("local.") {
        ("local", name)
    } else if reference.starts_with("module.") || reference.starts_with("data.") {
        ("", reference)
    } else {
        ("resource", reference)
    };
    let canonical = if namespace.is_empty() {
        suffix.to_string()
    } else {
        format!("{namespace}.{suffix}")
    };
    if module.is_empty() {
        canonical
    } else {
        format!("{module}.{canonical}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_typed_entities_refs_and_locations() {
        let parsed = parse("main.tf", "provider \"aws\" {\n region = \"us-east-1\"\n}\nvariable \"image\" {\n default = \"x\"\n}\nmodule \"app\" {\n source = \"./modules/app\"\n image = var.image\n}\nresource \"aws_instance\" \"web\" {\n ami = var.image\n depends_on = [module.app] \n}\noutput \"id\" {\n value = aws_instance.web.id\n}\n");
        let addresses: Vec<_> = parsed.entities.iter().map(|e| e.address.as_str()).collect();
        assert_eq!(
            addresses,
            [
                "provider.aws",
                "variable.image",
                "module.app",
                "resource.aws_instance.web",
                "output.id"
            ]
        );
        assert_eq!(parsed.entities[0].line, 1);
        assert!(parsed
            .edges
            .iter()
            .any(|e| e.rel == "module_source" && e.to == "terraform:module-source:./modules/app"));
        assert!(parsed
            .edges
            .iter()
            .any(|e| e.rel == "terraform_value_ref" && e.to == "terraform:variable.image"));
        assert!(parsed
            .edges
            .iter()
            .any(|e| e.rel == "depends_on" && e.to == "terraform:module.app"));
        assert!(parsed
            .edges
            .iter()
            .any(|e| e.rel == "terraform_resource_ref"
                && e.to == "terraform:resource.aws_instance.web.id"));
    }
    #[test]
    fn nested_module_addresses_and_data_references_are_typed() {
        let root = parse(
            "main.tf",
            r#"module "api" {
 source = "./modules/api"
}
"#,
        );
        assert_eq!(root.entities[0].address, "module.api");
        assert_eq!(root.entities[0].file, "main.tf");
        assert_eq!(root.entities[0].line, 1);
        assert!(root.edges.iter().any(|e| {
            e.rel == "module_source" && e.to == "terraform:module-source:./modules/api"
        }));
        assert!(root.edges.iter().any(|e| e.rel == "infrastructure_surface"));
        assert!(root.edges.iter().any(|e| e.rel == "terraform_declares"));

        let nested = parse_in_module(
            "modules/api/main.tf",
            r#"data "aws_ami" "base" {
 most_recent = true
}
locals {
 image_id = data.aws_ami.base.id
}
resource "aws_instance" "web" {
 ami = local.image_id
}
output "instance_id" {
 value = aws_instance.web.id
}
"#,
            "module.api",
        );
        let addresses: Vec<_> = nested.entities.iter().map(|e| e.address.as_str()).collect();
        assert_eq!(
            addresses,
            [
                "module.api.data.aws_ami.base",
                "module.api.local.image_id",
                "module.api.resource.aws_instance.web",
                "module.api.output.instance_id",
            ]
        );
        assert!(nested.edges.iter().any(|e| {
            e.from == "terraform:module.api.local.image_id"
                && e.to == "terraform:module.api.data.aws_ami.base.id"
                && e.rel == "terraform_data_ref"
        }));
        assert!(nested.edges.iter().any(|e| {
            e.to == "terraform:module.api.local.image_id" && e.rel == "terraform_value_ref"
        }));
        assert!(nested.edges.iter().any(|e| {
            e.from == "terraform:module.api.output.instance_id"
                && e.to == "terraform:module.api.resource.aws_instance.web.id"
                && e.rel == "terraform_resource_ref"
        }));
    }

    #[test]
    fn express_to_terraform_path_requires_explicit_mapping_and_reverses() {
        use crate::{Direction, GraphDB};
        use std::collections::BTreeSet;

        let root = parse(
            "infra/main.tf",
            r#"module "api" {
 source = "./modules/api"
}
"#,
        );
        let nested = parse_in_module(
            "infra/modules/api/main.tf",
            r#"resource "aws_apigatewayv2_api" "http" {
 name = "service-api"
}
resource "aws_instance" "compute" {
 depends_on = [aws_apigatewayv2_api.http]
}
output "compute_id" {
 value = aws_instance.compute.id
}
"#,
            "module.api",
        );
        let mapping = explicit_deployment_link(
            "src/server.js",
            "module.api",
            "user map: EXPRESS_API_BASE_URL targets the api module",
        )
        .unwrap();
        let mut edges = root.edges;
        edges.extend(nested.edges);
        edges.push(mapping);

        let node_count = edges
            .iter()
            .flat_map(|e| [e.from.as_str(), e.to.as_str()])
            .collect::<BTreeSet<_>>()
            .len();
        assert_eq!(edges.len(), 15, "fixture edge inventory changed");
        assert_eq!(node_count, 13, "fixture node inventory changed");
        let graph = GraphDB::open(":memory:").unwrap();
        graph.insert_edges(&edges).unwrap();
        let stats = graph.stats().unwrap();
        assert!(stats
            .components
            .iter()
            .any(|component| component.name == "infra"));
        let deployment = "terraform:module.api.resource.aws_instance.compute";
        let edge_set: BTreeSet<_> = edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.rel.as_str()))
            .collect();
        assert!(edge_set.contains(&(
            "src/server.js",
            "terraform:module.api",
            "application_deploys"
        )));
        assert!(edge_set.contains(&("terraform:module.api", deployment, "module_contains")));
        assert!(edge_set.contains(&(
            deployment,
            "terraform:module.api.resource.aws_apigatewayv2_api.http",
            "depends_on"
        )));
        let forward = graph
            .related("src/server.js", None, Direction::Forward)
            .unwrap();
        assert!(forward.iter().any(|e| e.to == "terraform:module.api"));
        let reverse = graph.impact_or_parse(deployment, 4).unwrap();
        assert!(reverse.iter().any(|row| row.path == "src/server.js"));
        assert!(edges.iter().any(|e| {
            e.rel == "deployment_link_unknown" && e.to.contains("no_explicit_application_mapping")
        }));
        assert!(explicit_deployment_link("src/server.js", "module.api", " ").is_err());
    }

    #[test]
    fn diagnostics_become_unresolved_coverage_gap_edges() {
        let parsed = parse("bad.tf", "resource {\\n value = var.x\\n}\\n");
        assert!(!parsed.diagnostics.is_empty());
        let gap = parsed
            .edges
            .iter()
            .find(|edge| edge.rel == "parse_coverage_gap")
            .unwrap();
        assert_eq!(gap.from, "bad.tf");
        assert!(gap.to.starts_with("unknown:terraform:1:"));
        assert_eq!(gap.provenance, "unresolved");
        assert_eq!(gap.confidence, 0.0);

        let json = parse("main.tf.json", r#"{"resource": {"aws_instance": {}}}"#);
        assert!(json
            .diagnostics
            .iter()
            .any(|d| d.message.contains("JSON syntax")));
        assert!(json
            .edges
            .iter()
            .any(|edge| edge.rel == "parse_coverage_gap"));
        assert!(json
            .edges
            .iter()
            .any(|edge| edge.rel == "infrastructure_surface"));
    }

    #[test]
    fn malformed_and_unknown_constructs_are_gaps() {
        let parsed = parse("bad.tf", "resource {\n x = 1\n}\nweird \"x\" {\n y = true\n}\nresource \"aws_x\" \"y\" {\n z = 1\n");
        assert!(!parsed.diagnostics.is_empty());
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("requires a label")));
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("unsupported")));
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("unterminated")));
    }
}
