//! The typed parameter prompt (pure state machine; no iced).
//!
//! One [`ParamSpec`] at a time, in declaration order. Each value starts at the spec's `default`
//! (a date with no default starts at today; a bool parses `"true"`). `required` blocks
//! [`Prompt::advance`] while the value is empty; a `Date` must be `YYYY-MM-DD`; an `Enum` value must
//! be one of `choices`. The finished map holds strings for text/multiline/path/enum/date and a
//! JSON bool for bool; empty optional values are omitted.

use chrono::NaiveDate;
use cv_protocol::{ActionInfo, ParamKind, ParamSpec};
use serde_json::{Map, Value};

/// Why the current step cannot advance.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PromptError {
    #[error("{0} is required")]
    Required(String),
    #[error("{0}: expected a date as YYYY-MM-DD")]
    BadDate(String),
    #[error("{0}: pick one of the listed choices")]
    BadChoice(String),
}

/// What [`Prompt::advance`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Moved to the next parameter.
    More,
    /// All parameters collected: the `params` map for `run_action`.
    Done(Map<String, Value>),
}

/// One parameter's current value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamValue {
    Text(String),
    Bool(bool),
}

/// The prompt over one action's params.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub action: ActionInfo,
    pub step: usize,
    pub values: Vec<ParamValue>,
}

fn initial(spec: &ParamSpec, today: NaiveDate) -> ParamValue {
    match spec.kind {
        ParamKind::Bool => ParamValue::Bool(
            spec.default
                .as_deref()
                .is_some_and(|d| d.trim().eq_ignore_ascii_case("true")),
        ),
        ParamKind::Date => ParamValue::Text(
            spec.default
                .clone()
                .unwrap_or_else(|| today.format("%Y-%m-%d").to_string()),
        ),
        ParamKind::Enum => ParamValue::Text(
            spec.default
                .clone()
                .or_else(|| {
                    if spec.required {
                        spec.choices.first().cloned()
                    } else {
                        None
                    }
                })
                .unwrap_or_default(),
        ),
        _ => ParamValue::Text(spec.default.clone().unwrap_or_default()),
    }
}

/// Whether `s` is a valid `YYYY-MM-DD` date.
pub fn valid_date(s: &str) -> bool {
    s.len() == 10 && NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

impl Prompt {
    /// A prompt over `action.params`, pre-filled.
    pub fn new(action: ActionInfo, today: NaiveDate) -> Self {
        let values = action.params.iter().map(|p| initial(p, today)).collect();
        Self {
            action,
            step: 0,
            values,
        }
    }

    /// The parameter being prompted.
    pub fn current(&self) -> Option<&ParamSpec> {
        self.action.params.get(self.step)
    }

    /// The current value.
    pub fn value(&self) -> Option<&ParamValue> {
        self.values.get(self.step)
    }

    /// The current value as text (`""` for a bool).
    pub fn text(&self) -> &str {
        match self.value() {
            Some(ParamValue::Text(s)) => s,
            _ => "",
        }
    }

    /// Sets the current text value (ignored on a bool step).
    pub fn set_text(&mut self, s: String) {
        if let Some(v @ ParamValue::Text(_)) = self.values.get_mut(self.step) {
            *v = ParamValue::Text(s);
        }
    }

    /// Sets the current bool value (ignored on a text step).
    pub fn set_bool(&mut self, b: bool) {
        if let Some(v @ ParamValue::Bool(_)) = self.values.get_mut(self.step) {
            *v = ParamValue::Bool(b);
        }
    }

    /// Validates the current step.
    pub fn validate(&self) -> Result<(), PromptError> {
        let (Some(spec), Some(v)) = (self.current(), self.value()) else {
            return Ok(());
        };
        let ParamValue::Text(s) = v else {
            return Ok(());
        };
        let s = s.trim();
        if s.is_empty() {
            return if spec.required {
                Err(PromptError::Required(spec.label.clone()))
            } else {
                Ok(())
            };
        }
        match spec.kind {
            ParamKind::Date if !valid_date(s) => Err(PromptError::BadDate(spec.label.clone())),
            ParamKind::Enum if !spec.choices.is_empty() && !spec.choices.iter().any(|c| c == s) => {
                Err(PromptError::BadChoice(spec.label.clone()))
            }
            _ => Ok(()),
        }
    }

    /// Validates and advances; on the last step returns the params map.
    pub fn advance(&mut self) -> Result<Step, PromptError> {
        self.validate()?;
        if self.step + 1 < self.action.params.len() {
            self.step += 1;
            return Ok(Step::More);
        }
        self.step = self.action.params.len();
        Ok(Step::Done(self.params()))
    }

    /// Back one step (no-op on the first).
    pub fn back(&mut self) {
        self.step = self.step.saturating_sub(1);
    }

    /// The params map built from every value so far.
    pub fn params(&self) -> Map<String, Value> {
        let mut m = Map::new();
        for (spec, v) in self.action.params.iter().zip(&self.values) {
            match v {
                ParamValue::Bool(b) => {
                    m.insert(spec.name.clone(), Value::Bool(*b));
                }
                ParamValue::Text(s) => {
                    let keep = if spec.kind == ParamKind::Multiline {
                        s.clone()
                    } else {
                        s.trim().to_owned()
                    };
                    if !keep.trim().is_empty() {
                        m.insert(spec.name.clone(), Value::String(keep));
                    }
                }
            }
        }
        m
    }
}
