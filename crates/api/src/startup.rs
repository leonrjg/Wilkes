use serde::{Deserialize, Serialize};

/// One user-actionable reason the full application cannot start yet.
///
/// Breaking features contribute blockers during preflight. Keeping the shape
/// feature-neutral lets the desktop shell render one startup experience while
/// each feature owns its own explanation and remediation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupBlocker {
    pub id: String,
    pub feature: String,
    pub title: String,
    pub message: String,
    #[serde(default)]
    pub actions: Vec<StartupAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupAction {
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StartupStatus {
    pub blockers: Vec<StartupBlocker>,
}

impl StartupStatus {
    pub fn ready() -> Self {
        Self::default()
    }

    pub fn blocked(blockers: Vec<StartupBlocker>) -> Self {
        Self { blockers }
    }

    pub fn is_ready(&self) -> bool {
        self.blockers.is_empty()
    }

    pub fn extend(&mut self, blockers: impl IntoIterator<Item = StartupBlocker>) {
        self.blockers.extend(blockers);
    }

    pub fn unexpected(error: impl std::fmt::Display) -> Self {
        Self::blocked(vec![StartupBlocker {
            id: "application.startup-failed".to_string(),
            feature: "Application startup".to_string(),
            title: "Wilkes could not finish starting".to_string(),
            message: error.to_string(),
            actions: vec![StartupAction {
                label: "Try again".to_string(),
                description: "Restart Wilkes. If this keeps happening, check the application logs."
                    .to_string(),
                command: None,
            }],
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_status_aggregates_feature_blockers() {
        let mut status = StartupStatus::ready();
        assert!(status.is_ready());

        status.extend([StartupBlocker {
            id: "feature.breaking-change".to_string(),
            feature: "Feature".to_string(),
            title: "Update required".to_string(),
            message: "Data needs an explicit update.".to_string(),
            actions: Vec::new(),
        }]);

        assert!(!status.is_ready());
        assert_eq!(status.blockers[0].id, "feature.breaking-change");
    }
}
