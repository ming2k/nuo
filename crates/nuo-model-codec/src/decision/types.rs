//! System One typed decision primitives, questions, answers, and payload representations.
//!
//! Designed for non-generative, calibrated probability models such as TypeSafe Jev.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

/// Question asking for a binary / boolean probability judgment (Yes/No).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoulQuestion {
    /// Natural-language prompt guiding the boolean judgment.
    pub instructions: String,
}

impl NoulQuestion {
    pub fn new(instructions: impl Into<String>) -> Self {
        Self {
            instructions: instructions.into(),
        }
    }
}

/// Question asking for a categorical classification or routing decision among choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceQuestion {
    /// Prompt describing the selection or routing context.
    pub instructions: String,
    /// Named option identifiers mapped to their rubric criteria.
    pub criteria: BTreeMap<String, String>,
}

impl ChoiceQuestion {
    pub fn new(
        instructions: impl Into<String>,
        criteria: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        Self {
            instructions: instructions.into(),
            criteria: criteria
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }
}

/// Question asking for an ordinal rubric score across ordered levels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreQuestion {
    /// Prompt describing the scoring context.
    pub instructions: String,
    /// Ordered rubric descriptions from lowest to highest level.
    pub criteria: Vec<String>,
}

impl ScoreQuestion {
    pub fn new(
        instructions: impl Into<String>,
        criteria: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            instructions: instructions.into(),
            criteria: criteria.into_iter().map(Into::into).collect(),
        }
    }
}

/// Strongly typed question primitive for System One models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionQuestion {
    Noul(NoulQuestion),
    Choice(ChoiceQuestion),
    Score(ScoreQuestion),
}

impl DecisionQuestion {
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul(NoulQuestion::new(instructions))
    }

    pub fn choice(
        instructions: impl Into<String>,
        criteria: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        Self::Choice(ChoiceQuestion::new(instructions, criteria))
    }

    pub fn score(
        instructions: impl Into<String>,
        criteria: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::Score(ScoreQuestion::new(instructions, criteria))
    }
}

/// Answer to a Noul (boolean) question with calibrated probability in `[0.0, 1.0]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoulAnswer {
    /// The calibrated probability that the answer is "yes".
    /// Values near 1.0 indicate high confidence "yes", 0.0 indicates "no",
    /// while 0.5 indicates maximum model uncertainty.
    pub noul: f64,
}

/// Answer to a Choice question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceAnswer {
    /// The highest-probability chosen option key.
    pub choice: String,
    /// Probability distribution over all available candidate keys (sums to ~1.0).
    #[serde(default)]
    pub probabilities: BTreeMap<String, f64>,
    /// Confidence estimate of the decision in `[0.0, 1.0]`.
    pub confidence: f64,
}

impl ChoiceAnswer {
    /// Returns options sorted by probability in descending order.
    pub fn ranked(&self) -> Vec<(&str, f64)> {
        let mut items: Vec<(&str, f64)> = self
            .probabilities
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        items
    }

    /// Returns the margin between the top two probabilities (difference between rank 1 and rank 2).
    ///
    /// If there is only one candidate option, returns 1.0.
    pub fn margin(&self) -> f64 {
        let ranked = self.ranked();
        if ranked.len() < 2 {
            1.0
        } else {
            (ranked[0].1 - ranked[1].1).max(0.0)
        }
    }
}

/// Answer to a Score question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreAnswer {
    /// Probability-weighted expected score across the rubric levels.
    pub score: f64,
    /// Legend mapping level index strings to their rubric descriptions.
    #[serde(default)]
    pub legend: BTreeMap<String, String>,
    /// Probability distribution over each level index.
    #[serde(default)]
    pub probabilities: BTreeMap<String, f64>,
    /// Confidence estimate of the score in `[0.0, 1.0]`.
    pub confidence: f64,
}

impl ScoreAnswer {
    /// Returns the discrete level index with the highest probability.
    pub fn modal_level(&self) -> Option<usize> {
        self.probabilities
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .and_then(|(k, _)| k.parse::<usize>().ok())
    }

    /// Returns the probability assigned to a given level index.
    pub fn level_probability(&self, level: usize) -> Option<f64> {
        let key = level.to_string();
        self.probabilities.get(&key).copied()
    }
}

/// Typed decision answer.
#[derive(Debug, Clone, PartialEq)]
pub enum DecisionAnswer {
    Noul(NoulAnswer),
    Choice(ChoiceAnswer),
    Score(ScoreAnswer),
}

impl Serialize for DecisionAnswer {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum TaggedAnswer<'a> {
            Noul(&'a NoulAnswer),
            Choice(&'a ChoiceAnswer),
            Score(&'a ScoreAnswer),
        }

        match self {
            Self::Noul(n) => TaggedAnswer::Noul(n).serialize(serializer),
            Self::Choice(c) => TaggedAnswer::Choice(c).serialize(serializer),
            Self::Score(s) => TaggedAnswer::Score(s).serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for DecisionAnswer {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;

        // First attempt standard tagged deserialization
        if let Some(kind) = value.get("type").and_then(|v| v.as_str()) {
            match kind {
                "noul" => {
                    let ans: NoulAnswer = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                    return Ok(Self::Noul(ans));
                }
                "choice" => {
                    let ans: ChoiceAnswer = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                    return Ok(Self::Choice(ans));
                }
                "score" => {
                    let ans: ScoreAnswer = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                    return Ok(Self::Score(ans));
                }
                other => {
                    return Err(serde::de::Error::custom(format!("unrecognized answer type: {other}")));
                }
            }
        }

        // Resilient fallback by inspecting characteristic fields
        if value.get("noul").is_some() {
            let ans: NoulAnswer = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self::Noul(ans))
        } else if value.get("choice").is_some() {
            let ans: ChoiceAnswer = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self::Choice(ans))
        } else if value.get("score").is_some() {
            let ans: ScoreAnswer = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self::Score(ans))
        } else {
            Err(serde::de::Error::custom("missing type and unidentifiable answer payload"))
        }
    }
}

impl DecisionAnswer {
    pub fn as_noul(&self) -> Option<&NoulAnswer> {
        match self {
            Self::Noul(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_choice(&self) -> Option<&ChoiceAnswer> {
        match self {
            Self::Choice(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_score(&self) -> Option<&ScoreAnswer> {
        match self {
            Self::Score(s) => Some(s),
            _ => None,
        }
    }
}

/// Token usage statistics for a decision request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DecisionUsage {
    pub input_tokens: usize,
    pub output_tokens: usize,
}

/// Request payload sent to a System One decision endpoint (such as Jev).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRequest {
    /// Model name or alias (e.g. "jev-latest", "jev-1.13.0").
    pub model: String,
    /// Contextual state evaluated by the model (text string, JSON object, or array).
    pub state: serde_json::Value,
    /// Map of caller-chosen question identifiers to question specifications.
    pub questions: BTreeMap<String, DecisionQuestion>,
}

impl DecisionRequest {
    pub fn new(model: impl Into<String>, state: serde_json::Value) -> Self {
        Self {
            model: model.into(),
            state,
            questions: BTreeMap::new(),
        }
    }

    pub fn with_state_text(model: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(model, serde_json::Value::String(text.into()))
    }

    /// Creates a request by serializing an arbitrary structured state into JSON.
    pub fn with_state_value<T: serde::Serialize>(
        model: impl Into<String>,
        value: &T,
    ) -> std::result::Result<Self, serde_json::Error> {
        let state = serde_json::to_value(value)?;
        Ok(Self::new(model, state))
    }

    pub fn add_question(mut self, id: impl Into<String>, question: DecisionQuestion) -> Self {
        self.questions.insert(id.into(), question);
        self
    }

    pub fn add_noul(self, id: impl Into<String>, instructions: impl Into<String>) -> Self {
        self.add_question(id, DecisionQuestion::noul(instructions))
    }

    pub fn add_choice(
        self,
        id: impl Into<String>,
        instructions: impl Into<String>,
        criteria: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        self.add_question(id, DecisionQuestion::choice(instructions, criteria))
    }

    pub fn add_score(
        self,
        id: impl Into<String>,
        instructions: impl Into<String>,
        criteria: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.add_question(id, DecisionQuestion::score(instructions, criteria))
    }
}

/// Response returned by a System One decision endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    /// The resolved model name utilized by the server.
    pub model: String,
    /// Answer map keyed by the caller's question identifiers.
    pub answers: BTreeMap<String, DecisionAnswer>,
    /// Optional token consumption metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<DecisionUsage>,
}

impl DecisionResponse {
    /// Retrieves a generic answer by question identifier.
    pub fn answer(&self, id: &str) -> Option<&DecisionAnswer> {
        self.answers.get(id)
    }

    /// Retrieves a Noul answer by question identifier.
    pub fn noul(&self, id: &str) -> Option<&NoulAnswer> {
        self.answer(id).and_then(DecisionAnswer::as_noul)
    }

    /// Convenience helper returning the Noul probability directly.
    pub fn noul_prob(&self, id: &str) -> Option<f64> {
        self.noul(id).map(|a| a.noul)
    }

    /// Retrieves a Choice answer by question identifier.
    pub fn choice(&self, id: &str) -> Option<&ChoiceAnswer> {
        self.answer(id).and_then(DecisionAnswer::as_choice)
    }

    /// Convenience helper returning the selected choice key string directly.
    pub fn choice_value<'a>(&'a self, id: &str) -> Option<&'a str> {
        self.choice(id).map(|a| a.choice.as_str())
    }

    /// Retrieves a Score answer by question identifier.
    pub fn score(&self, id: &str) -> Option<&ScoreAnswer> {
        self.answer(id).and_then(DecisionAnswer::as_score)
    }

    /// Convenience helper returning the calculated expected score directly.
    pub fn score_val(&self, id: &str) -> Option<f64> {
        self.score(id).map(|a| a.score)
    }
}
