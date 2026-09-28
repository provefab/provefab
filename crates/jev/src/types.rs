use std::collections::HashMap;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::JevError;

/// Question ids to questions. Insertion order is kept on the wire.
pub type Questions = IndexMap<String, Question>;

/// A typed question. `Choice` options keep the order they were given in:
/// TypeSafe documents that option order can move the answer, so we never re-sort.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    Noul {
        instructions: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    Choice {
        instructions: String,
        criteria: IndexMap<String, String>,
    },
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub yes: String,
    #[serde(rename = "false")]
    pub no: String,
}

impl Question {
    pub fn noul(instructions: impl Into<String>) -> Self {
        Question::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    pub fn choice<K, V>(
        instructions: impl Into<String>,
        options: impl IntoIterator<Item = (K, V)>,
    ) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        Question::Choice {
            instructions: instructions.into(),
            criteria: options
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    pub fn score<L: Into<String>>(
        instructions: impl Into<String>,
        levels: impl IntoIterator<Item = L>,
    ) -> Self {
        Question::Score {
            instructions: instructions.into(),
            criteria: levels.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Request<'a> {
    pub model: &'a str,
    pub state: &'a serde_json::Value,
    pub questions: &'a Questions,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NoulAnswer {
    pub noul: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChoiceAnswer {
    pub choice: String,
    pub probabilities: HashMap<String, f64>,
    pub confidence: f64,
}

/// `score` is the probability-weighted level index (0-based) and can land between levels.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScoreAnswer {
    pub score: f64,
    pub probabilities: HashMap<String, f64>,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul(NoulAnswer),
    Choice(ChoiceAnswer),
    Score(ScoreAnswer),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Response {
    /// Exact model version that answered, e.g. `jev-1.13.0`. Logged for audit.
    pub model: String,
    pub answers: HashMap<String, Answer>,
    pub usage: Usage,
}

impl Response {
    pub fn noul(&self, id: &str) -> Result<f64, JevError> {
        match self.answers.get(id) {
            Some(Answer::Noul(a)) => Ok(a.noul),
            Some(_) => Err(wrong_type(id, "noul")),
            None => Err(JevError::MissingAnswer(id.to_string())),
        }
    }

    pub fn choice(&self, id: &str) -> Result<&ChoiceAnswer, JevError> {
        match self.answers.get(id) {
            Some(Answer::Choice(a)) => Ok(a),
            Some(_) => Err(wrong_type(id, "choice")),
            None => Err(JevError::MissingAnswer(id.to_string())),
        }
    }

    pub fn score(&self, id: &str) -> Result<&ScoreAnswer, JevError> {
        match self.answers.get(id) {
            Some(Answer::Score(a)) => Ok(a),
            Some(_) => Err(wrong_type(id, "score")),
            None => Err(JevError::MissingAnswer(id.to_string())),
        }
    }
}

fn wrong_type(id: &str, expected: &'static str) -> JevError {
    JevError::WrongType {
        id: id.to_string(),
        expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn noul_question_matches_documented_shape() {
        let q = Question::noul("Does this convey urgency?");
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({"type": "noul", "instructions": "Does this convey urgency?"})
        );
    }

    #[test]
    fn noul_criteria_serialize_as_true_false_keys() {
        let q = Question::Noul {
            instructions: "Urgent?".into(),
            criteria: Some(NoulCriteria {
                yes: "Explicitly time-sensitive".into(),
                no: "No urgency expressed".into(),
            }),
        };
        assert_eq!(
            serde_json::to_value(&q).unwrap()["criteria"],
            json!({"true": "Explicitly time-sensitive", "false": "No urgency expressed"})
        );
    }

    #[test]
    fn choice_options_keep_insertion_order_on_the_wire() {
        let q = Question::choice(
            "Kind?",
            [("zeta", "last letter"), ("alpha", "first letter")],
        );
        let wire = serde_json::to_string(&q).unwrap();
        assert!(
            wire.find("zeta").unwrap() < wire.find("alpha").unwrap(),
            "{wire}"
        );
    }

    #[test]
    fn score_levels_serialize_as_array() {
        let q = Question::score("How frustrated?", ["Calm", "Frustrated", "Very angry"]);
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({"type": "score", "instructions": "How frustrated?", "criteria": ["Calm", "Frustrated", "Very angry"]})
        );
    }

    fn documented_response() -> Response {
        serde_json::from_value(json!({
            "model": "jev-1.13.0",
            "answers": {
                "is_urgent": {"type": "noul", "noul": 0.95},
                "department": {"type": "choice", "choice": "billing",
                    "probabilities": {"billing": 0.88, "technical": 0.12, "sales": 0.0}, "confidence": 0.81},
                "frustration": {"type": "score", "score": 1.05,
                    "legend": {"0": "Calm", "1": "Frustrated", "2": "Very angry"},
                    "probabilities": {"0": 0.0, "1": 0.95, "2": 0.05}, "confidence": 0.92}
            },
            "usage": {"input_tokens": 318, "output_tokens": 34}
        }))
        .unwrap()
    }

    #[test]
    fn parses_all_three_documented_answer_types() {
        let r = documented_response();
        assert_eq!(r.noul("is_urgent").unwrap(), 0.95);
        assert_eq!(r.choice("department").unwrap().choice, "billing");
        assert_eq!(r.choice("department").unwrap().confidence, 0.81);
        assert_eq!(r.score("frustration").unwrap().score, 1.05);
        assert_eq!(r.usage.input_tokens, 318);
    }

    #[test]
    fn missing_and_mistyped_answers_are_errors_not_panics() {
        let r = documented_response();
        assert!(matches!(r.noul("nope"), Err(JevError::MissingAnswer(id)) if id == "nope"));
        assert!(matches!(
            r.score("is_urgent"),
            Err(JevError::WrongType {
                expected: "score",
                ..
            })
        ));
        assert!(matches!(
            r.choice("frustration"),
            Err(JevError::WrongType {
                expected: "choice",
                ..
            })
        ));
    }
}
