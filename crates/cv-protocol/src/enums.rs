//! Closed string sets on the wire as typed enums (WP-R12, B3). Each serializes to exactly the
//! snake_case string the field carried when it was a `String`.

use serde::{Deserialize, Serialize};

/// Declares a unit enum with its wire strings, plus `ALL`, `as_str`, `parse` and `Display`.
macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vm])* #[serde(rename = $s)] $v),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$v),+];

            /// The wire string.
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$v => $s),+
                }
            }

            /// Parses a wire string.
            #[must_use]
            pub fn parse(s: &str) -> Option<Self> {
                match s {
                    $($s => Some(Self::$v),)+
                    _ => None,
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

wire_enum! {
    /// A job's queue lane (`JobSummary.lane`).
    Lane {
        /// Interactive work the user is waiting on.
        User = "user",
        /// Everything else.
        Background = "background",
    }
}

wire_enum! {
    /// What a tracker run did with an item (`IntakeItem.outcome`, `intake_list` filter).
    TrackerOutcome {
        /// A daily intake line was appended.
        Line = "line",
        /// A captured note was created.
        Note = "note",
        /// Filtered by `ignored_ids` / Ignore Shorts.
        Ignored = "ignored",
        /// Seen but not surfaced.
        Skipped = "skipped",
    }
}

wire_enum! {
    /// A service circuit breaker's state (`ServiceHealth.state`).
    BreakerState {
        /// Healthy.
        Closed = "closed",
        /// Not claiming.
        Open = "open",
        /// One probe may be claimed.
        HalfOpen = "half_open",
    }
}

wire_enum! {
    /// Why a job failed (`JobSummary.failure_kind`). Job-level, unlike the transport-level
    /// `cv_providers::FailureKind` (different variant set; no conversion between them).
    FailureKind {
        /// A dependency outage (eligible for requeue when the service recovers).
        Service = "service",
        /// The job failed on its own merits.
        Job = "job",
    }
}

wire_enum! {
    /// Context an action needs from the client (`ActionInfo.needs`).
    ContextNeed {
        /// `ActionContext.active_path`.
        ActivePath = "active_path",
        /// `ActionContext.selection`.
        Selection = "selection",
        /// `ActionContext.value`.
        Value = "value",
        /// `ActionContext.line`.
        Line = "line",
    }
}

/// `JobSummary.status` carries [`crate::JobState`] with the store's word `queued` for
/// [`crate::JobState::Pending`] (every other variant is its own snake_case name).
pub(crate) mod queue_status {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::JobState;

    pub(crate) fn as_str(s: JobState) -> &'static str {
        match s {
            JobState::Pending => "queued",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Deferred => "deferred",
            JobState::Cancelled => "cancelled",
        }
    }

    pub(crate) fn serialize<S: Serializer>(s: &JobState, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(as_str(*s))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<JobState, D::Error> {
        let s = String::deserialize(de)?;
        [
            JobState::Pending,
            JobState::Running,
            JobState::Done,
            JobState::Failed,
            JobState::Deferred,
            JobState::Cancelled,
        ]
        .into_iter()
        .find(|v| as_str(*v) == s)
        .ok_or_else(|| serde::de::Error::custom(format!("unknown job status {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JobState;
    use serde_json::json;

    fn pin<T>(cases: &[(T, &str)], all: &[T])
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug + Copy,
    {
        assert_eq!(cases.len(), all.len(), "every variant is pinned");
        for (v, s) in cases {
            assert_eq!(serde_json::to_value(v).unwrap(), json!(s));
            assert_eq!(serde_json::from_value::<T>(json!(s)).unwrap(), *v);
        }
    }

    #[test]
    fn lane_wire_strings() {
        pin(
            &[(Lane::User, "user"), (Lane::Background, "background")],
            Lane::ALL,
        );
    }

    #[test]
    fn tracker_outcome_wire_strings() {
        use TrackerOutcome as O;
        pin(
            &[
                (O::Line, "line"),
                (O::Note, "note"),
                (O::Ignored, "ignored"),
                (O::Skipped, "skipped"),
            ],
            O::ALL,
        );
        assert_eq!(O::parse("nope"), None);
    }

    #[test]
    fn breaker_state_wire_strings() {
        use BreakerState as B;
        pin(
            &[
                (B::Closed, "closed"),
                (B::Open, "open"),
                (B::HalfOpen, "half_open"),
            ],
            B::ALL,
        );
        assert_eq!(B::HalfOpen.to_string(), "half_open");
    }

    #[test]
    fn failure_kind_wire_strings() {
        pin(
            &[(FailureKind::Service, "service"), (FailureKind::Job, "job")],
            FailureKind::ALL,
        );
    }

    #[test]
    fn context_need_wire_strings() {
        use ContextNeed as C;
        pin(
            &[
                (C::ActivePath, "active_path"),
                (C::Selection, "selection"),
                (C::Value, "value"),
                (C::Line, "line"),
            ],
            C::ALL,
        );
    }

    /// `JobSummary.status` keeps the store vocabulary (`queued`, not `pending`).
    #[test]
    fn job_summary_status_wire_strings() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct W {
            #[serde(with = "super::queue_status")]
            s: JobState,
        }
        for (v, s) in [
            (JobState::Pending, "queued"),
            (JobState::Running, "running"),
            (JobState::Done, "done"),
            (JobState::Failed, "failed"),
            (JobState::Deferred, "deferred"),
            (JobState::Cancelled, "cancelled"),
        ] {
            assert_eq!(serde_json::to_value(W { s: v }).unwrap(), json!({ "s": s }));
            assert_eq!(
                serde_json::from_value::<W>(json!({ "s": s })).unwrap(),
                W { s: v }
            );
        }
        assert!(serde_json::from_value::<W>(json!({ "s": "pending" })).is_err());
    }
}
