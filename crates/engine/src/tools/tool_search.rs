//! `tool_search` — discover and activate deferred tools.
//!
//! Deferred tools keep a large MCP catalog out of every prompt. The model
//! searches the catalog with this tool; the hits are activated for the session
//! and appear in the next request's declaration. The ranker is dependency-free
//! BM25F over weighted name, alias, schema-property, and description fields;
//! exact and phrase name matches keep an additive bonus.

use std::collections::HashMap;
use std::sync::Weak;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tool::{BuiltinTool, RiskProfile, Tool, ToolContext, ToolError, ToolOutput, ToolSchema};

use crate::Agent;

/// A ranked deferred tool.
pub(crate) struct DeferredHit {
    pub name: String,
    pub description: String,
    pub score: f64,
    pub properties: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, ToolSchema)]
pub struct ToolSearchArgs {
    #[tool(desc = "What capability you need (name, namespace, or task words)")]
    pub query: String,
    #[tool(desc = "Maximum tools to activate (default 5, max 20)")]
    pub limit: Option<usize>,
}

/// Search the deferred catalog and activate the best matches.
pub struct ToolSearchTool {
    agent: Weak<Agent>,
}

impl ToolSearchTool {
    pub fn new(agent: Weak<Agent>) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        BuiltinTool::ToolSearch.as_str()
    }

    fn aliases(&self) -> &'static [&'static str] {
        BuiltinTool::ToolSearch.aliases()
    }

    fn description(&self) -> &str {
        "Search tools that are not currently declared (for example a large MCP catalog) and activate the best matches for the next turn."
    }

    fn parameters_schema(&self) -> Value {
        ToolSearchArgs::parameters_schema()
    }

    fn risk_profile(&self) -> RiskProfile {
        RiskProfile::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolContext, arguments: Value) -> Result<ToolOutput, ToolError> {
        let args: ToolSearchArgs = serde_json::from_value(arguments)
            .map_err(|error| ToolError::invalid_args(self.name(), error.to_string()))?;
        let agent = self.agent.upgrade().ok_or_else(|| {
            ToolError::execution(self.name(), "the owning agent is no longer available")
        })?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err(ToolError::invalid_args(
                self.name(),
                "`query` must name a capability, namespace, or task; an empty search \
                 would activate arbitrary unrelated tools",
            ));
        }
        if query.chars().count() > 512 {
            return Err(ToolError::invalid_args(
                self.name(),
                "`query` is limited to 512 characters",
            ));
        }
        let limit = args.limit.unwrap_or(5).clamp(1, 20);
        let hits = agent.search_deferred_tools(query, limit);
        if hits.is_empty() {
            return Ok(ToolOutput::text(format!(
                "No deferred tool matched `{query}`. The direct toolset is already available; \
                 try a broader capability name."
            )));
        }
        let mut text = format!(
            "Activated {} tool(s) for the next turn. Call them directly by name:\n",
            hits.len()
        );
        for hit in &hits {
            let params = if hit.properties.is_empty() {
                String::new()
            } else {
                let shown = hit
                    .properties
                    .iter()
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({shown})")
            };
            text.push_str(&format!(
                "\n- `{}{}` — {}",
                hit.name, params, hit.description
            ));
        }
        Ok(ToolOutput::text(text))
    }
}

/// Rank deferred tools against `query` with BM25F-style field weighting.
/// Pure so it is testable without an agent.
///
/// BM25 supplies term-frequency saturation and document-length normalisation,
/// which the previous additive score lacked: a long description can no longer
/// outrank a tool whose *name* is the query, and repeating a word in one field
/// has diminishing returns. Names, aliases, schema properties, and descriptions
/// are weighted 5/3/1.5/1 before scoring; exact and phrase name matches keep a
/// small additive bonus so an exact identifier always wins over prose.
pub(crate) fn rank_deferred(
    entries: &[(String, String, Vec<String>, Vec<String>)],
    query: &str,
    limit: usize,
) -> Vec<DeferredHit> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    const NAME_BOOST: f64 = 5.0;
    const ALIAS_BOOST: f64 = 3.0;
    const PROPERTY_BOOST: f64 = 1.5;
    const DESCRIPTION_BOOST: f64 = 1.0;

    struct Document {
        name: String,
        description: String,
        properties: Vec<String>,
        aliases: Vec<String>,
        lower_name: String,
        term_frequency: HashMap<String, f64>,
        length: f64,
    }

    fn add_field(
        term_frequency: &mut HashMap<String, f64>,
        length: &mut f64,
        tokens: &[String],
        boost: f64,
    ) {
        for token in tokens {
            *term_frequency.entry(token.clone()).or_insert(0.0) += boost;
        }
        *length += tokens.len() as f64 * boost;
    }

    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut query_tokens = tokenize(&query);
    query_tokens.sort();
    query_tokens.dedup();

    let documents: Vec<Document> = entries
        .iter()
        .map(|(name, description, aliases, properties)| {
            let mut term_frequency = HashMap::new();
            let mut length = 0.0;
            add_field(
                &mut term_frequency,
                &mut length,
                &tokenize(&name.to_ascii_lowercase()),
                NAME_BOOST,
            );
            for alias in aliases {
                add_field(
                    &mut term_frequency,
                    &mut length,
                    &tokenize(&alias.to_ascii_lowercase()),
                    ALIAS_BOOST,
                );
            }
            for property in properties {
                add_field(
                    &mut term_frequency,
                    &mut length,
                    &tokenize(&property.to_ascii_lowercase()),
                    PROPERTY_BOOST,
                );
            }
            add_field(
                &mut term_frequency,
                &mut length,
                &tokenize(&description.to_ascii_lowercase()),
                DESCRIPTION_BOOST,
            );
            Document {
                name: name.clone(),
                description: description.clone(),
                properties: properties.clone(),
                aliases: aliases.clone(),
                lower_name: name.to_ascii_lowercase(),
                term_frequency,
                length,
            }
        })
        .collect();

    let document_count = documents.len() as f64;
    let average_length = if documents.is_empty() {
        0.0
    } else {
        documents.iter().map(|doc| doc.length).sum::<f64>() / document_count
    };

    // Document frequency is corpus-wide: compute it once per query token, not
    // once per (document, token) pair. A large MCP catalog must stay O(N·Q).
    let inverse_document_frequency: HashMap<&str, f64> = query_tokens
        .iter()
        .map(|token| {
            let document_frequency = documents
                .iter()
                .filter(|candidate| candidate.term_frequency.contains_key(token))
                .count() as f64;
            (
                token.as_str(),
                (1.0 + (document_count - document_frequency + 0.5) / (document_frequency + 0.5))
                    .ln(),
            )
        })
        .collect();

    let mut hits: Vec<DeferredHit> = documents
        .iter()
        .filter_map(|doc| {
            let mut score = 0.0;
            for token in &query_tokens {
                let term_frequency = doc.term_frequency.get(token).copied().unwrap_or(0.0);
                if term_frequency > 0.0 {
                    let inverse_document_frequency = inverse_document_frequency
                        .get(token.as_str())
                        .copied()
                        .unwrap_or(0.0);
                    let norm = K1 * (1.0 - B + B * doc.length / average_length.max(f64::EPSILON));
                    score += inverse_document_frequency * (term_frequency * (K1 + 1.0))
                        / (term_frequency + norm);
                }
            }
            if doc.lower_name == query {
                score += 50.0;
            } else if doc.lower_name.contains(&query) {
                score += 10.0;
            }
            if doc
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(&query))
            {
                score += 25.0;
            }
            (score > 0.0).then(|| DeferredHit {
                name: doc.name.clone(),
                description: doc.description.clone(),
                score,
                properties: doc.properties.clone(),
            })
        })
        .collect();
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.name.cmp(&right.name))
    });
    hits.truncate(limit);
    hits
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, description: &str) -> (String, String, Vec<String>, Vec<String>) {
        (
            name.to_string(),
            description.to_string(),
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn exact_name_wins_over_description_matches() {
        let entries = vec![
            entry("mcp__github__create_issue", "Create a GitHub issue"),
            entry("mcp__jira__issue_search", "Search issues"),
        ];
        let hits = rank_deferred(&entries, "create_issue", 5);
        assert_eq!(hits[0].name, "mcp__github__create_issue");
    }

    #[test]
    fn a_long_description_does_not_bury_a_name_match() {
        let entries = vec![
            entry("mcp__noise__generic", &"query ".repeat(500)),
            entry("mcp__db__query", "Run a read-only SQL statement"),
        ];
        let hits = rank_deferred(&entries, "query", 5);
        assert_eq!(hits[0].name, "mcp__db__query");
    }

    #[test]
    fn descriptions_keep_their_original_casing_in_the_result() {
        let entries = vec![entry("mcp__issues__create", "Create Issue")];
        let hits = rank_deferred(&entries, "issue", 5);
        assert_eq!(hits[0].description, "Create Issue");
    }

    #[test]
    fn descriptive_queries_still_rank() {
        let entries = vec![
            entry("mcp__db__query", "Run a read-only SQL query"),
            entry("mcp__fs__write", "Write a file"),
        ];
        let hits = rank_deferred(&entries, "sql query", 5);
        assert_eq!(hits[0].name, "mcp__db__query");
    }

    #[test]
    fn empty_query_matches_nothing() {
        let entries = vec![entry("b", "b"), entry("a", "a")];
        assert!(rank_deferred(&entries, "", 5).is_empty());
        assert!(rank_deferred(&entries, "   ", 5).is_empty());
    }
}
